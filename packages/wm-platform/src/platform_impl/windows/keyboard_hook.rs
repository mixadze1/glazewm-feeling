use std::cell::Cell;

use windows::Win32::{
  Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
  UI::{
    Input::KeyboardAndMouse::{
      GetKeyState, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL,
      VK_RMENU, VK_RSHIFT, VK_RWIN,
    },
    WindowsAndMessaging::{
      CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK,
      KBDLLHOOKSTRUCT, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    },
  },
};

use crate::{Dispatcher, Key, KeyCode};

/// Callback stored in [`HOOK`] for intercepting keyboard events.
type HookCallback = Box<dyn Fn(KeyEvent) -> bool>;

thread_local! {
  /// Stores the hook callback for the current thread.
  ///
  /// The hook callback is called for every keyboard event and returns
  /// `true` if the event should be intercepted.
  static HOOK: Cell<Option<HookCallback>> = Cell::default();
}

/// A key event received from the keyboard hook.
#[derive(Clone, Debug)]
pub struct KeyEvent {
  /// The key that was pressed or released.
  pub key: Key,

  /// Key code that generated this event.
  #[allow(dead_code)]
  pub key_code: KeyCode,

  /// Whether the event is for a key press or release.
  pub is_keypress: bool,
}

impl KeyEvent {
  /// Gets whether the specified key is currently pressed.
  #[allow(clippy::unused_self)]
  pub fn is_key_down(&self, key: Key) -> bool {
    match key {
      Key::Cmd | Key::Win => {
        Self::is_key_down_raw(VK_LWIN.0)
          || Self::is_key_down_raw(VK_RWIN.0)
      }
      Key::Alt => {
        Self::is_key_down_raw(VK_LMENU.0)
          || Self::is_key_down_raw(VK_RMENU.0)
      }
      Key::Ctrl => {
        Self::is_key_down_raw(VK_LCONTROL.0)
          || Self::is_key_down_raw(VK_RCONTROL.0)
      }
      Key::Shift => {
        Self::is_key_down_raw(VK_LSHIFT.0)
          || Self::is_key_down_raw(VK_RSHIFT.0)
      }
      _ => {
        if let Ok(key_code) = KeyCode::try_from(key) {
          Self::is_key_down_raw(key_code.0)
        } else {
          false
        }
      }
    }
  }

  /// Gets whether the specified key is currently down using the raw key
  /// code.
  fn is_key_down_raw(key: u16) -> bool {
    unsafe { (GetKeyState(key.into()) & 0x80) == 0x80 }
  }
}

/// A system-wide low-level keyboard hook.
#[derive(Debug)]
pub struct KeyboardHook {
  handle: Option<HHOOK>,
  dispatcher: Dispatcher,
}

impl KeyboardHook {
  /// Creates an instance of `KeyboardHook`.
  ///
  /// The callback is called for every keyboard event and returns `true` if
  /// the event should be intercepted.
  ///
  /// # Panics
  ///
  /// Panics when attempting to register multiple hooks on the dispatcher's
  /// thread.
  pub fn new<F>(
    callback: F,
    dispatcher: &Dispatcher,
  ) -> crate::Result<Self>
  where
    F: Fn(KeyEvent) -> bool + Send + Sync + 'static,
  {
    let handle = dispatcher.dispatch_sync(move || {
      let occupied = HOOK.with(|state| {
        let previous = state.take();
        let occupied = previous.is_some();
        state.set(previous);
        occupied
      });
      if occupied {
        return Err(crate::Error::Platform(
          "Keyboard hook already registered.".to_string(),
        ));
      }
      let handle = unsafe {
        SetWindowsHookExW(
          WH_KEYBOARD_LL,
          Some(Self::hook_proc),
          HINSTANCE::default(),
          0,
        )
      }?;
      // Store the captured callback only after the native hook succeeds.
      HOOK.with(|state| state.set(Some(Box::new(callback))));
      Ok::<_, crate::Error>(handle)
    })??;

    Ok(Self {
      handle: Some(handle),
      dispatcher: dispatcher.clone(),
    })
  }

  /// Terminates the keyboard hook by unregistering it.
  pub fn terminate(&mut self) -> crate::Result<()> {
    let Some(handle) = self.handle else {
      return Ok(());
    };
    // Complete cleanup on the owning thread before permitting recreation.
    self.dispatcher.dispatch_sync(move || {
      unsafe { UnhookWindowsHookEx(handle) }?;
      HOOK.with(|state| {
        state.take();
      });
      Ok::<_, crate::Error>(())
    })??;
    self.handle = None;

    Ok(())
  }

  /// Hook procedure for keyboard events.
  ///
  /// For use with `SetWindowsHookExW`.
  extern "system" fn hook_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
  ) -> LRESULT {
    // If the code is less than zero, the hook procedure must pass the hook
    // notification directly to other applications.
    if code != 0 {
      return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    // Get struct with the keyboard input event.
    let input = unsafe { *(lparam.0 as *const KBDLLHOOKSTRUCT) };

    #[allow(clippy::cast_possible_truncation)]
    let key_code = KeyCode(input.vkCode as u16);
    #[allow(clippy::cast_possible_truncation)]
    let is_keypress =
      wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;

    let Ok(key) = Key::try_from(key_code) else {
      return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    };

    let key_event = KeyEvent {
      key,
      key_code,
      is_keypress,
    };

    let should_intercept = HOOK.with(|state| {
      if let Some(callback) = state.take() {
        let result = callback(key_event);
        state.set(Some(callback));
        result
      } else {
        false
      }
    });

    if should_intercept {
      return LRESULT(1);
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
  }
}

impl Drop for KeyboardHook {
  fn drop(&mut self) {
    let _ = self.terminate();
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::*;

  #[test]
  fn terminating_keyboard_hook_releases_capture_and_allows_recreation() {
    let (_event_loop, dispatcher) = crate::EventLoop::new().unwrap();
    for _ in 0..10 {
      let capture = Arc::new(());
      let weak = Arc::downgrade(&capture);
      let mut hook = KeyboardHook::new(
        move |_| {
          let _ = &capture;
          false
        },
        &dispatcher,
      )
      .unwrap();
      assert!(KeyboardHook::new(|_| false, &dispatcher).is_err());
      hook.terminate().unwrap();
      hook.terminate().unwrap();
      assert!(weak.upgrade().is_none());
    }
  }
}
