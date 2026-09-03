//! The hand-off point between the capture thread and the renderer.

use std::sync::{Arc, Mutex};

use cameras::Frame;

/// Holds the newest captured frame for the paint source to pick up.
///
/// The capture pump overwrites the slot on every frame and the renderer drains
/// it, so frames are dropped whenever the renderer falls behind. That is the
/// behaviour we want for a live preview: only the most recent image matters.
///
/// This is the piece `dioxus-cameras` keeps private. Its `Registry` exposes
/// `publish_frame` but no public reader, which is why the crate's frames can
/// only be retrieved over its loopback HTTP server.
#[derive(Clone, Default)]
pub struct FrameSlot(Arc<Mutex<Option<Frame>>>);

impl FrameSlot {
    /// Replace the pending frame. Called from the capture thread.
    pub fn publish(&self, frame: Frame) {
        *self.lock() = Some(frame);
    }

    /// Take the pending frame, leaving the slot empty. Called from the renderer.
    pub fn take(&self) -> Option<Frame> {
        self.lock().take()
    }

    /// Drop any pending frame, so a stale image does not outlive its source.
    pub fn clear(&self) {
        *self.lock() = None;
    }

    /// A poisoned slot only means a capture thread panicked mid-publish; the
    /// frame it held is still structurally fine to replace.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Frame>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for FrameSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameSlot")
            .field("pending", &self.lock().is_some())
            .finish()
    }
}
