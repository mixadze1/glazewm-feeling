use tracing::info;
use wm_common::{try_warn, WindowState};
use wm_platform::NativeWindow;

use crate::{
  commands::window::update_window_state, traits::WindowGetters,
  user_config::UserConfig, wm_state::WmState,
};

pub fn handle_window_minimize_ended(
  native_window: &NativeWindow,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let found_window = state.window_from_native(native_window);

  // Update the window's state to not be minimized.
  if let Some(window) = found_window {
    let is_minimized = try_warn!(window.native().is_minimized());

    window.update_native_properties(|properties| {
      properties.is_minimized = is_minimized;
    });

    if !is_minimized && window.state() == WindowState::Minimized {
      info!("Window minimize ended: {window}");

      update_window_state(
        window.clone(),
        WindowState::Tiling,
        state,
        config,
      )?;
    }
  }

  Ok(())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
  use super::*;
  use crate::{
    commands::container::attach_container,
    models::{Monitor, NonTilingWindow, Workspace},
    traits::CommonGetters,
  };

  #[test]
  fn restored_floating_window_rejoins_tiling_and_duplicate_event_is_safe()
  {
    let (event_tx, _events) = tokio::sync::mpsc::unbounded_channel();
    let (exit_tx, _) = tokio::sync::mpsc::unbounded_channel();
    let (tick_tx, _) = tokio::sync::mpsc::unbounded_channel();
    let mut state = WmState::new(
      wm_platform::Dispatcher::mock(),
      event_tx,
      exit_tx,
      tick_tx,
    );
    let window =
      NonTilingWindow::mock().state(WindowState::Minimized).call();
    window.set_prev_state(WindowState::Floating(Default::default()));
    let native = window.native().clone();
    let id = window.id();
    let workspace =
      Workspace::mock().non_tiling_windows(vec![window]).call();
    let monitor =
      Monitor::mock().workspaces(vec![workspace.clone()]).call();
    attach_container(
      &monitor.into(),
      &state.root_container.clone().into(),
      None,
    )
    .unwrap();
    let config = UserConfig::new(Some(
      std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../resources/assets/sample-config.yaml"),
    ))
    .unwrap();
    for _ in 0..2 {
      handle_window_minimize_ended(&native, &mut state, &config).unwrap();
      let restored = state
        .container_by_id(id)
        .unwrap()
        .as_window_container()
        .unwrap();
      assert_eq!(restored.state(), WindowState::Tiling);
      assert_eq!(workspace.tiling_children().count(), 1);
    }
    assert!(state.pending_sync.has_changes());
  }
}
