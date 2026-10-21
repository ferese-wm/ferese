//! Framed IPC messages and the services exposed over compositor connections.
pub mod activities;
mod connection;
pub mod events;
mod frame;
pub mod media;
pub mod night_light;
pub mod socket;
pub mod theme;
pub mod xwayland;

pub use connection::{Cancellation, Connection, socket_path};
pub use frame::*;
