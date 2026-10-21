pub mod adaptive;
pub mod morph;
pub mod motion;
pub mod panel;
pub use panel::*;
pub mod outline;
pub mod scene;
pub mod shape;

pub use shape::Style as PanelStyle;

#[must_use]
pub const fn default_island_padding() -> f32 {
    4.0
}
