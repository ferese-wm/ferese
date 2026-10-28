//! Timestamp sampling keeps launcher motion local to its surface's frame callbacks.
use ferese_animation::{AnimatedValue, CrossingPolicy, MotionSettings, SpringConfig};
use std::time::Instant;

#[derive(Clone, Copy)]
pub(super) struct Sample {
    pub progress: AnimatedValue,
    pub height: AnimatedValue,
}

impl Sample {
    pub(super) fn active(self) -> bool {
        self.progress.is_animating() || self.height.is_animating()
    }
}

pub(super) struct Animation {
    values: Sample,
    pub started: Instant,
    settings: MotionSettings,
    closing: bool,
}

impl Animation {
    pub(super) fn new(settings: MotionSettings, now: Instant) -> Self {
        let mut progress = AnimatedValue::new(0.);
        progress.set_target(1.);
        Self {
            values: Sample {
                progress,
                height: AnimatedValue::new(72.),
            },
            started: now,
            settings,
            closing: false,
        }
    }

    pub(super) fn retarget(
        &mut self,
        height: f64,
        closing: bool,
        settings: MotionSettings,
        now: Instant,
    ) {
        if self.values.height.target == height
            && self.closing == closing
            && self.settings == settings
        {
            return;
        }
        self.values = self.sample(now);
        // Velocities are in simulation seconds; keep visible velocity when speed changes.
        let ratio = self.settings.speed / settings.speed;
        self.values.progress.velocity *= ratio;
        self.values.height.velocity *= ratio;
        self.values
            .progress
            .set_target(if closing { 0. } else { 1. });
        self.values.height.set_target(height);
        self.settings = settings;
        self.closing = closing;
        self.started = now;
    }

    pub(super) fn sample(&self, now: Instant) -> Sample {
        let mut values = self.values;
        if self.settings.motion_enabled() {
            let base = self.settings.spring_config().unwrap_or_default();
            let stiffness = base.stiffness * if self.closing { 1.6 } else { 1. };
            let spring = SpringConfig {
                stiffness,
                damping: 2. * (stiffness * base.mass).sqrt(),
                position_tolerance: 0.001,
                velocity_tolerance: 0.001,
                crossing: CrossingPolicy::NoCrossing,
                ..base
            };
            let elapsed = now
                .saturating_duration_since(self.started)
                .mul_f64(self.settings.speed);
            values.progress.advance(elapsed, spring);
            values.height.advance(elapsed, spring);
        } else {
            values.progress.snap();
            values.height.snap();
        }
        values
    }
}
