use std::{
  cell::{Ref, RefCell, RefMut},
  collections::VecDeque,
  rc::Rc,
};

use anyhow::Context;
use uuid::Uuid;
use wm_common::{
  ActiveDrag, ContainerDto, DisplayState, GapsConfig, WindowDto,
  WindowRuleConfig, WindowState,
};
use wm_platform::{NativeWindow, Rect, RectDelta};

use crate::{
  impl_common_getters, impl_container_debug, impl_window_getters,
  models::{
    Container, DirectionContainer, InsertionTarget,
    NativeWindowProperties, TilingContainer, TilingWindow,
    WindowContainer,
  },
  traits::{CommonGetters, PositionGetters, WindowGetters},
};

#[derive(Clone)]
pub struct NonTilingWindow(Rc<RefCell<NonTilingWindowInner>>);

struct NonTilingWindowInner {
  id: Uuid,
  parent: Option<Container>,
  children: VecDeque<Container>,
  child_focus_order: VecDeque<Uuid>,
  native: NativeWindow,
  native_properties: NativeWindowProperties,
  state: WindowState,
  prev_state: Option<WindowState>,
  insertion_target: Option<InsertionTarget>,
  display_state: DisplayState,
  border_delta: RectDelta,
  has_pending_dpi_adjustment: bool,
  floating_placement: Rect,
  has_custom_floating_placement: bool,
  done_window_rules: Vec<WindowRuleConfig>,
  active_drag: Option<ActiveDrag>,
}

impl NonTilingWindow {
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    id: Option<Uuid>,
    native: NativeWindow,
    properties: NativeWindowProperties,
    state: WindowState,
    prev_state: Option<WindowState>,
    border_delta: RectDelta,
    insertion_target: Option<InsertionTarget>,
    floating_placement: Rect,
    has_custom_floating_placement: bool,
    done_window_rules: Vec<WindowRuleConfig>,
    active_drag: Option<ActiveDrag>,
  ) -> Self {
    let window = NonTilingWindowInner {
      id: id.unwrap_or_else(Uuid::new_v4),
      parent: None,
      children: VecDeque::new(),
      child_focus_order: VecDeque::new(),
      native,
      native_properties: properties,
      state,
      prev_state,
      insertion_target,
      display_state: DisplayState::Shown,
      border_delta,
      has_pending_dpi_adjustment: false,
      floating_placement,
      has_custom_floating_placement,
      done_window_rules,
      active_drag,
    };

    Self(Rc::new(RefCell::new(window)))
  }

  pub fn insertion_target(&self) -> Option<InsertionTarget> {
    self.0.borrow().insertion_target.clone()
  }

  pub fn set_insertion_target(
    &self,
    insertion_target: Option<InsertionTarget>,
  ) {
    self.0.borrow_mut().insertion_target = insertion_target;
  }

  pub fn to_tiling(&self, gaps_config: GapsConfig) -> TilingWindow {
    let prev_state = if self.active_drag().is_some() {
      self.prev_state()
    } else {
      Some(self.state())
    };

    TilingWindow::new(
      Some(self.id()),
      self.native().clone(),
      self.native_properties().clone(),
      prev_state,
      self.border_delta(),
      self.floating_placement(),
      self.has_custom_floating_placement(),
      gaps_config,
      self.done_window_rules(),
      self.active_drag(),
    )
  }

  pub fn to_dto(&self) -> anyhow::Result<ContainerDto> {
    let rect = self.to_rect()?;

    Ok(ContainerDto::Window(WindowDto {
      id: self.id(),
      parent_id: self.parent().map(|parent| parent.id()),
      has_focus: self.has_focus(None),
      tiling_size: None,
      width: rect.width(),
      height: rect.height(),
      x: rect.x(),
      y: rect.y(),
      state: self.state(),
      prev_state: self.prev_state(),
      display_state: self.display_state(),
      border_delta: self.border_delta(),
      floating_placement: self.floating_placement(),
      #[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
      handle: self.native().id().0 as isize,
      title: self.native_properties().title,
      #[cfg(target_os = "windows")]
      class_name: self.native_properties().class_name,
      process_name: self.native_properties().process_name,
      active_drag: self.active_drag(),
    }))
  }
}

impl_container_debug!(NonTilingWindow);
impl_common_getters!(NonTilingWindow);
impl_window_getters!(NonTilingWindow);

impl PositionGetters for NonTilingWindow {
  fn to_rect(&self) -> anyhow::Result<Rect> {
    match self.state() {
      WindowState::Fullscreen(_fullscreen) => {
        let monitor = self.monitor().context("No monitor.")?;

        #[cfg(target_os = "windows")]
        {
          if _fullscreen.maximized {
            Ok(monitor.native_properties().working_area)
          } else {
            monitor.to_rect()
          }
        }
        #[cfg(target_os = "macos")]
        {
          // On macOS, the public APIs only allow window placement within
          // the display's working area.
          Ok(monitor.native_properties().working_area)
        }
      }
      _ => Ok(self.floating_placement()),
    }
  }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
  use wm_common::FullscreenStateConfig;

  use super::*;
  use crate::models::Monitor;

  #[test]
  fn maximized_window_uses_work_area_and_fullscreen_uses_monitor() {
    let window = NonTilingWindow::mock()
      .state(WindowState::Fullscreen(FullscreenStateConfig {
        maximized: true,
        shown_on_top: false,
      }))
      .call();
    let workspace = crate::models::Workspace::mock()
      .non_tiling_windows(vec![window.clone()])
      .call();
    let bounds = Rect::from_xy(100, 0, 1920, 1080);
    let work_area = Rect::from_xy(100, 0, 1920, 1040);
    let _monitor = Monitor::mock()
      .bounds(bounds.clone())
      .working_area(work_area.clone())
      .workspaces(vec![workspace])
      .call();
    assert_eq!(window.to_rect().unwrap(), work_area);
    window.set_state(WindowState::Fullscreen(FullscreenStateConfig {
      maximized: false,
      shown_on_top: false,
    }));
    assert_eq!(window.to_rect().unwrap(), bounds);
  }
}
