use std::time::Duration;

use ferese_layout::Rect;
use serde::{Deserialize, Serialize};

use crate::motion::{Geometry, Motion};

const FADE: Duration = Duration::from_millis(120);

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub idle_width: f64,
    pub peek_width: f64,
    pub peek_height: f64,
    pub toast_width: f64,
    pub toast_height: f64,
    pub expanded_width: f64,
    pub media_height: f64,
    pub calendar_height: f64,
    pub notifications_height: f64,
    pub controls_height: f64,
    pub expanded_radius: f64,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            idle_width: 150.,
            peek_width: 260.,
            peek_height: 44.,
            toast_width: 320.,
            toast_height: 48.,
            expanded_width: 340.,
            media_height: 170.,
            calendar_height: 230.,
            notifications_height: 176.,
            controls_height: 156.,
            expanded_radius: 26.,
        }
    }
}

impl Profile {
    pub fn validate(self) -> Result<(), &'static str> {
        let sizes = [
            self.idle_width,
            self.peek_width,
            self.peek_height,
            self.toast_width,
            self.toast_height,
            self.expanded_width,
            self.media_height,
            self.calendar_height,
            self.notifications_height,
            self.controls_height,
        ];
        if sizes
            .iter()
            .any(|value| !value.is_finite() || !(24. ..=1024.).contains(value))
            || !self.expanded_radius.is_finite()
            || !(0. ..=128.).contains(&self.expanded_radius)
        {
            Err("hub dimensions must be 24..1024 and its expanded radius 0..128")
        } else {
            Ok(())
        }
    }

    pub fn size(self, mode: Mode, collapsed_height: f64) -> [f64; 2] {
        match mode {
            Mode::Idle => [self.idle_width, collapsed_height],
            Mode::Peek => [self.peek_width, self.peek_height],
            Mode::Toast => [self.toast_width, self.toast_height],
            Mode::Expanded(content) => [
                self.expanded_width,
                match content {
                    Content::Media => self.media_height,
                    Content::Calendar => self.calendar_height,
                    Content::Notifications => self.notifications_height,
                    Content::Controls => self.controls_height,
                },
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    Media,
    Calendar,
    Notifications,
    Controls,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Idle,
    Peek,
    Toast,
    Expanded(Content),
}

impl Mode {
    pub fn expanded(self) -> bool {
        matches!(self, Self::Expanded(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub geometry: Geometry,
    pub previous: Mode,
    pub previous_opacity: f32,
    pub current: Mode,
    pub opacity: f32,
}

/// The exclusive zone is deliberately absent. Expansion changes the painted
/// island and its hit target; the caller retains the collapsed reservation.
#[derive(Clone, Debug)]
pub struct Hub {
    profile: Profile,
    pins: Option<[f64; 4]>,
    height: f64,
    motion: Motion,
    mode: Mode,
    previous: Mode,
    previous_opacity: f32,
    initial_opacity: f32,
    elapsed: Duration,
    ready_at: Option<Duration>,
}

impl Hub {
    pub fn new(profile: Profile, collapsed_height: f64) -> Result<Self, &'static str> {
        profile.validate()?;
        if !collapsed_height.is_finite() || !(24. ..=128.).contains(&collapsed_height) {
            return Err("hub height must be 24..128");
        }
        let size = profile.size(Mode::Idle, collapsed_height);
        Ok(Self {
            profile,
            pins: None,
            height: collapsed_height,
            motion: Motion::new(Geometry::rounded(Rect::new(0., 0., size[0], size[1]), size[1] / 2.))?,
            mode: Mode::Idle,
            previous: Mode::Idle,
            previous_opacity: 0.,
            initial_opacity: 1.,
            elapsed: FADE,
            ready_at: Some(Duration::ZERO),
        })
    }

    pub fn pin_radii(&mut self, pins: Option<[f64; 4]>) -> Result<(), &'static str> {
        if pins.is_some_and(|pins| {
            pins.iter()
                .any(|value| !value.is_finite() || !(0. ..=128.).contains(value))
        }) {
            return Err("hub radius pins must be finite values between 0 and 128");
        }
        self.pins = pins;
        let mut target = self.motion.target();
        target.radii = pins.unwrap_or(
            [if self.mode.expanded() {
                self.profile.expanded_radius
            } else {
                target.bounds.height / 2.
            }; 4],
        );
        self.motion.retarget(target)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn retarget(&mut self, mode: Mode, available: [f64; 2]) -> Result<(), &'static str> {
        if available.iter().any(|value| !value.is_finite() || *value <= 0.) {
            return Err("hub requires a finite positive available area");
        }
        let [width, height] = self.profile.size(mode, self.height);
        let width = width.min(available[0]);
        let height = height.min(available[1]);
        let radius = if mode.expanded() {
            self.profile.expanded_radius
        } else {
            height / 2.
        };
        let mut geometry = Geometry::rounded(Rect::new(0., 0., width, height), radius);
        geometry.radii = self.pins.unwrap_or(geometry.radii);
        if mode == self.mode && geometry == self.motion.target() {
            return Ok(());
        }
        let sample = self.sample();
        if sample.previous_opacity > sample.opacity {
            self.previous = sample.previous;
            self.previous_opacity = sample.previous_opacity;
        } else {
            self.previous = sample.current;
            self.previous_opacity = sample.opacity;
        }
        self.initial_opacity = if mode == sample.current {
            sample.opacity
        } else if mode == sample.previous {
            sample.previous_opacity
        } else {
            0.
        };
        if self.previous == mode {
            self.previous_opacity = 0.;
        }
        self.mode = mode;
        self.elapsed = Duration::ZERO;
        self.ready_at = None;
        self.motion.retarget(geometry)
    }

    pub fn advance(&mut self, elapsed: Duration, reduced_motion: bool) -> bool {
        self.elapsed = self.elapsed.saturating_add(elapsed);
        if !self.motion.advance(elapsed, reduced_motion) && self.ready_at.is_none() {
            self.ready_at = Some(self.elapsed);
        }
        if reduced_motion {
            self.previous_opacity = 0.;
            self.initial_opacity = 1.;
            self.ready_at = Some(Duration::ZERO);
        }
        self.active()
    }

    pub fn active(&self) -> bool {
        self.motion.active() || self.sample().previous_opacity > 0. || self.sample().opacity < 1.
    }

    pub fn sample(&self) -> Sample {
        let fade = |elapsed: Duration| (elapsed.as_secs_f32() / FADE.as_secs_f32()).clamp(0., 1.);
        let opacity = self.ready_at.map_or(self.initial_opacity, |ready| {
            self.initial_opacity + (1. - self.initial_opacity) * fade(self.elapsed.saturating_sub(ready))
        });
        Sample {
            geometry: self.motion.current(),
            previous: self.previous,
            previous_opacity: self.previous_opacity * (1. - fade(self.elapsed)),
            current: self.mode,
            opacity,
        }
    }

    pub fn target_size(&self) -> [f64; 2] {
        let target = self.motion.target().bounds;
        [target.width, target.height]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expanded_content_waits_for_the_shape_and_idle_stops_scheduling() {
        let mut hub = Hub::new(Profile::default(), 34.).unwrap();
        assert!(!hub.active());
        hub.retarget(Mode::Expanded(Content::Calendar), [800., 600.]).unwrap();
        assert_eq!(hub.target_size(), [340., 230.]);
        hub.advance(Duration::from_millis(100), false);
        assert_eq!(hub.sample().opacity, 0.);
        for _ in 0..50 {
            hub.advance(Duration::from_millis(16), false);
        }
        assert_eq!(hub.sample().opacity, 1.);
        assert_eq!(hub.sample().previous_opacity, 0.);
        assert!(!hub.active());
    }

    #[test]
    fn interrupted_expansion_keeps_the_presented_shape() {
        let mut hub = Hub::new(Profile::default(), 34.).unwrap();
        hub.retarget(Mode::Expanded(Content::Media), [800., 600.]).unwrap();
        hub.advance(Duration::from_millis(40), false);
        let before = hub.sample().geometry;
        hub.retarget(Mode::Idle, [800., 600.]).unwrap();
        assert_eq!(hub.sample().geometry, before);
        for _ in 0..50 {
            hub.advance(Duration::from_millis(16), false);
        }
        assert_eq!(hub.target_size(), [150., 34.]);
        assert!(!hub.active());
    }

    #[test]
    fn narrow_outputs_and_reduced_motion_keep_the_hub_bounded() {
        let mut hub = Hub::new(Profile::default(), 30.).unwrap();
        hub.retarget(Mode::Expanded(Content::Calendar), [240., 180.]).unwrap();
        hub.advance(Duration::ZERO, true);
        assert_eq!(hub.sample().geometry.bounds, Rect::new(0., 0., 240., 180.));
        assert_eq!(hub.sample().opacity, 1.);
        assert!(!hub.active());
        assert!(hub.retarget(Mode::Idle, [f64::NAN, 180.]).is_err());
        assert_eq!(hub.mode(), Mode::Expanded(Content::Calendar));
    }
}
