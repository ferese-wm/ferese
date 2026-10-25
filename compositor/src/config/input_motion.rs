use super::{
    Config, ConfigError, Deserialize, SpringConfig, default_repeat_delay, default_repeat_rate,
    enabled_by_default,
};

impl Config {
    pub fn input_settings(&self) -> Result<InputSettings, ConfigError> {
        if self.input.xkb_layout.trim().is_empty() {
            return Err(ConfigError::InputValue {
                field: "xkb_layout",
                value: self.input.xkb_layout.clone(),
            });
        }
        if self.input.repeat_rate <= 0 {
            return Err(ConfigError::InputValue {
                field: "repeat_rate",
                value: self.input.repeat_rate.to_string(),
            });
        }
        if self.input.repeat_delay_ms < 0 {
            return Err(ConfigError::InputValue {
                field: "repeat_delay_ms",
                value: self.input.repeat_delay_ms.to_string(),
            });
        }

        if !(16..=1000).contains(&self.input.touchpad.swipe_threshold) {
            return Err(ConfigError::InputValue {
                field: "touchpad.swipe_threshold",
                value: self.input.touchpad.swipe_threshold.to_string(),
            });
        }
        let mouse = self.input.mouse;
        for (field, value, valid) in [
            (
                "touchpad.accel-speed",
                self.input.touchpad.accel_speed,
                (-1.0..=1.0).contains(&self.input.touchpad.accel_speed),
            ),
            (
                "mouse.accel-speed",
                mouse.accel_speed,
                (-1.0..=1.0).contains(&mouse.accel_speed),
            ),
            (
                "mouse.scroll-factor",
                mouse.scroll_factor,
                (0.01..=100.0).contains(&mouse.scroll_factor),
            ),
            (
                "mouse.drag-threshold",
                mouse.drag_threshold,
                (0.0..=100.0).contains(&mouse.drag_threshold),
            ),
        ] {
            if !value.is_finite() || !valid {
                return Err(ConfigError::InputValue {
                    field,
                    value: value.to_string(),
                });
            }
        }
        if mouse.scroll_cooldown_ms > 5000 {
            return Err(ConfigError::InputValue {
                field: "mouse.scroll-cooldown-ms",
                value: mouse.scroll_cooldown_ms.to_string(),
            });
        }
        Ok(InputSettings {
            mouse,
            focus_follows_mouse: self.input.focus_follows_mouse,
            xkb_layout: self.input.xkb_layout.clone(),
            xkb_variant: self.input.xkb_variant.clone(),
            xkb_options: self.input.xkb_options.clone(),
            repeat_rate: self.input.repeat_rate,
            repeat_delay_ms: self.input.repeat_delay_ms,
            touchpad: TouchpadSettings {
                tap: self.input.touchpad.tap,
                accel_speed: self.input.touchpad.accel_speed,
                disable_while_mouse_connected: self.input.touchpad.disable_while_mouse_connected,
                natural_scroll: self.input.touchpad.natural_scroll,
                disable_while_typing: self.input.touchpad.disable_while_typing,
                swipe_threshold: self.input.touchpad.swipe_threshold,
            },
        })
    }

    pub fn animations_enabled(&self) -> bool {
        self.animations.motion_enabled()
    }

    pub fn animation_speed(&self) -> Result<f64, ConfigError> {
        self.animations
            .validate_speed()
            .map_err(|_| ConfigError::AnimationValue {
                field: "speed",
                value: self.animations.speed,
            })
    }

    pub fn spring_config(&self) -> Result<SpringConfig, ConfigError> {
        self.animations
            .spring_config()
            .map_err(ConfigError::Binding)
    }

    pub fn viewport_spring_config(&self) -> Result<SpringConfig, ConfigError> {
        self.animations
            .viewport_config()
            .map_err(ConfigError::Binding)
    }
}

pub(super) fn default_xkb_layout() -> String {
    "us".to_owned()
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputSettings {
    pub focus_follows_mouse: bool,
    pub xkb_layout: String,
    pub xkb_variant: String,
    pub xkb_options: Vec<String>,
    pub repeat_rate: i32,
    pub repeat_delay_ms: i32,
    pub touchpad: TouchpadSettings,
    pub mouse: MouseSettings,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchpadSettings {
    pub tap: bool,
    pub accel_speed: f64,
    pub disable_while_mouse_connected: bool,
    pub natural_scroll: bool,
    pub disable_while_typing: bool,
    pub swipe_threshold: u16,
}

fn default_swipe_threshold() -> u16 {
    80
}

#[derive(Debug, Deserialize)]
pub(super) struct InputConfig {
    #[serde(default)]
    pub(super) focus_follows_mouse: bool,
    #[serde(default = "default_xkb_layout")]
    pub(super) xkb_layout: String,
    #[serde(default)]
    pub(super) xkb_variant: String,
    #[serde(default)]
    pub(super) xkb_options: Vec<String>,
    #[serde(default = "default_repeat_rate")]
    pub(super) repeat_rate: i32,
    #[serde(default = "default_repeat_delay")]
    pub(super) repeat_delay_ms: i32,
    #[serde(default)]
    pub(super) touchpad: TouchpadConfig,
    #[serde(default)]
    pub(super) mouse: MouseSettings,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            focus_follows_mouse: false,
            xkb_layout: default_xkb_layout(),
            xkb_variant: String::new(),
            xkb_options: Vec::new(),
            repeat_rate: default_repeat_rate(),
            repeat_delay_ms: default_repeat_delay(),
            touchpad: TouchpadConfig::default(),
            mouse: MouseSettings::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct TouchpadConfig {
    #[serde(default)]
    pub(super) accel_speed: f64,
    #[serde(default = "enabled_by_default")]
    pub(super) tap: bool,
    #[serde(default)]
    pub(super) disable_while_mouse_connected: bool,
    #[serde(default = "enabled_by_default")]
    pub(super) natural_scroll: bool,
    #[serde(default = "enabled_by_default")]
    pub(super) disable_while_typing: bool,
    #[serde(default = "default_swipe_threshold")]
    pub(super) swipe_threshold: u16,
}

impl Default for TouchpadConfig {
    fn default() -> Self {
        Self {
            tap: true,
            accel_speed: 0.0,
            disable_while_mouse_connected: false,
            natural_scroll: true,
            disable_while_typing: true,
            swipe_threshold: default_swipe_threshold(),
        }
    }
}

pub(super) type AnimationsConfig = ferese_animation::MotionSettings;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_settings_are_separate_from_touchpad_and_validate_ranges() {
        let defaults = Config::parse_source("").unwrap().input_settings().unwrap();
        assert!(!defaults.mouse.natural_scroll);
        assert!(defaults.touchpad.natural_scroll);
        assert!(!defaults.touchpad.disable_while_mouse_connected);
        let config = Config::parse_source(r#"
            input {
                mouse { accel-speed -0.4; accel-profile "flat"; scroll-factor 0.5; left-handed #true; }
                touchpad { disable-while-mouse-connected #true; accel-speed 0.35; }
            }
        "#).unwrap().input_settings().unwrap();
        assert!(config.touchpad.disable_while_mouse_connected);
        assert_eq!(config.touchpad.accel_speed, 0.35);
        assert_eq!(config.mouse.accel_profile, Some(AccelerationProfile::Flat));
        assert_eq!(config.mouse.accel_speed, -0.4);
        assert_eq!(config.mouse.scroll_factor, 0.5);
        assert!(config.mouse.left_handed);
        for speed in [-1.01, 1.01] {
            let config =
                Config::parse_source(&format!("input {{ touchpad {{ accel-speed {speed}; }} }}"))
                    .unwrap();
            assert!(config.input_settings().is_err());
        }
        for (key, value) in [
            ("accel-speed", "1.1"),
            ("scroll-factor", "0"),
            ("drag-threshold", "-1"),
            ("scroll-cooldown-ms", "5001"),
        ] {
            let config =
                Config::parse_source(&format!("input {{ mouse {{ {key} {value}; }} }}")).unwrap();
            assert!(config.input_settings().is_err(), "{key}={value}");
        }
    }

    #[test]
    fn compositor_speed_uses_shared_motion_bounds() {
        for speed in [1e-100, 0.009, 101.0, 1e100] {
            let config =
                Config::parse_source(&format!("animations {{ speed {speed:e}; }}")).unwrap();
            assert!(config.animation_speed().is_err());
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccelerationProfile {
    Adaptive,
    Flat,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MouseSettings {
    pub natural_scroll: bool,
    pub accel_speed: f64,
    pub accel_profile: Option<AccelerationProfile>,
    pub left_handed: bool,
    pub middle_emulation: bool,
    pub scroll_factor: f64,
    pub drag_threshold: f64,
    pub scroll_cooldown_ms: u64,
}

impl Default for MouseSettings {
    fn default() -> Self {
        Self {
            natural_scroll: false,
            accel_speed: 0.0,
            accel_profile: None,
            left_handed: false,
            middle_emulation: false,
            scroll_factor: 1.0,
            drag_threshold: 4.0,
            scroll_cooldown_ms: 150,
        }
    }
}
