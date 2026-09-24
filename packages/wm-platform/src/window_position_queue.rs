//! Foreign windows must never make the WM wait for their message pump.
use std::{
  collections::HashMap,
  sync::{Arc, Condvar, Mutex, OnceLock},
  time::{Duration, Instant},
};

use windows::Win32::{
  Foundation::{HWND, LPARAM, RECT, WPARAM},
  UI::WindowsAndMessaging::{
    GetWindowRect, GetWindowThreadProcessId, SendMessageTimeoutW,
    SetWindowPos, SET_WINDOW_POS_FLAGS, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, WM_NULL,
  },
};

#[derive(Clone)]
struct Position {
  thread: u32,
  process: u32,
  after: isize,
  x: i32,
  y: i32,
  width: i32,
  height: i32,
  flags: SET_WINDOW_POS_FLAGS,
  sent: Option<Instant>,
  retry_once: bool,
}

impl Position {
  /// A z-order-only update must not discard a pending resize (or vice
  /// versa).
  fn replace_with(&mut self, mut next: Self) {
    if (self.thread, self.process) == (next.thread, next.process) {
      if next.flags.contains(SWP_NOMOVE) {
        next.x = self.x;
        next.y = self.y;
        next.flags &= !SWP_NOMOVE | (self.flags & SWP_NOMOVE);
      }
      if next.flags.contains(SWP_NOSIZE) {
        next.width = self.width;
        next.height = self.height;
        next.flags &= !SWP_NOSIZE | (self.flags & SWP_NOSIZE);
      }
      if next.flags.contains(SWP_NOZORDER) {
        next.after = self.after;
        next.flags &= !SWP_NOZORDER | (self.flags & SWP_NOZORDER);
      }
      next.flags |= self.flags & SWP_FRAMECHANGED;
      next.retry_once |= self.retry_once;
    }
    *self = next;
  }

  fn reached(&self, rect: &RECT) -> bool {
    (self.flags.contains(SWP_NOMOVE)
      || (rect.left, rect.top) == (self.x, self.y))
      && (self.flags.contains(SWP_NOSIZE)
        || (rect.right - rect.left, rect.bottom - rect.top)
          == (self.width, self.height))
  }
}

#[derive(Default)]
struct Queue {
  pending: Mutex<HashMap<isize, Position>>,
  wake: Condvar,
}

static QUEUE: OnceLock<Arc<Queue>> = OnceLock::new();

/// Success means accepted, not that the application applied the geometry.
pub(crate) fn enqueue(
  hwnd: HWND,
  after: HWND,
  x: i32,
  y: i32,
  width: i32,
  height: i32,
  flags: SET_WINDOW_POS_FLAGS,
) -> crate::Result<()> {
  let mut process = 0;
  let thread =
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut process)) };
  if thread == 0 {
    return Err(windows::core::Error::from_win32().into());
  }
  let queue = QUEUE.get_or_init(|| {
    let queue = Arc::new(Queue::default());
    let worker = queue.clone();
    std::thread::Builder::new()
      .name("window-position".into())
      .spawn(move || run(&worker))
      .expect("Cannot start window positioning worker");
    queue
  });
  let next = Position {
    thread,
    process,
    after: after.0,
    x,
    y,
    width,
    height,
    flags: flags | SWP_ASYNCWINDOWPOS,
    sent: None,
    retry_once: false,
  };
  let mut pending = queue.pending.lock().unwrap();
  pending
    .entry(hwnd.0)
    .and_modify(|old| old.replace_with(next.clone()))
    .or_insert(next);
  queue.wake.notify_one();
  Ok(())
}

/// Invalidate queued geometry before a native maximize/minimize or
/// unmanage.
pub(crate) fn cancel(hwnd: HWND) {
  if let Some(queue) = QUEUE.get() {
    queue.pending.lock().unwrap().remove(&hwnd.0);
  }
}

/// DPI transitions sometimes need a second positioning after the first
/// request was processed. Two immediate enqueues would be coalesced.
pub(crate) fn retry_after_apply(hwnd: HWND) {
  if let Some(queue) = QUEUE.get() {
    if let Some(position) = queue.pending.lock().unwrap().get_mut(&hwnd.0)
    {
      position.retry_once = true;
    }
  }
}

fn run(queue: &Queue) {
  loop {
    let handles = {
      let mut pending = queue.pending.lock().unwrap();
      while pending.is_empty() {
        pending = queue.wake.wait(pending).unwrap();
      }
      pending.keys().copied().collect::<Vec<_>>()
    };
    for handle in handles {
      // Only the worker probes responsiveness. Never hold the queue lock
      // while waiting, even for this bounded 1 ms foreign-thread query.
      let responsive = unsafe {
        SendMessageTimeoutW(
          HWND(handle),
          WM_NULL,
          WPARAM(0),
          LPARAM(0),
          SMTO_ABORTIFHUNG | SMTO_BLOCK,
          1,
          None,
        )
        .0 != 0
      };
      let mut pending = queue.pending.lock().unwrap();
      let Some(position) = pending.get_mut(&handle) else {
        continue;
      };
      let mut process = 0;
      let thread = unsafe {
        GetWindowThreadProcessId(HWND(handle), Some(&raw mut process))
      };
      if (thread, process) != (position.thread, position.process) {
        pending.remove(&handle);
        continue;
      }
      if !responsive {
        continue;
      }
      if let Some(sent) = position.sent {
        let mut actual = RECT::default();
        let measured =
          unsafe { GetWindowRect(HWND(handle), &raw mut actual) }.is_ok();
        let reached = measured && position.reached(&actual);
        if reached || sent.elapsed() >= Duration::from_millis(250) {
          // A responsive app may constrain its size. Do not fight its
          // minimum size indefinitely or report the requested rect as
          // real.
          if position.retry_once {
            position.retry_once = false;
            position.sent = None;
          } else {
            if !reached {
              tracing::debug!(
                handle,
                "Window constrained async positioning"
              );
            }
            pending.remove(&handle);
          }
        }
        continue;
      }
      // This worker owns no windows and never attaches input queues.
      // ASYNCWINDOWPOS therefore posts to the application's thread.
      let result = unsafe {
        SetWindowPos(
          HWND(handle),
          HWND(position.after),
          position.x,
          position.y,
          position.width,
          position.height,
          position.flags,
        )
      };
      if let Err(error) = result {
        tracing::debug!(handle, %error, "Async window positioning failed");
        pending.remove(&handle);
      } else {
        position.sent = Some(Instant::now());
      }
    }
    std::thread::sleep(Duration::from_millis(16));
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn position(x: i32, flags: SET_WINDOW_POS_FLAGS) -> Position {
    Position {
      thread: 1,
      process: 2,
      after: 0,
      x,
      y: 20,
      width: 400,
      height: 300,
      flags,
      sent: None,
      retry_once: false,
    }
  }

  #[test]
  fn latest_target_replaces_busy_window_backlog() {
    let mut pending = position(0, SWP_NOZORDER);
    for x in 1..1000 {
      pending.replace_with(position(x, SWP_NOZORDER));
    }
    assert_eq!(pending.x, 999);
    assert!(pending.sent.is_none());
  }

  #[test]
  fn z_order_update_preserves_pending_geometry() {
    let mut pending = position(123, SWP_NOZORDER);
    let mut next = position(0, SWP_NOMOVE | SWP_NOSIZE);
    next.after = -1;
    pending.replace_with(next);
    assert_eq!(pending.x, 123);
    assert_eq!(pending.after, -1);
    assert!(!pending.flags.contains(SWP_NOMOVE | SWP_NOSIZE));
  }

  #[test]
  fn success_requires_observed_geometry() {
    let pending = position(10, SWP_NOZORDER);
    assert!(!pending.reached(&RECT::default()));
    assert!(pending.reached(&RECT {
      left: 10,
      top: 20,
      right: 410,
      bottom: 320
    }));
  }

  #[test]
  fn dpi_retry_survives_coalescing() {
    let mut pending = position(10, SWP_NOZORDER);
    pending.retry_once = true;
    pending.replace_with(position(20, SWP_NOZORDER));
    assert!(pending.retry_once);
    assert_eq!(pending.x, 20);
  }

  #[test]
  fn reused_handle_does_not_inherit_old_geometry() {
    let mut pending = position(123, SWP_NOZORDER);
    let mut next = position(0, SWP_NOMOVE | SWP_NOSIZE);
    next.process = 3;
    pending.replace_with(next);
    assert_eq!(pending.x, 0);
    assert!(pending.flags.contains(SWP_NOMOVE | SWP_NOSIZE));
  }
}
