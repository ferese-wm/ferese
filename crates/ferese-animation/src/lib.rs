//! Spring-driven geometry and client presentation coordination.
pub mod gesture;
mod settings;
mod spring;
mod window_geometry;

pub use settings::{MotionSettings, SpringSettings};
pub use window_geometry::*;
