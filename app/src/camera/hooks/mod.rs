//! Camera hooks that work under any renderer.
//!
//! These mirror `dioxus_cameras::use_devices` / `use_camera_stream`, including
//! their reconnect handling, but publish frames into a [`FrameSlot`] we can
//! actually read instead of into a registry whose only reader is an HTTP server.
//!
//! [`FrameSlot`]: super::FrameSlot

mod channel;
mod devices;
mod status;
mod stream;

// Globs because this is a binary crate: naming a type nothing mentions warns
// as an unused import. Same reason `camera/mod.rs` globs this module.
pub use devices::*;
pub use status::*;
pub use stream::*;
