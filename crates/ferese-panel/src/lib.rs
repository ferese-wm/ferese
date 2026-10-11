//! Panel composition and allocation without rendering or platform dependencies.
pub mod panel;
pub use panel::*;
pub mod shape;

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PanelPreset {
    #[default]
    Continuous,
    Islands,
}

pub const fn default_island_padding() -> f32 {
    4.0
}
