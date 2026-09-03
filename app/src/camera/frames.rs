//! The hand-off point between the capture thread and the renderer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use moq_media::format::{PixelFormat, VideoFrame};

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
    frame: Arc<Mutex<Option<VideoFrame>>>,
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
    pub fn publish(&self, frame: VideoFrame) {
        *self.lock() = Some(frame);
        self.published.fetch_add(1, Ordering::Relaxed);
    }

    /// Take the pending frame, leaving the slot empty. Called from the renderer.
    pub fn take(&self) -> Option<VideoFrame> {
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
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<VideoFrame>> {
        self.frame.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Identity, not contents: two handles to the same slot are the same stream.
/// Comparing the pending frame would be both expensive and meaningless.
impl PartialEq for FrameSlot {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.frame, &other.frame)
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

/// Converts a captured frame into the neutral frame type the slot carries.
///
/// The slot cannot hold `cameras::Frame`: it is `#[non_exhaustive]`, so a
/// decoder could never construct one, and remote video has to reach the same
/// renderer as local capture. `moq_media`'s `VideoFrame` is what the decoder
/// produces, so it is the type both ends can meet on.
///
/// Free of copies in the common case -- both sides are `Bytes` -- except when
/// the camera pads its rows, which `VideoFrame` has no way to express.
pub fn from_capture(frame: &cameras::Frame) -> VideoFrame {
    let packed = frame.width * 4;
    let pixel_format = match frame.pixel_format {
        cameras::PixelFormat::Bgra8 => Some(PixelFormat::Bgra),
        cameras::PixelFormat::Rgba8 => Some(PixelFormat::Rgba),
        _ => None,
    };

    if let Some(pixel_format) = pixel_format {
        let data = if frame.stride == packed {
            frame.plane_primary.clone()
        } else {
            // Padded rows: repack, since `VideoFrame` assumes tight packing and
            // would otherwise shear every row after the first.
            let stride = frame.stride as usize;
            let mut tight = Vec::with_capacity((packed * frame.height) as usize);
            for row in 0..frame.height as usize {
                let start = row * stride;
                tight.extend_from_slice(&frame.plane_primary[start..start + packed as usize]);
            }
            Bytes::from(tight)
        };
        return VideoFrame::new_packed(data, frame.width, frame.height, pixel_format, frame.timestamp);
    }

    // NV12, YUYV, MJPEG and friends: the CPU decoder is the only way through.
    match cameras::to_rgba8(frame) {
        Ok(rgba) => VideoFrame::new_packed(
            Bytes::from(rgba),
            frame.width,
            frame.height,
            PixelFormat::Rgba,
            frame.timestamp,
        ),
        Err(error) => {
            tracing::warn!("frame conversion failed: {error}");
            VideoFrame::new_packed(Bytes::new(), 0, 0, PixelFormat::Rgba, frame.timestamp)
        }
    }
}
