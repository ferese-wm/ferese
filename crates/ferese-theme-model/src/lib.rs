//! Resolved theme values and pure color operations. No configuration or runtime access.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
mod color;
pub mod families;
pub use color::*;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    #[default]
    Dark,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Light,
    #[default]
    Dark,
    Auto,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Accessibility {
    pub increase_contrast: bool,
    pub reduce_transparency: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Colors {
    pub surface_base: String,
    pub surface_raised: String,
    pub application_background: String,
    pub text_primary: String,
    pub text_muted: String,
    pub accent: String,
    pub on_accent: String,
    pub border: String,
    pub shadow: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Material {
    pub style: String,
    pub opacity: f64,
    pub blur_radius: f64,
    pub tint_strength: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Geometry {
    pub border_width: f64,
    pub focus_ring_width: f64,
    pub window_radius: f64,
    pub shell_radius: f64,
    pub top_bar_height: f64,
    pub top_bar_margin_top: i32,
    pub top_bar_margin_horizontal: i32,
    pub top_bar_window_gap: i32,
    pub panel_padding: f64,
    pub control_gap: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Typography {
    pub font_family: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Background {
    pub path: Option<PathBuf>,
    pub lock_path: Option<PathBuf>,
    pub mode: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct BarSurface {
    pub background: String,
    pub text_primary: String,
    pub text_muted: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Surfaces {
    pub bar: BarSurface,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SoftShadow {
    pub offset_y: f64,
    pub blur: f64,
    pub opacity: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Shadows {
    pub soft: SoftShadow,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Gradient {
    pub from: String,
    pub to: String,
    pub angle: f64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PaintStyle {
    #[default]
    Auto,
    Solid,
}

impl PaintStyle {
    fn is_auto(&self) -> bool {
        *self == Self::Auto
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Paint {
    #[serde(default, skip_serializing_if = "PaintStyle::is_auto")]
    pub style: PaintStyle,
    pub gradient: Option<Gradient>,
}

impl Paint {
    pub fn effective_gradient(&self) -> Option<&Gradient> {
        if self.style == PaintStyle::Solid {
            None
        } else {
            self.gradient.as_ref()
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Tokens {
    pub colors: Colors,
    pub material: Material,
    pub geometry: Geometry,
    pub typography: Typography,
    pub background: Background,
    pub surface: Surfaces,
    pub shadow: Shadows,
    pub border: Paint,
    pub focus_ring: Paint,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ResolvedTheme {
    pub appearance: Appearance,
    pub tokens: Tokens,
    pub requested_accent: String,
    pub accessibility: Accessibility,
    pub reduced_motion: bool,
}
