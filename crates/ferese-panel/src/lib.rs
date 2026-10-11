//! Panel composition and allocation without rendering or platform dependencies.
pub mod panel;
pub use panel::*;
pub mod outline;
pub mod scene;
pub mod shape;

pub use shape::Style as PanelStyle;

pub const fn default_island_padding() -> f32 {
    4.0
}
