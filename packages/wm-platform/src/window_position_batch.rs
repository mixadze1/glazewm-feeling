use windows::Win32::UI::WindowsAndMessaging::{
  SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOSENDCHANGING, SWP_NOZORDER,
};

use crate::{NativeWindow, NativeWindowWindowsExt, Rect, WindowZOrder};

/// Queue windows independently so a busy or destroyed window cannot
/// prevent its neighbors from processing the latest layout.
pub fn set_window_positions(
  windows: &[(NativeWindow, Rect)],
) -> crate::Result<()> {
  let flags = SWP_NOACTIVATE
    | SWP_NOZORDER
    | SWP_NOSENDCHANGING
    | SWP_ASYNCWINDOWPOS;
  for (window, rect) in windows {
    if let Err(error) =
      window.set_window_pos(&WindowZOrder::Normal, rect, flags)
    {
      tracing::debug!(%error, "Skipping unavailable layout window");
    }
  }
  Ok(())
}
