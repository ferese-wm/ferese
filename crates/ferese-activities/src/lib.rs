//! Typed activity data and deterministic policy. The host supplies verified owners
//! and suspend-aware milliseconds; this crate performs no I/O or rendering.
pub mod pomodoro;
mod store;

pub use store::*;

#[cfg(test)]
mod tests;
