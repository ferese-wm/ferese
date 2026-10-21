//! Panel composition, geometry and allocation share stable group and item identities.
mod composition;
pub(crate) mod geometry;
pub mod layout;
mod radius;

pub use composition::*;
pub use geometry::{GeometryIntent, PanelGeometry};
pub use radius::CornerRadii;
