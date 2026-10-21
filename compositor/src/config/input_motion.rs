use super::*;

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
        Ok(InputSettings {
            focus_follows_mouse: self.input.focus_follows_mouse,
            xkb_layout: self.input.xkb_layout.clone(),
            xkb_variant: self.input.xkb_variant.clone(),
            xkb_options: self.input.xkb_options.clone(),
            repeat_rate: self.input.repeat_rate,
            repeat_delay_ms: self.input.repeat_delay_ms,
            touchpad: TouchpadSettings {
                tap: self.input.touchpad.tap,
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
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchpadSettings {
    pub tap: bool,
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
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct TouchpadConfig {
    #[serde(default = "enabled_by_default")]
    pub(super) tap: bool,
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
    fn compositor_speed_uses_shared_motion_bounds() {
        for speed in [1e-100, 0.009, 101.0, 1e100] {
            let config =
                Config::parse_source(&format!("animations {{ speed {speed:e}; }}")).unwrap();
            assert!(config.animation_speed().is_err());
        }
    }
}
