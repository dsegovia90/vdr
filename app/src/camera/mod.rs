//! Camera capture and GPU presentation.

mod format;
mod frames;
mod hooks;
mod paint;

pub use frames::FrameSlot;
pub use hooks::*;
pub use paint::{Mirror, VideoPaintSource};
