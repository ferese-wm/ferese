use crate::{CrossingPolicy, SpringConfig};

/// One configuration contract for compositor and out-of-process shell motion.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MotionSettings {
    pub enabled: bool,
    pub reduced_motion: bool,
    pub speed: f64,
    pub spring: SpringSettings,
    pub viewport_spring: SpringSettings,
}

impl Default for MotionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            reduced_motion: false,
            speed: 1.0,
            spring: SpringSettings::default(),
            viewport_spring: SpringSettings::default(),
        }
    }
}

impl MotionSettings {
    pub fn motion_enabled(self) -> bool {
        self.enabled && !self.reduced_motion
    }

    pub fn validate(self) -> Result<(), String> {
        self.validate_speed()?;
        self.spring_config()?;
        self.viewport_config()?;
        Ok(())
    }

    /// Bound both duration division and elapsed-time multiplication.
    pub fn validate_speed(self) -> Result<f64, String> {
        if self.speed.is_finite() && (0.01..=100.0).contains(&self.speed) {
            Ok(self.speed)
        } else {
            Err("animation speed must be between 0.01 and 100 (inclusive)".into())
        }
    }

    pub fn spring_config(self) -> Result<SpringConfig, String> {
        self.spring.resolve(240.0)
    }

    pub fn viewport_config(self) -> Result<SpringConfig, String> {
        self.viewport_spring.resolve(350.0)
    }

    /// Only small opacity changes should use a timed duration.
    pub fn duration(self, milliseconds: f64) -> std::time::Duration {
        if !self.motion_enabled() {
            return std::time::Duration::ZERO;
        }
        std::time::Duration::from_secs_f64(milliseconds / 1000.0 / self.speed)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpringSettings {
    pub duration_ms: Option<f64>,
    pub bounce: f64,
    pub overshoot: bool,
}

impl SpringSettings {
    fn resolve(self, default_duration_ms: f64) -> Result<SpringConfig, String> {
        let duration = self.duration_ms.unwrap_or(default_duration_ms);
        if !duration.is_finite() || duration <= 0.0 {
            return Err("spring duration-ms must be finite and positive".into());
        }
        if !self.bounce.is_finite() || self.bounce <= -1.0 || self.bounce >= 1.0 {
            return Err("spring bounce must be between -1 and 1 (exclusive)".into());
        }
        if self.bounce > 0.0 && !self.overshoot {
            return Err("positive spring bounce requires overshoot".into());
        }
        let ratio = if self.bounce >= 0.0 {
            1.0 - self.bounce
        } else {
            1.0 / (1.0 + self.bounce)
        };
        let omega = std::f64::consts::TAU / (duration / 1000.0);
        let config = SpringConfig {
            mass: 1.0,
            stiffness: omega * omega,
            damping: 2.0 * omega * ratio,
            crossing: if self.overshoot {
                CrossingPolicy::AllowOvershoot
            } else {
                CrossingPolicy::NoCrossing
            },
            ..SpringConfig::default()
        };
        // Guard derived values too: extreme finite inputs can overflow or
        // underflow. Every accepted presentation spring must dissipate motion.
        if !config.stiffness.is_finite()
            || config.stiffness <= 0.0
            || !config.damping.is_finite()
            || config.damping <= 0.0
        {
            return Err("spring duration-ms/bounce produce out-of-range coefficients".into());
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_validation_rejects_extremes_and_accepts_safe_boundaries() {
        for speed in [
            f64::NAN,
            f64::INFINITY,
            0.0,
            -1.0,
            1e-100,
            1e100,
            0.009,
            101.0,
        ] {
            let settings = MotionSettings {
                speed,
                ..Default::default()
            };
            assert!(settings.validate_speed().is_err());
            assert!(settings.validate().is_err());
        }
        for speed in [0.01, 0.5, 1.0, 100.0] {
            let settings = MotionSettings {
                speed,
                ..Default::default()
            };
            assert_eq!(settings.validate_speed().unwrap(), speed);
            assert!(settings.validate().is_ok());
            assert!(!settings.duration(120.0).is_zero());
            assert!(!std::time::Duration::from_secs(60).mul_f64(speed).is_zero());
        }
    }

    #[test]
    fn defaults_are_critical_with_distinct_spatial_and_viewport_responses() {
        let settings = MotionSettings::default();
        for (spring, duration) in [
            (settings.spring_config().unwrap(), 0.240),
            (settings.viewport_config().unwrap(), 0.350),
        ] {
            assert_eq!(spring.mass, 1.0);
            assert!((spring.damping / (2.0 * spring.stiffness.sqrt()) - 1.0).abs() < 1e-12);
            assert!((std::f64::consts::TAU / spring.stiffness.sqrt() - duration).abs() < 1e-12);
            assert_eq!(spring.crossing, CrossingPolicy::NoCrossing);
        }
        settings.validate().unwrap();
    }

    #[test]
    fn perceptual_conversion_covers_under_critical_and_overdamping() {
        for (bounce, ratio) in [(0.0, 1.0), (0.3, 0.7), (-0.5, 2.0)] {
            let spring = SpringSettings {
                duration_ms: Some(500.0),
                bounce,
                overshoot: bounce > 0.0,
            }
            .resolve(240.0)
            .unwrap();
            assert!((spring.stiffness - 157.913670417).abs() < 1e-8);
            assert!((spring.damping / (2.0 * spring.stiffness.sqrt()) - ratio).abs() < 1e-10);
            assert!(
                (std::f64::consts::TAU * (spring.mass / spring.stiffness).sqrt() - 0.5).abs()
                    < 1e-10
            );
        }
    }

    #[test]
    fn rejects_nonsettling_and_out_of_range_settings_for_both_properties() {
        for spring in [
            SpringSettings {
                bounce: 1.0,
                overshoot: true,
                ..Default::default()
            },
            SpringSettings {
                bounce: -1.0,
                ..Default::default()
            },
            SpringSettings {
                bounce: f64::NAN,
                ..Default::default()
            },
            SpringSettings {
                bounce: 0.2,
                ..Default::default()
            },
            SpringSettings {
                duration_ms: Some(0.0),
                ..Default::default()
            },
            SpringSettings {
                duration_ms: Some(-100.0),
                ..Default::default()
            },
            SpringSettings {
                duration_ms: Some(f64::INFINITY),
                ..Default::default()
            },
            SpringSettings {
                duration_ms: Some(f64::MIN_POSITIVE),
                ..Default::default()
            },
            SpringSettings {
                duration_ms: Some(f64::MAX),
                ..Default::default()
            },
        ] {
            assert!(
                MotionSettings {
                    spring,
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
            assert!(
                MotionSettings {
                    viewport_spring: spring,
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
    }

    #[test]
    fn accepted_bounce_retargets_and_settles() {
        let spring = SpringSettings {
            bounce: 0.3,
            overshoot: true,
            ..Default::default()
        }
        .resolve(240.0)
        .unwrap();
        let mut motion = crate::AnimatedValue {
            current: 0.0,
            target: 1.0,
            velocity: 0.0,
        };
        motion.advance(std::time::Duration::from_millis(50), spring);
        let before = (motion.current, motion.velocity);
        motion.set_target(0.0);
        assert_eq!((motion.current, motion.velocity), before);
        motion.advance(std::time::Duration::from_secs(3), spring);
        assert!(!motion.is_animating());
    }

    #[test]
    fn policy_applies_speed_and_reduced_motion_consistently() {
        let slow = MotionSettings {
            speed: 0.5,
            ..Default::default()
        };
        assert_eq!(slow.duration(120.0), std::time::Duration::from_millis(240));
        assert!(
            !MotionSettings {
                reduced_motion: true,
                ..slow
            }
            .motion_enabled()
        );
        assert!(
            MotionSettings {
                reduced_motion: true,
                ..slow
            }
            .duration(120.0)
            .is_zero()
        );
        assert!(MotionSettings { speed: 0.0, ..slow }.validate().is_err());
        assert_eq!(
            slow.spring_config().unwrap(),
            MotionSettings::default().spring_config().unwrap()
        );
    }
}
