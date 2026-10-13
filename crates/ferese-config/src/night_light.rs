use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub temperature: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            temperature: 4500,
        }
    }
}

impl Settings {
    pub fn validate(self) -> Result<Self, String> {
        if !(1000..=6500).contains(&self.temperature) {
            return Err("night-light temperature must be between 1000 and 6500 K".into());
        }
        Ok(self)
    }
}
