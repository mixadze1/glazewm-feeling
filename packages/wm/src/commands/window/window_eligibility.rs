//! Shared admission and focus policy for native auxiliary windows.
use wm_platform::{
  NativeWindow, NativeWindowWindowsExt, WS_CHILD, WS_EX_NOACTIVATE,
  WS_EX_TOOLWINDOW,
};

/// Re-evaluated on every admission/focus event, not cached by HWND. A
/// splash may become a main window, and Windows can recycle handles.
pub fn unmanaged_window_reason(
  window: &NativeWindow,
) -> Option<&'static str> {
  if super::is_unmanaged_system_window(window) {
    return Some("system utility");
  }
  let owned = window.has_owner_window();
  let child = window.has_window_style(WS_CHILD);
  let tool = window.has_window_style_ex(WS_EX_TOOLWINDOW);
  let no_activate = window.has_window_style_ex(WS_EX_NOACTIVATE);
  // Preserve the existing explicit Flow Launcher compatibility exception.
  let flow_launcher = (owned || child || tool || no_activate)
    && window
      .process_name()
      .is_ok_and(|name| name == "Flow.Launcher")
    && window.title().is_ok_and(|title| title == "Flow.Launcher");
  classify(owned, child, tool, no_activate, flow_launcher)
}

fn classify(
  owned: bool,
  child: bool,
  tool: bool,
  no_activate: bool,
  flow_launcher: bool,
) -> Option<&'static str> {
  if flow_launcher {
    return None;
  }
  if child {
    return Some("child window");
  }
  if owned {
    return Some("owned auxiliary window");
  }
  if tool {
    return Some("tool window");
  }
  if no_activate {
    return Some("non-activating window");
  }
  None
}

#[cfg(test)]
mod tests {
  use super::classify;

  #[test]
  fn owned_windows_are_auxiliary_even_when_resizable_or_app_windows() {
    // Unity Recorder has a caption and owner, but no TOOLWINDOW flag.
    assert_eq!(
      classify(true, false, false, false, false),
      Some("owned auxiliary window")
    );
  }

  #[test]
  fn independent_windows_in_the_same_process_remain_manageable() {
    // Title, process, resizability and APPWINDOW are deliberately not
    // classification inputs: none establishes an owner relationship.
    assert_eq!(classify(false, false, false, false, false), None);
  }

  #[test]
  fn temporary_auxiliary_can_become_a_main_window() {
    assert!(classify(true, false, false, false, false).is_some());
    assert!(classify(false, false, false, false, false).is_none());
  }

  #[test]
  fn native_exclusions_and_launcher_exception_are_preserved() {
    assert_eq!(
      classify(false, true, false, false, false),
      Some("child window")
    );
    assert_eq!(
      classify(false, false, true, false, false),
      Some("tool window")
    );
    assert_eq!(
      classify(false, false, false, true, false),
      Some("non-activating window")
    );
    assert_eq!(classify(true, false, true, false, true), None);
  }
}
