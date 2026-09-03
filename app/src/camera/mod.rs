//! Camera capture and GPU presentation.

mod format;
mod frames;
mod hooks;
mod paint;

pub use frames::FrameSlot;
pub use hooks::*;
// Glob for the same reason `hooks` is globbed: naming a type nothing happens
// to mention warns as an unused import in a binary crate.
pub use paint::*;
