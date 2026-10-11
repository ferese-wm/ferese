use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Audio {
    #[default]
    Off,
    System,
    Microphone,
    Both,
}

impl Audio {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::System => "System sound",
            Self::Microphone => "Microphone",
            Self::Both => "Both",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Recording {
    pub(crate) audio: Audio,
    pub(crate) crop: Option<Crop>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Crop {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
}

impl Crop {
    pub(crate) fn validate(self) -> Result<(), String> {
        if [self.left, self.top, self.right, self.bottom]
            .iter()
            .any(|value| *value < 0)
            || self.left.checked_add(self.right).is_none()
            || self.top.checked_add(self.bottom).is_none()
        {
            return Err("Invalid recording crop".into());
        }
        Ok(())
    }
}
