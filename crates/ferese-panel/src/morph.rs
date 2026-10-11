use std::time::Duration;

use ferese_layout::Rect;
use serde::{Deserialize, Serialize};

use crate::motion::{Geometry, Motion};

const FADE: Duration = Duration::from_millis(120);
const CONTENT_DELAY: Duration = Duration::from_millis(130);

/// A denser surface that retains the theme's hue and foreground contrast.
pub fn surface_color(mut background: [f32; 4]) -> [f32; 4] {
    let dark = background[0] * 0.2126 + background[1] * 0.7152 + background[2] * 0.0722 < 0.5;
    for channel in &mut background[..3] {
        *channel = if dark {
            *channel * 0.72
        } else {
            *channel + (1. - *channel) * 0.04
        };
    }
    background[3] = background[3].max(0.96);
    background
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub peek_on_hover: bool,
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
            peek_on_hover: true,
            idle_width: 150.,
            peek_width: 260.,
            peek_height: 44.,
            toast_width: 320.,
            toast_height: 48.,
            expanded_width: 340.,
            media_height: 170.,
            calendar_height: 230.,
            notifications_height: 176.,
            controls_height: 248.,
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
    available: [f64; 2],
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
            motion: Motion::with_spring(
                Geometry::rounded(Rect::new(0., 0., size[0], size[1]), size[1] / 2.),
                ferese_animation::SpringConfig {
                    stiffness: 300.,
                    damping: 2. * 0.7 * 300_f64.sqrt(),
                    position_tolerance: 0.05,
                    velocity_tolerance: 0.05,
                    crossing: ferese_animation::CrossingPolicy::AllowOvershoot,
                    ..Default::default()
                },
            )?,
            mode: Mode::Idle,
            previous: Mode::Idle,
            previous_opacity: 0.,
            initial_opacity: 1.,
            elapsed: FADE,
            available: [f64::MAX; 2],
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

    pub fn retarget_peek(&mut self, width: f64, available: [f64; 2]) -> Result<(), &'static str> {
        if !width.is_finite() || !(24. ..=1024.).contains(&width) {
            return Err("peek width must be 24..1024");
        }
        let configured = self.profile.peek_width;
        self.profile.peek_width = width;
        let result = self.retarget(Mode::Peek, available);
        self.profile.peek_width = configured;
        result
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
            self.available = available;
            return Ok(());
        }
        let sample = self.sample();
        self.available = available;
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
        self.motion.retarget(geometry)
    }

    /// Replace content inside an already settled shape without restarting its springs.
    pub fn replace_content(&mut self) {
        self.previous = self.mode;
        self.previous_opacity = self.sample().opacity;
        self.initial_opacity = 0.;
        self.elapsed = Duration::ZERO;
    }

    pub fn advance(&mut self, elapsed: Duration, reduced_motion: bool) -> bool {
        self.elapsed = self.elapsed.saturating_add(elapsed);
        self.motion.advance(elapsed, reduced_motion);
        if reduced_motion {
            self.previous_opacity = 0.;
            self.initial_opacity = 1.;
        }
        self.active()
    }

    pub fn active(&self) -> bool {
        self.motion.active() || self.sample().previous_opacity > 0. || self.sample().opacity < 1.
    }

    pub fn sample(&self) -> Sample {
        let fade = |elapsed: Duration| (elapsed.as_secs_f32() / FADE.as_secs_f32()).clamp(0., 1.);
        let opacity =
            self.initial_opacity + (1. - self.initial_opacity) * fade(self.elapsed.saturating_sub(CONTENT_DELAY));
        let mut geometry = self.motion.current();
        geometry.bounds.width = geometry.bounds.width.clamp(0., self.available[0]);
        geometry.bounds.height = geometry.bounds.height.clamp(0., self.available[1]);
        geometry.radii = geometry.radii.map(|radius| radius.max(0.));
        Sample {
            geometry,
            previous: self.previous,
            previous_opacity: self.previous_opacity * (1. - fade(self.elapsed)),
            current: self.mode,
            opacity,
        }
    }

    pub fn allocation_size(&self) -> [f64; 2] {
        let maximum = self.motion.maximum_size();
        std::array::from_fn(|i| maximum[i].min(self.available[i]))
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
    fn a_content_swap_fades_without_restarting_the_geometry() {
        let mut hub = Hub::new(Profile::default(), 34.).unwrap();
        hub.retarget(Mode::Expanded(Content::Controls), [800., 600.]).unwrap();
        hub.advance(Duration::from_secs(2), false);
        hub.advance(Duration::from_secs(1), false);
        let geometry = hub.sample().geometry;
        hub.replace_content();
        assert_eq!(hub.sample().geometry, geometry);
        assert_eq!(hub.sample().opacity, 0.);
        assert_eq!(hub.sample().previous_opacity, 1.);
        hub.advance(Duration::from_millis(60), false);
        assert_eq!(hub.sample().opacity, 0.);
        assert!((hub.sample().previous_opacity - 0.5).abs() < 0.001);
        hub.advance(Duration::from_millis(130), false);
        assert!((hub.sample().opacity - 0.5).abs() < 0.001);
        assert_eq!(hub.sample().previous_opacity, 0.);
        hub.advance(Duration::from_millis(60), false);
        assert!(!hub.active());
    }

    #[test]
    fn content_swap_uses_a_fixed_delay_instead_of_waiting_for_spring_settlement() {
        let mut hub = Hub::new(Profile::default(), 34.).unwrap();
        assert!(!hub.active());
        hub.retarget(Mode::Expanded(Content::Calendar), [800., 600.]).unwrap();
        assert_eq!(hub.target_size(), [340., 230.]);
        hub.advance(Duration::from_millis(120), false);
        assert_eq!(hub.sample().opacity, 0.);
        assert_eq!(hub.sample().previous_opacity, 0.);
        hub.advance(Duration::from_millis(70), false);
        assert!((hub.sample().opacity - 0.5).abs() < 0.001);
        hub.advance(Duration::from_millis(60), false);
        assert_eq!(hub.sample().opacity, 1.);
        assert!(hub.active(), "content finishes while the shape is still settling");
        for _ in 0..50 {
            hub.advance(Duration::from_millis(16), false);
        }
        assert_eq!(hub.sample().opacity, 1.);
        assert_eq!(hub.sample().previous_opacity, 0.);
        assert!(!hub.active());
    }

    #[test]
    fn an_interrupted_content_delay_never_reveals_the_cancelled_panel() {
        let mut hub = Hub::new(Profile::default(), 34.).unwrap();
        hub.retarget(Mode::Expanded(Content::Media), [800., 600.]).unwrap();
        hub.advance(Duration::from_millis(80), false);
        hub.retarget(Mode::Expanded(Content::Calendar), [800., 600.]).unwrap();
        for _ in 0..130 {
            hub.advance(Duration::from_millis(8), false);
            let sample = hub.sample();
            assert_eq!(sample.current, Mode::Expanded(Content::Calendar));
            assert_eq!(sample.previous, Mode::Idle);
        }
        assert!(!hub.active());
        assert_eq!(hub.sample().opacity, 1.);
    }

    #[test]
    fn spring_overshoots_then_settles_within_its_reserved_surface() {
        let mut hub = Hub::new(Profile::default(), 34.).unwrap();
        hub.retarget(Mode::Expanded(Content::Calendar), [800., 600.]).unwrap();
        let allocation = hub.allocation_size();
        let mut peak = 340_f64;
        for _ in 0..200 {
            hub.advance(Duration::from_millis(8), false);
            let sample = hub.sample();
            peak = peak.max(sample.geometry.bounds.width);
            assert!(sample.geometry.bounds.width <= allocation[0] + 1e-9);
            assert!(sample.geometry.bounds.height <= allocation[1] + 1e-9);
        }
        let overshoot = (peak - 340.) / (340. - 150.);
        assert!((0.04..0.05).contains(&overshoot));
        assert_eq!(hub.sample().geometry.bounds, Rect::new(0., 0., 340., 230.));
        assert!(!hub.active());
    }

    #[test]
    fn overshoot_is_clipped_to_small_outputs_and_reduced_motion_removes_all_fades() {
        let mut hub = Hub::new(Profile::default(), 34.).unwrap();
        hub.retarget(Mode::Expanded(Content::Calendar), [240., 180.]).unwrap();
        for _ in 0..60 {
            hub.advance(Duration::from_millis(8), false);
            let bounds = hub.sample().geometry.bounds;
            assert!(bounds.width <= 240. && bounds.height <= 180.);
        }
        hub.retarget(Mode::Idle, [240., 180.]).unwrap();
        hub.advance(Duration::ZERO, true);
        assert_eq!(hub.sample().geometry.bounds, Rect::new(0., 0., 150., 34.));
        assert_eq!(hub.sample().previous_opacity, 0.);
        assert_eq!(hub.sample().opacity, 1.);
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
