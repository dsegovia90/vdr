//! Camera capture and GPU presentation.

mod frames;
mod hooks;
mod paint;

pub use frames::FrameSlot;
pub use hooks::*;
pub use paint::CameraPaintSource;
