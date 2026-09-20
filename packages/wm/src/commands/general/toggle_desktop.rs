use wm_common::WindowState;
use wm_platform::{NativeWindow, NativeWindowWindowsExt};

use crate::{
  traits::{CommonGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

/// Hide without minimizing: minimizing would dismantle the tiling tree
/// and lose split proportions and the previous floating/fullscreen state.
pub fn toggle_desktop(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  if let Some(windows) = state.desktop_windows.take() {
    let mut failed = Vec::new();
    for window in windows {
      if window.is_valid() && window.set_cloaked(false).is_err() {
        failed.push(window);
      }
    }
    if !failed.is_empty() {
      state.desktop_windows = Some(failed);
      anyhow::bail!("Unable to restore some desktop windows; retry the toggle.");
    }
    state.pending_sync.suppress_animations();
    state
      .pending_sync
      .queue_container_to_redraw(state.root_container.clone())
      .queue_focus_change()
      .queue_all_effects_update();
    return Ok(());
  }

  state.animation_manager.finish_for_pause();
  super::platform_sync(state, config)?;
  let windows = state
    .windows()
    .into_iter()
    .filter(|window| {
      window.state() != WindowState::Minimized
        && window
          .workspace()
          .is_some_and(|workspace| workspace.is_displayed())
    })
    .collect::<Vec<_>>();
  if windows.is_empty() {
    return Ok(());
  }

  let mut hidden: Vec<NativeWindow> = Vec::new();
  for window in windows {
    let native = window.native().clone();
    if let Err(error) = native.set_cloaked(true) {
      for previous in &hidden {
        let _ = previous.set_cloaked(false);
      }
      return Err(error.into());
    }
    hidden.push(native);
  }
  state.desktop_windows = Some(hidden);
  state.focus_outline = None;
  state.resize_cursor_clip = None;
  state.dispatcher.reset_focus()?;
  Ok(())
}
