use anyhow::Context;
use wm_common::WindowState;

/// System utilities that must remain outside WM layout and focus
/// management.
#[cfg(target_os = "windows")]
pub fn is_unmanaged_system_window(
  window: &wm_platform::NativeWindow,
) -> bool {
  use wm_platform::NativeWindowWindowsExt;

  let Ok(process) = window.process_name() else {
    return false;
  };
  if is_unmanaged_system_process(&process) {
    return true;
  }
  process.eq_ignore_ascii_case("explorer")
    && window.class_name().is_ok_and(|class| class == "#32770")
    && window
      .title()
      .is_ok_and(|title| is_run_dialog_title(&title))
}

#[cfg(target_os = "windows")]
fn is_run_dialog_title(title: &str) -> bool {
  // Run is an Explorer-owned dialog, not a separate executable. Avoid
  // excluding Explorer itself or unrelated #32770 property/file dialogs.
  title.eq_ignore_ascii_case("Run") || title == "Выполнить"
}

#[cfg(target_os = "windows")]
fn is_unmanaged_system_process(name: &str) -> bool {
  // Exclude the editor as well as the capture/recording overlays before
  // any cloak, layout insertion, animation or focus correction occurs.
  [
    "Taskmgr",
    "regedit",
    "SnippingTool",
    "ScreenClippingHost",
    "ScreenSketch",
  ]
  .iter()
  .any(|process| name.eq_ignore_ascii_case(process))
}

use crate::{
  commands::container::{
    detach_container, flatten_child_split_containers,
  },
  models::WindowContainer,
  traits::{CommonGetters, WindowGetters},
  wm_state::WmState,
};

#[allow(clippy::needless_pass_by_value)]
pub fn ignore_window(
  window: WindowContainer,
  state: &mut WmState,
) -> anyhow::Result<()> {
  #[cfg(target_os = "windows")]
  {
    use wm_platform::NativeWindowWindowsExt;
    window.native().cancel_pending_position();
  }
  // Create iterator of parent, grandparent, and great-grandparent.
  let ancestors = window.ancestors().take(3).collect::<Vec<_>>();

  state.ignored_windows.push(window.native().clone());
  detach_container(window.clone().into())?;

  // After detaching the container, flatten any redundant split containers.
  // For example, in the layout V[1 H[2]] where container 1 is detached to
  // become V[H[2]], this will then need to be flattened to V[2].
  for ancestor in ancestors.iter().rev() {
    flatten_child_split_containers(ancestor)?;
  }

  // Sibling containers need to be redrawn if the window was tiling.
  if window.state() == WindowState::Tiling {
    let ancestor_to_redraw = ancestors
      .into_iter()
      .find(|ancestor| !ancestor.is_detached())
      .context("No ancestor to redraw.")?;

    state
      .pending_sync
      .queue_containers_to_redraw(ancestor_to_redraw.tiling_children());
  }

  Ok(())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
  use super::{is_run_dialog_title, is_unmanaged_system_process};

  #[test]
  fn excludes_capture_tools_without_excluding_shared_app_hosts() {
    for process in [
      "SnippingTool",
      "snippingtool",
      "ScreenClippingHost",
      "ScreenSketch",
      "TASKMGR",
      "RegEdit",
    ] {
      assert!(is_unmanaged_system_process(process), "{process}");
    }
    for process in
      ["ApplicationFrameHost", "explorer", "mspaint", "notepad"]
    {
      assert!(!is_unmanaged_system_process(process), "{process}");
    }
  }

  #[test]
  fn run_dialog_titles_do_not_match_other_explorer_dialogs() {
    assert!(is_run_dialog_title("Run"));
    assert!(is_run_dialog_title("Выполнить"));
    assert!(!is_run_dialog_title("Properties"));
    assert!(!is_run_dialog_title("Open"));
  }
}
