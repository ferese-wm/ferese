use super::ConfigError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum ScrollDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerTrigger {
    Button(u32),
    Wheel(ScrollDirection),
    Touchpad(ScrollDirection),
}

impl PointerTrigger {
    pub(super) fn parse(name: &str) -> Result<Option<Self>, ConfigError> {
        let name = name.to_ascii_lowercase();
        let button = match name.as_str() {
            "mouseleft" => Some(272),
            "mouseright" => Some(273),
            "mousemiddle" => Some(274),
            "mouseside" | "mouseback" => Some(275),
            "mouseextra" | "mouseforward" => Some(276),
            _ => None,
        };
        if let Some(button) = button {
            return Ok(Some(Self::Button(button)));
        }
        if let Some(code) = name.strip_prefix("mouse:") {
            let code = code
                .parse::<u32>()
                .ok()
                .filter(|code| (0x110..=0x11f).contains(code))
                .ok_or_else(|| {
                    ConfigError::Binding(
                        "mouse button must be a Linux mouse button code (272–287)".into(),
                    )
                })?;
            return Ok(Some(Self::Button(code)));
        }
        for (prefix, touchpad) in [
            ("wheelscroll", false),
            ("wheel", false),
            ("touchpadscroll", true),
        ] {
            if let Some(direction) = name.strip_prefix(prefix) {
                let direction = match direction {
                    "left" => ScrollDirection::Left,
                    "right" => ScrollDirection::Right,
                    "up" => ScrollDirection::Up,
                    "down" => ScrollDirection::Down,
                    _ => {
                        return Err(ConfigError::Binding(format!(
                            "invalid scroll direction {direction:?}"
                        )));
                    }
                };
                return Ok(Some(if touchpad {
                    Self::Touchpad(direction)
                } else {
                    Self::Wheel(direction)
                }));
            }
        }
        Ok(None)
    }

    pub(super) fn key(self) -> u32 {
        match self {
            Self::Button(code) => code,
            Self::Wheel(direction) => 1024 + direction as u32,
            Self::Touchpad(direction) => 1028 + direction as u32,
        }
    }

    pub(super) fn from_key(key: u32) -> Self {
        let directions = [
            ScrollDirection::Left,
            ScrollDirection::Right,
            ScrollDirection::Up,
            ScrollDirection::Down,
        ];
        match key {
            1024..=1027 => Self::Wheel(directions[(key - 1024) as usize]),
            1028..=1031 => Self::Touchpad(directions[(key - 1028) as usize]),
            code => Self::Button(code),
        }
    }

    pub(super) fn label(self) -> String {
        match self {
            Self::Button(272) => "Mouse Left".into(),
            Self::Button(273) => "Mouse Right".into(),
            Self::Button(274) => "Mouse Middle".into(),
            Self::Button(275) => "Mouse Back".into(),
            Self::Button(276) => "Mouse Forward".into(),
            Self::Button(code) => format!("Mouse {code}"),
            Self::Wheel(direction) => format!("Wheel {direction:?}"),
            Self::Touchpad(direction) => format!("Touchpad scroll {direction:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BindingAction, Config};

    #[test]
    fn pointer_names_round_trip_without_colliding_with_keyboard_keys() {
        for name in [
            "MouseLeft",
            "MouseBack",
            "MouseForward",
            "mouse:287",
            "WheelUp",
            "WheelScrollLeft",
            "TouchpadScrollDown",
        ] {
            let trigger = PointerTrigger::parse(name).unwrap().unwrap();
            assert_eq!(PointerTrigger::from_key(trigger.key()), trigger);
        }
        for name in ["mouse:12", "mouse:288", "mouse:foo", "WheelSideways"] {
            assert!(PointerTrigger::parse(name).is_err(), "{name}");
        }
        assert_eq!(PointerTrigger::parse("Return").unwrap(), None);
    }

    #[test]
    fn pointer_defaults_can_be_overridden_and_disabled() {
        let config = Config::parse_source(
            r#"
            binding "Super+MouseLeft" action="close"
            binding "Super+MouseRight" disabled=#true
            binding "Alt+MouseMiddle" action="pointer-resize"
            binding "Super+WheelDown" action="workspace-next"
        "#,
        )
        .unwrap();
        let bindings = config.bindings(&config.input_settings().unwrap()).unwrap();
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.action == BindingAction::PointerMove)
        );
        assert_eq!(
            bindings
                .iter()
                .filter(|binding| binding.action == BindingAction::PointerResize)
                .count(),
            1
        );
        let config =
            Config::parse_source(r#"binding "Super+WheelUp" action="pointer-move""#).unwrap();
        assert!(config.bindings(&config.input_settings().unwrap()).is_err());
    }
}
