use serde::{Deserialize, Serialize};

use crate::{Appearance, Tokens};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Palette {
    pub background: String,
    pub bar: String,
    pub surface: String,
    pub text: String,
    pub muted: String,
    pub accent: String,
}

impl From<&Tokens> for Palette {
    fn from(tokens: &Tokens) -> Self {
        Self {
            background: tokens.colors.surface_base.clone(),
            bar: tokens.surface.bar.background.clone(),
            surface: tokens.colors.surface_raised.clone(),
            text: tokens.colors.text_primary.clone(),
            muted: tokens.colors.text_muted.clone(),
            accent: tokens.colors.accent.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Family {
    pub id: String,
    pub name: String,
    pub light: Option<Palette>,
    pub dark: Option<Palette>,
}

impl Family {
    #[must_use]
    pub fn palette(&self, appearance: Appearance) -> Option<&Palette> {
        match appearance {
            Appearance::Light => self.light.as_ref(),
            Appearance::Dark => self.dark.as_ref(),
        }
    }

    #[must_use]
    pub fn availability(&self) -> &'static str {
        match (self.light.is_some(), self.dark.is_some()) {
            (true, true) => "light and dark",
            (true, false) => "light only",
            _ => "dark only",
        }
    }
}
