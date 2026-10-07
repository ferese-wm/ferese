use std::time::Duration;

use crate::config::FocusEffectSettings;
use ferese_layout::WindowId;

pub(crate) fn target(settings: FocusEffectSettings, active: Option<WindowId>, window: WindowId) -> f64 {
    if !settings.has_effects() || active == Some(window) {
        1.0
    } else {
        0.0
    }
}

impl FocusEffectSettings {
    pub(crate) fn has_effects(self) -> bool {
        self.enabled && (self.active_opacity != self.inactive_opacity || self.inactive_dim != 0.0)
    }

    pub(crate) fn resolve(self, progress: f64, has_active: bool, overview_opacity: f32) -> (f32, f64) {
        if !self.enabled {
            return (1.0, 0.0);
        }
        let progress = progress.clamp(0.0, 1.0);
        let alpha = self.inactive_opacity + (self.active_opacity - self.inactive_opacity) * progress;
        let dim = if has_active {
            self.inactive_dim * (1.0 - progress) * (1.0 - f64::from(overview_opacity))
        } else {
            0.0
        };
        (alpha as f32, dim)
    }
}

/// A bounded, interruption-safe opacity transition; it never overshoots.
#[derive(Clone, Debug)]
pub(crate) struct BoundedFade {
    pub current: f64,
    start: f64,
    target: f64,
    elapsed: f64,
}

impl BoundedFade {
    pub(crate) fn needs_update(&self, target: f64) -> bool {
        self.target != target || self.current != target
    }

    pub(crate) fn is_animating(&self) -> bool {
        self.current != self.target
    }

    pub fn new(value: f64) -> Self {
        Self {
            current: value,
            start: value,
            target: value,
            elapsed: 0.0,
        }
    }

    pub fn advance(&mut self, target: f64, delta: Duration, duration_ms: f64) -> bool {
        if self.target != target {
            self.start = self.current;
            self.target = target;
            self.elapsed = 0.0;
        }

        if duration_ms == 0.0 {
            self.current = target;
            return false;
        }

        self.elapsed += delta.as_secs_f64() * 1000.0;
        let progress = (self.elapsed / duration_ms).min(1.0);
        if progress == 1.0 {
            self.current = target;
            return false;
        }

        let eased = progress * progress * (3.0 - 2.0 * progress);
        self.current = self.start + (target - self.start) * eased;
        self.current != target
    }

    /// The target changed at this tick, not at the beginning of a potentially
    /// long idle interval. Do not spend that old elapsed time on a new fade.
    pub fn advance_visual(&mut self, target: f64, delta: Duration, duration_ms: f64) -> bool {
        self.advance(
            target,
            if self.target != target { Duration::ZERO } else { delta },
            duration_ms,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> FocusEffectSettings {
        FocusEffectSettings {
            enabled: true,
            active_opacity: 1.0,
            inactive_opacity: 0.8,
            inactive_dim: 0.2,
            duration_ms: 150.0,
        }
    }

    #[test]
    fn disabled_effect_bypasses_opacity_and_dimming_without_changing_settings() {
        let mut effect = settings();
        effect.enabled = false;
        assert!(!effect.has_effects());
        for progress in [0.0, 0.5, 1.0] {
            assert_eq!(effect.resolve(progress, true, 0.0), (1.0, 0.0));
        }
        assert_eq!(target(effect, Some(WindowId(1)), WindowId(2)), 1.0);
        effect.enabled = true;
        assert_eq!(effect.resolve(0.0, true, 0.0), (0.8, 0.2));
    }

    #[test]
    fn opacity_and_dimming_share_one_interruption_safe_transition() {
        let mut focus = BoundedFade::new(0.0);
        assert!(focus.advance_visual(1.0, Duration::from_secs(30), 150.0));
        assert_eq!(focus.current, 0.0);
        focus.advance_visual(1.0, Duration::from_millis(75), 150.0);
        let (alpha, dim) = settings().resolve(focus.current, true, 0.0);
        assert!((alpha - 0.9).abs() < 1e-6);
        assert!((dim - 0.1).abs() < 1e-9);
        let before = focus.current;
        focus.advance_visual(0.0, Duration::from_secs(30), 150.0);
        assert_eq!(focus.current, before);
        assert!(!focus.advance_visual(0.0, Duration::from_millis(150), 150.0));
        assert_eq!(settings().resolve(focus.current, true, 0.0), (0.8, 0.2));
    }

    #[test]
    fn activation_is_independent_of_overview_and_no_active_window() {
        assert_eq!(target(settings(), Some(WindowId(1)), WindowId(1)), 1.0);
        assert_eq!(target(settings(), Some(WindowId(1)), WindowId(2)), 0.0);
        assert_eq!(target(settings(), None, WindowId(2)), 0.0);
        assert_eq!(settings().resolve(0.0, true, 1.0), (0.8, 0.0));
        assert_eq!(settings().resolve(0.0, false, 0.0), (0.8, 0.0));
        assert_eq!(settings().resolve(0.0, true, 0.5), (0.8, 0.1));
    }

    #[test]
    fn reduced_motion_and_zero_duration_snap() {
        let mut focus = BoundedFade::new(0.0);
        assert!(!focus.advance_visual(1.0, Duration::ZERO, 0.0));
        assert_eq!(settings().resolve(focus.current, true, 0.0), (1.0, 0.0));
    }
}
