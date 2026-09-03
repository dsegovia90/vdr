//! The hand-off point between the capture thread and the renderer.

use std::sync::atomic::{AtomicU64, Ordering};
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
pub struct FrameSlot {
    frame: Arc<Mutex<Option<Frame>>>,
    /// Total frames ever published. Compared against the renderer's own count
    /// to tell "the camera is slow" apart from "we are dropping frames".
    published: Arc<AtomicU64>,
    /// Bumped on every [`FrameSlot::clear`]. Lets the renderer tell "no new
    /// frame yet, keep showing the last one" apart from "the source went away,
    /// stop showing it" -- an empty slot alone cannot distinguish the two.
    cleared: Arc<AtomicU64>,
}

impl FrameSlot {
    /// Replace the pending frame. Called from the capture thread.
    pub fn publish(&self, frame: Frame) {
        *self.lock() = Some(frame);
        self.published.fetch_add(1, Ordering::Relaxed);
    }

    /// Take the pending frame, leaving the slot empty. Called from the renderer.
    pub fn take(&self) -> Option<Frame> {
        self.lock().take()
    }

    /// Drop any pending frame, so a stale image does not outlive its source.
    pub fn clear(&self) {
        *self.lock() = None;
        self.cleared.fetch_add(1, Ordering::Relaxed);
    }

    /// How many times this slot has been cleared. Only changes are meaningful:
    /// a reader that sees a new value should discard whatever it was showing.
    pub fn cleared(&self) -> u64 {
        self.cleared.load(Ordering::Relaxed)
    }

    /// How many frames the camera has delivered since this slot was created.
    ///
    /// Monotonic across camera switches; only differences are meaningful.
    pub fn published(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }

    /// A poisoned slot only means a capture thread panicked mid-publish; the
    /// frame it held is still structurally fine to replace.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Frame>> {
        self.frame.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for FrameSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameSlot")
            .field("pending", &self.lock().is_some())
            .field("published", &self.published())
            .finish()
    }
}
