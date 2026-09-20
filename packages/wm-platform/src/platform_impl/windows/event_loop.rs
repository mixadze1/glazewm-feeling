use std::{
  cell::RefCell,
  collections::HashMap,
  sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
  },
  thread::{self, ThreadId},
};

use windows::{
  core::w,
  Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::WindowsAndMessaging::{
      CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
      GetMessageW, PostMessageW, PostThreadMessageW, RegisterClassW,
      RegisterWindowMessageW, SendMessageW, TranslateMessage, CS_HREDRAW,
      CS_VREDRAW, CW_USEDEFAULT, MSG, WINDOW_EX_STYLE, WM_QUIT, WNDCLASSW,
      WNDPROC, WS_OVERLAPPEDWINDOW,
    },
  },
};

use crate::{DispatchFn, Dispatcher, WndProcCallback};

type PendingDispatches =
  Arc<Mutex<Option<HashMap<usize, Box<DispatchFn>>>>>;

thread_local! {
  /// Custom message ID for dispatching closures to be run on the event
  /// loop thread.
  ///
  /// Async messages carry a queue ID. Sync messages (LPARAM = 1) borrow
  /// an Option<Box<dyn FnOnce() + Send>> for the duration of SendMessageW.
  ///
  /// This message is sent using `PostMessageW` and handled in
  /// [`EventLoop::window_proc`].
  static WM_DISPATCH_CALLBACK: u32 = unsafe { RegisterWindowMessageW(w!("GlazeWM:Dispatch")) };
  static PENDING_DISPATCHES: RefCell<HashMap<isize, PendingDispatches>> = RefCell::new(HashMap::new());

  /// Registered callbacks that pre-process messages in the event loop's
  /// window procedure.
  ///
  /// Keyed by a unique callback ID for later deregistration.
  static WNDPROC_CALLBACKS: RefCell<HashMap<usize, Box<WndProcCallback>>> =
    RefCell::new(HashMap::new());
}

/// Source for dispatching callbacks onto the event loop thread.
#[derive(Clone)]
pub(crate) struct EventLoopSource {
  pub(crate) message_window_handle: isize,
  pub(crate) thread_id: ThreadId,
  os_thread_id: u32,
  next_callback_id: Arc<AtomicUsize>,
  pending_dispatches: PendingDispatches,
}

impl EventLoopSource {
  pub(crate) fn send_dispatch_async<F>(
    &self,
    dispatch_fn: F,
  ) -> crate::Result<()>
  where
    F: FnOnce() + Send + 'static,
  {
    let id = self.next_callback_id.fetch_add(1, Ordering::Relaxed);
    {
      let mut pending = self.pending_dispatches.lock().unwrap();
      pending
        .as_mut()
        .ok_or(crate::Error::EventLoopStopped)?
        .insert(id, Box::new(dispatch_fn));
    }

    unsafe {
      if PostMessageW(
        HWND(self.message_window_handle),
        WM_DISPATCH_CALLBACK.with(|v| *v),
        WPARAM(id),
        LPARAM(0),
      )
      .is_ok()
      {
        Ok(())
      } else {
        let callback = self
          .pending_dispatches
          .lock()
          .unwrap()
          .as_mut()
          .and_then(|pending| pending.remove(&id));
        drop(callback);
        Err(crate::Error::WindowMessage(
          "Failed to post message".to_string(),
        ))
      }
    }
  }

  pub(crate) fn send_dispatch_sync<F>(
    &self,
    dispatch_fn: F,
  ) -> crate::Result<()>
  where
    F: FnOnce() + Send,
  {
    let mut dispatch_fn: Option<Box<dyn FnOnce() + Send + '_>> =
      Some(Box::new(dispatch_fn));

    // `SendMessageW` blocks the calling thread until the window procedure
    // processes the message and executes the closure. This guarantees the
    // closure's lifetime remains valid.
    unsafe {
      SendMessageW(
        HWND(self.message_window_handle),
        WM_DISPATCH_CALLBACK.with(|v| *v),
        WPARAM(std::ptr::from_mut(&mut dispatch_fn) as usize),
        LPARAM(1),
      );
    }

    if dispatch_fn.is_some() {
      Err(crate::Error::EventLoopStopped)
    } else {
      Ok(())
    }
  }

  pub(crate) fn send_stop(&self) -> crate::Result<()> {
    unsafe {
      PostThreadMessageW(self.os_thread_id, WM_QUIT, WPARAM(0), LPARAM(0))
    }
    .map_err(|_| {
      crate::Error::WindowMessage(
        "Failed to post quit message".to_string(),
      )
    })
  }

  pub(crate) fn register_wndproc_callback(
    &self,
    callback: Box<WndProcCallback>,
  ) -> crate::Result<usize> {
    let id = self.next_callback_id.fetch_add(1, Ordering::Relaxed);

    // The callback is installed asynchronously on the event loop thread.
    self.send_dispatch_async(move || {
      WNDPROC_CALLBACKS.with(|cbs| {
        cbs.borrow_mut().insert(id, callback);
      });
    })?;

    Ok(id)
  }

  pub(crate) fn deregister_wndproc_callback(
    &self,
    id: usize,
  ) -> crate::Result<()> {
    self.send_dispatch_async(move || {
      WNDPROC_CALLBACKS.with(|cbs| {
        cbs.borrow_mut().remove(&id);
      });
    })
  }
}

/// Platform-specific implementation of [`EventLoop`].
pub(crate) struct EventLoop {
  source: EventLoopSource,
  stopped: Arc<AtomicBool>,
}

impl EventLoop {
  /// Implements [`EventLoop::new`].
  pub(crate) fn new() -> crate::Result<(Self, Dispatcher)> {
    // Create a hidden message window on the current thread.
    let window_handle =
      Self::create_message_window(Some(Self::window_proc))?;

    let source = EventLoopSource {
      message_window_handle: window_handle,
      thread_id: thread::current().id(),
      os_thread_id: unsafe { GetCurrentThreadId() },
      next_callback_id: Arc::new(AtomicUsize::new(0)),
      pending_dispatches: Arc::new(Mutex::new(Some(HashMap::new()))),
    };
    PENDING_DISPATCHES.with(|queues| {
      queues
        .borrow_mut()
        .insert(window_handle, source.pending_dispatches.clone());
    });

    let stopped = Arc::new(AtomicBool::new(false));
    let dispatcher =
      Dispatcher::new(Some(source.clone()), stopped.clone());

    Ok((Self { source, stopped }, dispatcher))
  }

  /// Implements [`EventLoop::run`].
  pub(crate) fn run(&self) -> crate::Result<()> {
    debug_assert_eq!(thread::current().id(), self.source.thread_id);
    tracing::info!("Starting event loop.");
    let mut msg = MSG::default();

    // Start the message loop. Blocks until `WM_QUIT` is received.
    loop {
      let result = unsafe { GetMessageW(&raw mut msg, None, 0, 0) }.0;
      if result == -1 {
        return Err(crate::Error::Platform(
          "GetMessageW failed.".to_string(),
        ));
      }
      if result != 0 {
        unsafe {
          TranslateMessage(&raw const msg);
          DispatchMessageW(&raw const msg);
        }
      } else {
        break;
      }
    }

    tracing::info!("Event loop thread exiting.");
    Ok(())
  }

  /// Creates a hidden message window.
  ///
  /// Returns a handle to the created window.
  fn create_message_window(
    window_procedure: WNDPROC,
  ) -> crate::Result<isize> {
    let wnd_class = WNDCLASSW {
      lpszClassName: w!("MessageWindow"),
      style: CS_HREDRAW | CS_VREDRAW,
      lpfnWndProc: window_procedure,
      ..Default::default()
    };

    unsafe { RegisterClassW(&raw const wnd_class) };

    let handle = unsafe {
      CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("MessageWindow"),
        w!("MessageWindow"),
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        None,
        None,
        wnd_class.hInstance,
        None,
      )
    };

    if handle.0 == 0 {
      return Err(crate::Error::Platform(
        "Creation of message window failed.".to_string(),
      ));
    }

    Ok(handle.0)
  }

  /// Window procedure for handling messages.
  unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
  ) -> LRESULT {
    // Handle dispatch callbacks first.
    if msg == WM_DISPATCH_CALLBACK.with(|v| *v) {
      if lparam.0 == 1 {
        // The sending thread owns this slot and is blocked until return.
        let callback =
          &mut *(wparam.0 as *mut Option<Box<dyn FnOnce() + Send>>);
        if let Some(callback) = callback.take() {
          callback();
        }
      } else {
        let callback = PENDING_DISPATCHES.with(|queues| {
          queues.borrow().get(&hwnd.0).and_then(|queue| {
            queue
              .lock()
              .unwrap()
              .as_mut()
              .and_then(|pending| pending.remove(&wparam.0))
          })
        });
        if let Some(callback) = callback {
          callback();
        }
      }
      return LRESULT(0);
    }

    // Let registered callbacks pre-process the message.
    let handled = WNDPROC_CALLBACKS.with(|cbs| {
      for callback in cbs.borrow().values() {
        if let Some(result) = callback(hwnd.0, msg, wparam.0, lparam.0) {
          return Some(LRESULT(result));
        }
      }
      None
    });

    if let Some(result) = handled {
      return result;
    }

    // `WM_QUIT` is handled by the message loop and should be forwarded
    // along with other messages.
    DefWindowProcW(hwnd, msg, wparam, lparam)
  }
}

impl Drop for EventLoop {
  fn drop(&mut self) {
    self.stopped.store(true, Ordering::SeqCst);
    // Release queued closures even if run() was never called. Taking the
    // map closes the queue and breaks closures capturing their dispatcher.
    let pending = self.source.pending_dispatches.lock().unwrap().take();
    drop(pending);
    PENDING_DISPATCHES.with(|queues| {
      queues
        .borrow_mut()
        .remove(&self.source.message_window_handle);
    });
    WNDPROC_CALLBACKS.with(|callbacks| callbacks.borrow_mut().clear());
    if let Err(err) =
      unsafe { DestroyWindow(HWND(self.source.message_window_handle)) }
    {
      tracing::warn!("Failed to destroy event loop window: {err}");
    }
  }
}

#[cfg(test)]
mod tests {
  use windows::Win32::UI::WindowsAndMessaging::IsWindow;

  use super::*;

  #[test]
  fn dropping_unstarted_loop_releases_queued_closures_and_window() {
    let (event_loop, _) = EventLoop::new().unwrap();
    let source = event_loop.source.clone();
    let capture = Arc::new(());
    let weak = Arc::downgrade(&capture);
    let captured_source = source.clone();
    source
      .send_dispatch_async(move || drop((capture, captured_source)))
      .unwrap();
    assert!(weak.upgrade().is_some());
    drop(event_loop);
    assert!(weak.upgrade().is_none());
    assert!(
      !unsafe { IsWindow(HWND(source.message_window_handle)) }.as_bool()
    );
    assert!(source.send_dispatch_async(|| {}).is_err());
    assert!(source.send_dispatch_sync(|| {}).is_err());
  }
}
