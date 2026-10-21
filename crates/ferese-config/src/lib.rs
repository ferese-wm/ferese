//! KDL documents, validation and persisted configuration values.
pub mod activities;
mod document;
pub mod families;
pub mod file;
pub mod night_light;
pub mod notifications;
pub mod presets;
pub mod theme;

pub use document::*;
pub use ferese_panel::panel;
pub use ferese_panel::{PanelStyle, default_island_padding};
