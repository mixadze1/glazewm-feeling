use std::{
  cell::{Ref, RefCell, RefMut},
  collections::VecDeque,
  rc::{Rc, Weak},
};

use ambassador::Delegate;
use enum_as_inner::EnumAsInner;
use uuid::Uuid;
use wm_common::{
  ActiveDrag, ContainerDto, DisplayState, GapsConfig, TilingDirection,
  WindowRuleConfig, WindowState,
};
use wm_platform::{Direction, NativeWindow, Rect, RectDelta};

#[allow(clippy::wildcard_imports)]
use crate::{
  models::{
    Monitor, NativeWindowProperties, NonTilingWindow, RootContainer,
    SplitContainer, TilingWindow, Workspace,
  },
  traits::*,
  user_config::UserConfig,
};

/// A container of any type.
///
/// Uses:
///
///  * [`wm_macros::SubEnum`] to define subtypes of containers.
///  * [`wm_macros::EnumFromInner`] to define conversions between the enum
///    and wrapped types.
///  * [`ambassador::Delegate`] to delegate common getters to the contained
///    types. E.g. implements [`CommonGetters`] for [Container] by
///    forwarding the call to the item contained in the enum variant.
///
/// # Example
/// Conversion between the different container types:
/// ```
/// use wm::models::{Container, DirectionContainer, SplitContainer, TilingContainer};
/// use wm::traits::{TilingSizeGetters, TilingDirectionGetters};
///
/// fn example(split: SplitContainer) {
///   // Convert a `SplitContainer` into a `Container`
///   let container: Container = split.into(); // Will be a `Container::Split`
///
///   // Could also have gone straight to a [TilingContainer] from SplitContainer
///   // let tiling: TilingContainer = split.into(); // Will be a `TilingContainer::Split`
///
///   // Try to convert a [Container] into a sub container type ([TilingContainer] in this case).
///   let tiling: TilingContainer = container.try_into().unwrap(); // Will be a `TilingContainer::Split`
///   tiling.tiling_size(); // Can use methods from the `TilingSizeGetters` trait.
///
///   // Try to convert a one sub container type into another. ([TilingContainer] to [DirectionContainer] in this case).
///   let direction: DirectionContainer = tiling.try_into().unwrap(); // Will be a `DirectionContainer::Split`
///   direction.tiling_direction(); // Can use methods from the `TilingDirectionGetters` trait.
///
///   // Convert a sub container back into a [Container]
///   let container: Container = direction.into(); // Will be a `Container::Split`
/// }
/// ```
#[derive(
  Clone,
  Debug,
  EnumAsInner,
  wm_macros::EnumFromInner,
  Delegate,
  wm_macros::SubEnum,
)]
#[delegate(CommonGetters)]
#[delegate(PositionGetters)]
#[subenum(defaults, {
  /// Subenum of [Container]
  #[derive(Clone, Debug, EnumAsInner, Delegate, wm_macros::EnumFromInner)]
  #[delegate(CommonGetters)]
  #[delegate(PositionGetters)]
})]
#[subenum(TilingContainer, {
  /// Subset of containers that implement the following traits:
  /// * `CommonGetters`
  /// * `PositionGetters`
  /// * `TilingSizeGetters`
  #[delegate(TilingSizeGetters)]
})]
#[subenum(WindowContainer, {
  /// Subset of containers that implement the following traits:
  /// * `CommonGetters`
  /// * `PositionGetters`
  /// * `WindowGetters`
  #[delegate(WindowGetters)]
})]
#[subenum(DirectionContainer, {
  /// Subset of containers that implement the following traits:
  /// * `CommonGetters`
  /// * `PositionGetters`
  /// * `DirectionGetters`
  #[delegate(TilingDirectionGetters)]
})]
pub enum Container {
  Root(RootContainer),
  Monitor(Monitor),
  #[subenum(DirectionContainer)]
  Workspace(Workspace),
  #[subenum(TilingContainer, DirectionContainer)]
  Split(SplitContainer),
  #[subenum(TilingContainer, WindowContainer)]
  TilingWindow(TilingWindow),
  #[subenum(WindowContainer)]
  NonTilingWindow(NonTilingWindow),
}

/// Non-owning parent link. Children own their descendants, never
/// ancestors.
#[derive(Clone, Debug)]
pub struct WeakContainer(WeakContainerInner);

#[derive(Clone, Debug)]
enum WeakContainerInner {
  Root(Weak<RefCell<super::root_container::RootContainerInner>>),
  Monitor(Weak<RefCell<super::monitor::MonitorInner>>),
  Workspace(Weak<RefCell<super::workspace::WorkspaceInner>>),
  Split(Weak<RefCell<super::split_container::SplitContainerInner>>),
  TilingWindow(Weak<RefCell<super::tiling_window::TilingWindowInner>>),
  NonTilingWindow(
    Weak<RefCell<super::non_tiling_window::NonTilingWindowInner>>,
  ),
}

impl Container {
  pub fn downgrade(&self) -> WeakContainer {
    WeakContainer(match self {
      Self::Root(value) => {
        WeakContainerInner::Root(Rc::downgrade(&value.0))
      }
      Self::Monitor(value) => {
        WeakContainerInner::Monitor(Rc::downgrade(&value.0))
      }
      Self::Workspace(value) => {
        WeakContainerInner::Workspace(Rc::downgrade(&value.0))
      }
      Self::Split(value) => {
        WeakContainerInner::Split(Rc::downgrade(&value.0))
      }
      Self::TilingWindow(value) => {
        WeakContainerInner::TilingWindow(Rc::downgrade(&value.0))
      }
      Self::NonTilingWindow(value) => {
        WeakContainerInner::NonTilingWindow(Rc::downgrade(&value.0))
      }
    })
  }
}

impl WeakContainer {
  pub fn upgrade(&self) -> Option<Container> {
    match &self.0 {
      WeakContainerInner::Root(value) => value
        .upgrade()
        .map(|inner| Container::Root(RootContainer(inner))),
      WeakContainerInner::Monitor(value) => value
        .upgrade()
        .map(|inner| Container::Monitor(Monitor(inner))),
      WeakContainerInner::Workspace(value) => value
        .upgrade()
        .map(|inner| Container::Workspace(Workspace(inner))),
      WeakContainerInner::Split(value) => value
        .upgrade()
        .map(|inner| Container::Split(SplitContainer(inner))),
      WeakContainerInner::TilingWindow(value) => value
        .upgrade()
        .map(|inner| Container::TilingWindow(TilingWindow(inner))),
      WeakContainerInner::NonTilingWindow(value) => value
        .upgrade()
        .map(|inner| Container::NonTilingWindow(NonTilingWindow(inner))),
    }
  }
}

impl PartialEq for Container {
  fn eq(&self, other: &Self) -> bool {
    self.id() == other.id()
  }
}

impl Eq for Container {}

impl PartialEq for TilingContainer {
  fn eq(&self, other: &Self) -> bool {
    self.id() == other.id()
  }
}

impl Eq for TilingContainer {}

impl PartialEq for WindowContainer {
  fn eq(&self, other: &Self) -> bool {
    self.id() == other.id()
  }
}

impl Eq for WindowContainer {}

impl std::fmt::Display for WindowContainer {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    // Truncate title if longer than 20 chars. Need to use `chars()`
    // instead of byte slices to handle invalid byte indices.
    let title = {
      let title = self.native_properties().title;
      if title.len() > 20 {
        format!("{}...", title.chars().take(17).collect::<String>())
      } else {
        title
      }
    };

    let class = {
      #[cfg(target_os = "windows")]
      {
        self.native_properties().class_name
      }
      #[cfg(not(target_os = "windows"))]
      {
        String::new()
      }
    };

    let process = self.native_properties().process_name;

    write!(
      f,
      "Window(id={:?}, process={}, class={}, title={})",
      self.native().id(),
      process,
      class,
      title,
    )?;

    Ok(())
  }
}

impl PartialEq for DirectionContainer {
  fn eq(&self, other: &Self) -> bool {
    self.id() == other.id()
  }
}

impl Eq for DirectionContainer {}

/// Implements the `Debug` trait for a given container struct.
///
/// Expects that the struct has a `to_dto()` method.
#[macro_export]
macro_rules! impl_container_debug {
  ($type:ty) => {
    impl std::fmt::Debug for $type {
      fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Debug::fmt(
          &self.to_dto().map_err(|_| std::fmt::Error),
          f,
        )
      }
    }
  };
}

#[cfg(test)]
mod ownership_tests {
  use super::*;
  use crate::commands::container::{attach_container, detach_container};

  #[test]
  fn dropping_tree_releases_every_container_type() {
    let root = RootContainer::new().as_container();
    let tiled = TilingWindow::mock().call();
    let floating = NonTilingWindow::mock().call();
    let split = SplitContainer::mock()
      .tiling_containers(vec![tiled.clone().into()])
      .call();
    let workspace = Workspace::mock()
      .tiling_containers(vec![split.clone().into()])
      .non_tiling_windows(vec![floating.clone()])
      .call();
    let monitor =
      Monitor::mock().workspaces(vec![workspace.clone()]).call();
    attach_container(&monitor.as_container(), &root, None).unwrap();
    let weak = [
      root.downgrade(),
      monitor.as_container().downgrade(),
      workspace.as_container().downgrade(),
      split.as_container().downgrade(),
      tiled.as_container().downgrade(),
      floating.as_container().downgrade(),
    ];
    drop((monitor, workspace, split, tiled, floating));
    assert!(weak.iter().all(|value| value.upgrade().is_some()));
    drop(root);
    assert!(weak.iter().all(|value| value.upgrade().is_none()));
  }

  #[test]
  fn detached_subtree_drops_without_leaking_descendants() {
    let child = TilingWindow::mock().call().as_container();
    let workspace = Workspace::mock()
      .tiling_containers(vec![child.clone().try_into().unwrap()])
      .call()
      .as_container();
    let monitor = Monitor::mock().call().as_container();
    attach_container(&workspace, &monitor, None).unwrap();
    let weak_child = child.downgrade();
    drop(child);
    detach_container(workspace.clone()).unwrap();
    drop(workspace);
    assert!(weak_child.upgrade().is_none());
    assert_eq!(monitor.child_count(), 0);
  }

  #[test]
  fn surviving_child_does_not_keep_parent_alive() {
    let child = TilingWindow::mock().call().as_container();
    let parent = Workspace::mock().call().as_container();
    attach_container(&child, &parent, None).unwrap();
    let weak_parent = parent.downgrade();
    drop(parent);
    assert!(weak_parent.upgrade().is_none());
    assert!(child.is_detached());
    assert_eq!(child.ancestors().count(), 0);
  }

  #[test]
  fn child_focus_iterator_advances_and_terminates() {
    let first = TilingWindow::mock().call();
    let second = TilingWindow::mock().call();
    let workspace = Workspace::mock()
      .tiling_containers(vec![first.clone().into(), second.clone().into()])
      .call();
    // take bounds the regression: the old implementation yielded the
    // first child forever and collecting the iterator exhausted memory.
    let ids: Vec<_> = workspace
      .child_focus_order()
      .take(3)
      .map(|child| child.id())
      .collect();
    assert_eq!(ids, vec![first.id(), second.id()]);
  }
}
