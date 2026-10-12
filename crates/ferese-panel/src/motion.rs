use std::time::Duration;

use ferese_animation::{AnimatedValue, CrossingPolicy, SpringConfig};
use ferese_layout::Rect;

/// Unsnapped logical geometry. Pixel snapping and radius normalization belong
/// to scene derivation, so neither changes the spring's trajectory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub bounds: Rect,
    pub radii: [f64; 4],
    pub margin: [f64; 4],
    pub thickness: [f64; 4],
    pub gap: f64,
    pub fusion: f64,
}

const COUNT: usize = 18;

impl Geometry {
    pub fn rounded(bounds: Rect, radius: f64) -> Self {
        Self {
            bounds,
            radii: [radius; 4],
            margin: [0.; 4],
            thickness: [0.; 4],
            gap: 0.,
            fusion: 0.,
        }
    }

    fn values(self) -> [f64; COUNT] {
        let mut values = [0.; COUNT];
        values[..4].copy_from_slice(&[self.bounds.x, self.bounds.y, self.bounds.width, self.bounds.height]);
        values[4..8].copy_from_slice(&self.radii);
        values[8..12].copy_from_slice(&self.margin);
        values[12..16].copy_from_slice(&self.thickness);
        values[16] = self.gap;
        values[17] = self.fusion;
        values
    }

    fn from_values(values: [f64; COUNT]) -> Self {
        Self {
            bounds: Rect::new(values[0], values[1], values[2], values[3]),
            radii: values[4..8].try_into().unwrap(),
            margin: values[8..12].try_into().unwrap(),
            thickness: values[12..16].try_into().unwrap(),
            gap: values[16],
            fusion: values[17],
        }
    }

    fn validate(self) -> Result<Self, &'static str> {
        let values = self.values();
        if values.iter().any(|value| !value.is_finite()) || values[2..].iter().any(|value| *value < 0.) {
            Err("panel animation geometry must be finite with nonnegative extents")
        } else {
            Ok(self)
        }
    }
}

#[derive(Clone, Debug)]
pub struct Motion {
    values: [AnimatedValue; COUNT],
    spring: SpringConfig,
}

impl Motion {
    pub fn new(geometry: Geometry) -> Result<Self, &'static str> {
        Self::with_spring(
            geometry,
            SpringConfig {
                stiffness: 6400.,
                damping: 160.,
                position_tolerance: 0.05,
                velocity_tolerance: 0.1,
                crossing: CrossingPolicy::NoCrossing,
                ..Default::default()
            },
        )
    }

    pub(crate) fn with_spring(geometry: Geometry, spring: SpringConfig) -> Result<Self, &'static str> {
        Ok(Self {
            values: geometry.validate()?.values().map(AnimatedValue::new),
            spring,
        })
    }

    /// Largest future dimensions, including the momentum carried through a retarget.
    pub(crate) fn maximum_size(&self) -> [f64; 2] {
        let omega2 = self.spring.stiffness / self.spring.mass;
        let decay = self.spring.damping / (2. * self.spring.mass);
        [self.values[2], self.values[3]].map(|value| {
            let displacement = value.current - value.target;
            let mut maximum = value.current.max(value.target);
            if omega2 > decay * decay {
                let frequency = (omega2 - decay * decay).sqrt();
                let sine = (value.velocity + decay * displacement) / frequency;
                let derivative_sine = -(decay * value.velocity + omega2 * displacement) / frequency;
                let first = (-value.velocity)
                    .atan2(derivative_sine)
                    .rem_euclid(std::f64::consts::PI);
                // Extrema alternate; later peaks decay, so the first two suffice.
                for phase in [first, first + std::f64::consts::PI] {
                    let t = phase / frequency;
                    let position =
                        value.target + (-decay * t).exp() * (displacement * phase.cos() + sine * phase.sin());
                    maximum = maximum.max(position);
                }
            } else {
                // Total spring energy bounds displacement for any nonnegative damping.
                maximum = maximum.max(
                    value.target + (displacement * displacement + value.velocity * value.velocity / omega2).sqrt(),
                );
            }
            maximum
        })
    }

    pub fn current(&self) -> Geometry {
        Geometry::from_values(self.values.map(|value| value.current))
    }

    pub fn target(&self) -> Geometry {
        Geometry::from_values(self.values.map(|value| value.target))
    }

    pub fn retarget(&mut self, geometry: Geometry) -> Result<(), &'static str> {
        let target = geometry.validate()?.values();
        for (value, target) in self.values.iter_mut().zip(target) {
            value.retarget_preserving_motion(target);
        }
        Ok(())
    }

    pub fn active(&self) -> bool {
        self.values.iter().any(AnimatedValue::is_animating)
    }

    /// Returns whether another frame is needed. Reduced motion also clears
    /// residual velocity, including when enabled halfway through an animation.
    pub fn advance(&mut self, elapsed: Duration, reduced_motion: bool) -> bool {
        let mut active = false;
        for value in &mut self.values {
            if reduced_motion {
                value.snap();
            } else {
                active |= value.advance(elapsed, self.spring);
            }
        }
        active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collapsed() -> Geometry {
        Geometry::rounded(Rect::new(0., 0., 150., 34.), 17.)
    }
    fn expanded() -> Geometry {
        Geometry::rounded(Rect::new(-95., 0., 340., 230.), 26.)
    }

    #[test]
    fn interruption_keeps_the_current_geometry_and_velocity() {
        let mut motion = Motion::new(collapsed()).unwrap();
        motion.retarget(expanded()).unwrap();
        motion.advance(Duration::from_millis(16), false);
        let before = motion.current();
        let velocity = motion.values.map(|value| value.velocity);
        motion.retarget(collapsed()).unwrap();
        assert_eq!(motion.current(), before);
        assert_eq!(motion.values.map(|value| value.velocity), velocity);
        for _ in 0..30 {
            motion.advance(Duration::from_millis(16), false);
        }
        assert_eq!(motion.current(), collapsed());
        assert!(!motion.active());
    }

    #[test]
    fn dimensions_share_one_trajectory_and_stop_requesting_frames() {
        let mut motion = Motion::new(collapsed()).unwrap();
        motion.retarget(expanded()).unwrap();
        for _ in 0..12 {
            motion.advance(Duration::from_millis(16), false);
            let geometry = motion.current();
            let width_progress = (geometry.bounds.width - 150.) / 190.;
            let height_progress = (geometry.bounds.height - 34.) / 196.;
            let radius_progress = (geometry.radii[0] - 17.) / 9.;
            // A dimension may snap at its final subpixel tolerance first.
            assert!((width_progress - height_progress).abs() < 0.0005);
            assert!((width_progress - radius_progress).abs() < 0.0005);
        }
        for _ in 0..12 {
            motion.advance(Duration::from_millis(16), false);
        }
        assert_eq!(motion.current(), expanded());
        assert!(!motion.advance(Duration::from_secs(1), false));
    }

    #[test]
    fn reduced_motion_snaps_an_inflight_animation() {
        let mut motion = Motion::new(collapsed()).unwrap();
        motion.retarget(expanded()).unwrap();
        motion.advance(Duration::from_millis(16), false);
        assert!(!motion.advance(Duration::ZERO, true));
        assert_eq!(motion.current(), expanded());
    }

    #[test]
    fn underdamped_reversals_preserve_velocity_and_fit_the_predicted_extrema() {
        let mut motion = Motion::with_spring(
            collapsed(),
            SpringConfig {
                stiffness: 300.,
                damping: 2. * 0.7 * 300_f64.sqrt(),
                crossing: CrossingPolicy::AllowOvershoot,
                ..Default::default()
            },
        )
        .unwrap();
        for turn in 0..60 {
            let before = motion.current();
            let velocity = motion.values.map(|value| value.velocity);
            motion
                .retarget(if turn % 2 == 0 { expanded() } else { collapsed() })
                .unwrap();
            assert_eq!(motion.current(), before);
            assert_eq!(motion.values.map(|value| value.velocity), velocity);
            let bound = motion.maximum_size();
            let mut prediction = motion.clone();
            for _ in 0..200 {
                prediction.advance(Duration::from_millis(8), false);
                let geometry = prediction.current();
                assert!(geometry.bounds.width <= bound[0] + 1e-9);
                assert!(geometry.bounds.height <= bound[1] + 1e-9);
            }
            motion.advance(Duration::from_millis(16 + turn * 7), false);
        }
    }

    #[test]
    fn invalid_retarget_leaves_motion_untouched() {
        let mut motion = Motion::new(collapsed()).unwrap();
        let mut invalid = expanded();
        invalid.bounds.height = f64::NAN;
        assert!(motion.retarget(invalid).is_err());
        assert_eq!(motion.target(), collapsed());
        assert!(!motion.active());
    }
}
