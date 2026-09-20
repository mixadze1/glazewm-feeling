use std::time::{Duration, Instant};

use wm_common::{DesktopToggleConfig, WindowState};
use wm_platform::{
  NativeWindow, NativeWindowWindowsExt, WorkspaceSurrogate,
};

use crate::{
  animation::engine::{animation_progress_at, apply_easing},
  traits::{CommonGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

pub struct DesktopAnimation {
  motion: DesktopMotion,
  surrogates: Vec<WorkspaceSurrogate>,
}

struct DesktopMotion {
  started: Instant,
  duration: Duration,
  from: f32,
  to: f32,
  options: DesktopToggleConfig,
}

impl DesktopMotion {
  fn new(
    showing: bool,
    options: DesktopToggleConfig,
    now: Instant,
  ) -> Self {
    let duration = if showing {
      options.show_duration_ms
    } else {
      options.hide_duration_ms
    };
    Self {
      started: now,
      duration: Duration::from_millis(u64::from(duration)),
      from: if showing { 0.0 } else { 1.0 },
      to: if showing { 1.0 } else { 0.0 },
      options,
    }
  }

  fn sample(&self, now: Instant) -> f32 {
    let progress = animation_progress_at(self.started, self.duration, now);
    if progress >= 1.0 {
      return self.to;
    }
    let eased =
      apply_easing(progress, &self.options.easing).clamp(0.0, 1.0);
    self.from + (self.to - self.from) * eased
  }

  fn reverse(&mut self, now: Instant) {
    self.from = self.sample(now);
    self.to = 1.0 - self.to;
    let duration = if self.to == 1.0 {
      self.options.show_duration_ms
    } else {
      self.options.hide_duration_ms
    };
    self.duration = Duration::from_secs_f64(
      f64::from(duration) / 1000.0
        * f64::from((self.to - self.from).abs()),
    );
    self.started = now;
  }
}

impl DesktopAnimation {
  fn draw(&mut self, now: Instant) {
    let visible = self.motion.sample(now);
    let options = &self.motion.options;
    let scale = if options.hidden_scale.is_finite() {
      options.hidden_scale.clamp(0.0, 2.0)
    } else {
      0.94
    };
    for surrogate in &mut self.surrogates {
      surrogate.update_desktop(
        visible,
        scale,
        options.offset_x,
        options.offset_y,
      );
    }
  }
}

fn create_animation(
  state: &WmState,
  config: &UserConfig,
  showing: bool,
) -> anyhow::Result<DesktopAnimation> {
  let options = config.value.animations.desktop_toggle.clone();
  let mut surrogates = Vec::new();
  for window in state.windows().into_iter().filter(|window| {
    window.state() != WindowState::Minimized
      && window
        .workspace()
        .is_some_and(|workspace| workspace.is_displayed())
  }) {
    let Some(monitor) = window.monitor() else {
      continue;
    };
    let effect = if window.has_focus(None) {
      &config.value.window_effects.focused_window
    } else {
      &config.value.window_effects.other_windows
    };
    let opacity = if effect.transparency.enabled {
      effect.transparency.opacity.to_alpha()
    } else {
      u8::MAX
    };
    let native = window.native();
    let hidden_opacity = if options.hidden_opacity.is_finite() {
      options.hidden_opacity.clamp(0.0, 1.0)
    } else {
      0.0
    };
    let mut surrogate = WorkspaceSurrogate::new(
      native.hwnd(),
      &native.frame()?,
      &monitor.native_properties().bounds,
      opacity,
      hidden_opacity,
    )?;
    if showing {
      surrogate.show_incoming();
    } else {
      surrogate.show_initial();
    }
    surrogates.push(surrogate);
  }
  Ok(DesktopAnimation {
    motion: DesktopMotion::new(showing, options, Instant::now()),
    surrogates,
  })
}

/// Restore immediately before another command, pause, or monitor change.
#[track_caller]
pub fn restore_desktop(state: &mut WmState) -> anyhow::Result<()> {
  tracing::debug!(
    "Restoring desktop from {}",
    std::panic::Location::caller()
  );
  // Keep the final preview alive until the real windows are revealed.
  let preview = state.desktop_animation.take();
  state.animation_manager.set_desktop_animation_active(false);
  if let Some(windows) = state.desktop_windows.take() {
    let mut failed = Vec::new();
    for window in windows {
      if window.is_valid() && window.set_cloaked(false).is_err() {
        failed.push(window);
      }
    }
    if !failed.is_empty() {
      state.desktop_windows = Some(failed);
      anyhow::bail!(
        "Unable to restore some desktop windows; retry the toggle."
      );
    }
    state.pending_sync.suppress_animations();
    state
      .pending_sync
      .queue_container_to_redraw(state.root_container.clone())
      .queue_focus_change()
      .queue_all_effects_update();
  }
  drop(preview);
  Ok(())
}

pub fn tick_desktop_animation(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let Some(animation) = &mut state.desktop_animation else {
    return Ok(());
  };
  let now = Instant::now();
  animation.draw(now);
  if now.saturating_duration_since(animation.motion.started)
    < animation.motion.duration
  {
    return Ok(());
  }
  if animation.motion.to == 1.0 {
    restore_desktop(state)?;
    super::platform_sync(state, config)?;
  } else {
    state.desktop_animation = None;
    state.animation_manager.set_desktop_animation_active(false);
  }
  Ok(())
}

/// Keep the original tree and native geometry intact; only previews
/// animate.
pub fn toggle_desktop(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  if let Some(animation) = &mut state.desktop_animation {
    animation.motion.reverse(Instant::now());
    return Ok(());
  }
  let options = &config.value.animations.desktop_toggle;
  if state.desktop_windows.is_some() {
    if options.enabled && options.show_duration_ms > 0 {
      match create_animation(state, config, true) {
        Ok(mut animation) => {
          animation.draw(Instant::now());
          state.desktop_animation = Some(animation);
          state.animation_manager.set_desktop_animation_active(true);
          return Ok(());
        }
        Err(error) => {
          tracing::warn!("Desktop preview unavailable: {error}")
        }
      }
    }
    return restore_desktop(state);
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
  let preview = if options.enabled && options.hide_duration_ms > 0 {
    match create_animation(state, config, false) {
      Ok(animation) => Some(animation),
      Err(error) => {
        tracing::warn!("Desktop preview unavailable: {error}");
        None
      }
    }
  } else {
    None
  };

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
  state.desktop_animation = preview;
  if let Some(animation) = &mut state.desktop_animation {
    // Cloaking several apps can be slow: start the clock after setup.
    animation.motion.started = Instant::now();
    animation.draw(animation.motion.started);
    state.animation_manager.set_desktop_animation_active(true);
  }
  state.focus_outline = None;
  state.resize_cursor_clip = None;
  state.dispatcher.reset_focus()?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reversing_mid_flight_preserves_current_visibility_and_finishes() {
    let now = Instant::now();
    let mut motion =
      DesktopMotion::new(false, DesktopToggleConfig::default(), now);
    let midway = now + Duration::from_millis(100);
    let before = motion.sample(midway);
    assert!(before > 0.0 && before < 1.0);
    motion.reverse(midway);
    assert_eq!(motion.sample(midway), before);
    assert_eq!(motion.sample(midway + Duration::from_secs(1)), 1.0);
    motion.reverse(midway + Duration::from_millis(30));
    assert_eq!(motion.sample(midway + Duration::from_secs(1)), 0.0);
  }

  #[test]
  fn zero_duration_reaches_endpoint_without_nan() {
    let now = Instant::now();
    let options = DesktopToggleConfig {
      hide_duration_ms: 0,
      show_duration_ms: 0,
      ..Default::default()
    };
    assert_eq!(
      DesktopMotion::new(false, options.clone(), now).sample(now),
      0.0
    );
    assert_eq!(DesktopMotion::new(true, options, now).sample(now), 1.0);
  }
}
