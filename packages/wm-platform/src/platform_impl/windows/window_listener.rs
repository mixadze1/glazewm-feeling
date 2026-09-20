use std::cell::RefCell;

use tokio::sync::mpsc;
use windows::Win32::{
  Foundation::HWND,
  UI::{
    Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
    WindowsAndMessaging::{
      EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
      EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE,
      EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
      EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART,
      EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART, OBJID_WINDOW,
      WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    },
  },
};

use super::NativeWindow;
use crate::{Dispatcher, WindowEvent, WindowId};

thread_local! {
  /// Sender for window events. For use with hook procedure.
  static EVENT_TX: RefCell<Option<mpsc::UnboundedSender<WindowEvent>>> = const { RefCell::new(None) };
}

/// Platform-specific implementation of [`WindowEventNotification`].
#[derive(Clone, Debug)]
pub struct WindowEventNotificationInner;

/// Platform-specific implementation of [`WindowListener`].
#[derive(Debug)]
pub(crate) struct WindowListener {
  hook_handles: Vec<HWINEVENTHOOK>,
  dispatcher: Dispatcher,
}

impl WindowListener {
  /// Implements [`WindowListener::new`].
  pub(crate) fn new(
    event_tx: mpsc::UnboundedSender<WindowEvent>,
    dispatcher: &Dispatcher,
  ) -> crate::Result<Self> {
    let hook_handles = dispatcher.dispatch_sync(move || {
      EVENT_TX.with(|sender| {
        if sender.borrow().is_some() {
          return Err(crate::Error::Platform(
            "Window event sender already set.".to_string(),
          ));
        }
        let handles = Self::hook_win_events()?;
        *sender.borrow_mut() = Some(event_tx);
        Ok(handles)
      })
    })??;

    Ok(Self {
      hook_handles,
      dispatcher: dispatcher.clone(),
    })
  }

  /// Implements [`WindowListener::terminate`].
  pub(crate) fn terminate(&mut self) {
    if self.hook_handles.is_empty() {
      return;
    }
    let handles = std::mem::take(&mut self.hook_handles);
    // WinEvent hooks must be removed on the thread that installed them.
    if let Err(err) = self.dispatcher.dispatch_sync(move || {
      for handle in handles {
        let _ = unsafe { UnhookWinEvent(handle) };
      }
      EVENT_TX.with(|sender| *sender.borrow_mut() = None);
    }) {
      tracing::warn!("Failed to terminate window listener: {err}");
    }
  }

  /// Creates several window event hooks via `SetWinEventHook`.
  ///
  /// Separate hooks are created per event range, which is more performant
  /// than a single hook covering all events.
  fn hook_win_events() -> crate::Result<Vec<HWINEVENTHOOK>> {
    let event_ranges = [
      (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
      (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
      (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
      (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
      (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE),
      (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    ];

    event_ranges
      .iter()
      .try_fold(Vec::new(), |mut handles, (min, max)| {
        // Create a window hook for the event range.
        let hook_handle = unsafe {
          SetWinEventHook(
            *min,
            *max,
            None,
            Some(Self::window_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
          )
        };

        if hook_handle.is_invalid() {
          for handle in handles {
            let _ = unsafe { UnhookWinEvent(handle) };
          }
          return Err(crate::Error::Platform(
            "Failed to set window event hook.".to_string(),
          ));
        }

        handles.push(hook_handle);
        Ok(handles)
      })
  }

  /// Callback passed to `SetWinEventHook`.
  ///
  /// This function is called on selected window events, and forwards them
  /// through an MPSC channel.
  extern "system" fn window_event_proc(
    _hook: HWINEVENTHOOK,
    event_type: u32,
    handle: HWND,
    id_object: i32,
    id_child: i32,
    _event_thread: u32,
    _event_time: u32,
  ) {
    // Check whether the event is associated with a window object rather
    // than a UI control.
    let is_window_event =
      id_object == OBJID_WINDOW.0 && id_child == 0 && handle != HWND(0);

    if !is_window_event {
      return;
    }

    let Some(event_tx) = EVENT_TX.with(|sender| sender.borrow().clone())
    else {
      return;
    };

    let notification = crate::WindowEventNotification(None);

    let event = match event_type {
      EVENT_OBJECT_DESTROY => WindowEvent::Destroyed {
        window_id: WindowId(handle.0),
        notification,
      },
      EVENT_SYSTEM_FOREGROUND => WindowEvent::Focused {
        window: NativeWindow::new(handle.0).into(),
        notification,
      },
      EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED => WindowEvent::Hidden {
        window: NativeWindow::new(handle.0).into(),
        notification,
      },
      EVENT_OBJECT_LOCATIONCHANGE => WindowEvent::MovedOrResized {
        window: NativeWindow::new(handle.0).into(),
        is_interactive_start: false,
        is_interactive_end: false,
        notification,
      },
      EVENT_SYSTEM_MINIMIZESTART => WindowEvent::Minimized {
        window: NativeWindow::new(handle.0).into(),
        notification,
      },
      EVENT_SYSTEM_MINIMIZEEND => WindowEvent::MinimizeEnded {
        window: NativeWindow::new(handle.0).into(),
        notification,
      },
      EVENT_SYSTEM_MOVESIZESTART => WindowEvent::MovedOrResized {
        window: NativeWindow::new(handle.0).into(),
        is_interactive_start: true,
        is_interactive_end: false,
        notification,
      },
      EVENT_SYSTEM_MOVESIZEEND => WindowEvent::MovedOrResized {
        window: NativeWindow::new(handle.0).into(),
        is_interactive_start: false,
        is_interactive_end: true,
        notification,
      },
      EVENT_OBJECT_SHOW | EVENT_OBJECT_UNCLOAKED => WindowEvent::Shown {
        window: NativeWindow::new(handle.0).into(),
        notification,
      },
      EVENT_OBJECT_NAMECHANGE => WindowEvent::TitleChanged {
        window: NativeWindow::new(handle.0).into(),
        notification,
      },
      _ => return,
    };

    if let Err(err) = event_tx.send(event) {
      tracing::warn!("Failed to send window event: {}.", err);
    }
  }
}

impl Drop for WindowListener {
  fn drop(&mut self) {
    self.terminate();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn listener_can_be_recreated_after_termination() {
    let (_event_loop, dispatcher) = crate::EventLoop::new().unwrap();
    for _ in 0..10 {
      let (tx, _rx) = mpsc::unbounded_channel();
      let mut listener = WindowListener::new(tx, &dispatcher).unwrap();
      listener.terminate();
      assert!(EVENT_TX.with(|sender| sender.borrow().is_none()));
      assert_eq!(listener.hook_handles.len(), 0);
    }
  }
}
