//! The hand-off point between the capture thread and the renderer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use std::time::Duration;

use bytes::Bytes;

/// How a frame's pixels are laid out.
///
/// Two shapes, because the two producers genuinely differ and flattening them
/// would cost real work:
///
/// - **Packed** is what `cameras` delivers. It uploads straight into a
///   `Bgra8Unorm`/`Rgba8Unorm` texture with no conversion at all, which is the
///   preview's fast path and worth preserving exactly.
/// - **I420** is what a decoder produces. `moq_video::Surface` has *no* packed
///   CPU variant -- only `I420` plus platform GPU handles -- so adopting
///   `moq_video::Frame` as the slot's type would mean a CPU BGRA->I420
///   conversion on every captured frame purely to satisfy a type.
#[derive(Clone)]
pub enum Pixels {
    /// Tightly packed BGRA8, `width * 4` bytes per row.
    Bgra(Bytes),
    /// Tightly packed RGBA8, `width * 4` bytes per row.
    Rgba(Bytes),
    /// Planar 4:2:0: `width * height` luma, then two `(width/2) * (height/2)`
    /// chroma planes, in one buffer. `moq_video::Surface::into_i420` hands it
    /// over exactly this way.
    I420(Bytes),
}

/// The neutral frame both producers meet on.
///
/// The slot cannot hold `cameras::Frame` (it is `#[non_exhaustive]`, so a
/// decoder could never build one), and it cannot hold `moq_video::Frame`
/// (see [`Pixels`]). So it holds this.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub timestamp: Duration,
    pub pixels: Pixels,
}

impl Frame {
    /// Bytes an I420 buffer must have for these dimensions.
    pub fn i420_len(width: u32, height: u32) -> usize {
        let (w, h) = (width as usize, height as usize);
        w * h + 2 * ((w / 2) * (h / 2))
    }

    /// Converts to the planar surface an encoder wants.
    ///
    /// **Not via `moq_video::Surface::rgba`.** That is not a constructor -- it
    /// calls `I420::from_rgba` immediately -- and it assumes RGBA channel
    /// order, so BGRA input silently comes out with red and blue swapped.
    /// `I420::from_nv12` is `pub(crate)` upstream, so the conversion is ours
    /// either way.
    ///
    /// Rows are already tightly packed here: `from_capture` repacks a padded
    /// camera frame before it ever reaches this type.
    pub fn to_i420_surface(&self) -> Option<moq_video::Surface> {
        let (w, h) = (self.width as usize, self.height as usize);
        if w < 2 || h < 2 {
            return None;
        }
        // Odd dimensions have no well-defined 4:2:0 chroma plane.
        let (cw, ch) = (w / 2, h / 2);

        let data = match &self.pixels {
            Pixels::I420(data) => data.to_vec(),
            Pixels::Bgra(src) | Pixels::Rgba(src) => {
                if src.len() < w * h * 4 {
                    return None;
                }
                let (ri, bi) = match self.pixels {
                    Pixels::Bgra(_) => (2usize, 0usize),
                    _ => (0usize, 2usize),
                };
                let at = |x: usize, y: usize| -> (f32, f32, f32) {
                    let p = (y * w + x) * 4;
                    (src[p + ri] as f32, src[p + 1] as f32, src[p + bi] as f32)
                };

                // BT.601 limited range, the inverse of `preview.wgsl`'s I420
                // branch. Keep the two in step or colours drift.
                let mut out = vec![0u8; Self::i420_len(self.width, self.height)];
                for y in 0..h {
                    for x in 0..w {
                        let (r, g, b) = at(x, y);
                        out[y * w + x] =
                            (0.257 * r + 0.504 * g + 0.098 * b + 16.0).clamp(0.0, 255.0) as u8;
                    }
                }
                let (u_off, v_off) = (w * h, w * h + cw * ch);
                for cy in 0..ch {
                    for cx in 0..cw {
                        // Averaged, not point sampled: point sampling shimmers.
                        let (mut rs, mut gs, mut bs) = (0.0, 0.0, 0.0);
                        for dy in 0..2 {
                            for dx in 0..2 {
                                let (r, g, b) = at(cx * 2 + dx, cy * 2 + dy);
                                rs += r;
                                gs += g;
                                bs += b;
                            }
                        }
                        let (r, g, b) = (rs / 4.0, gs / 4.0, bs / 4.0);
                        out[u_off + cy * cw + cx] =
                            (-0.148 * r - 0.291 * g + 0.439 * b + 128.0).clamp(0.0, 255.0) as u8;
                        out[v_off + cy * cw + cx] =
                            (0.439 * r - 0.368 * g - 0.071 * b + 128.0).clamp(0.0, 255.0) as u8;
                    }
                }
                out
            }
        };

        match moq_video::I420::new(self.width, self.height, data) {
            Ok(i420) => Some(moq_video::Surface::I420(i420)),
            Err(error) => {
                tracing::warn!("I420 conversion failed: {error}");
                None
            }
        }
    }
}

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
/// Free of copies in the common case -- both sides are `Bytes` -- except when
/// the camera pads its rows, which the packed variants cannot express.
pub fn from_capture(frame: &cameras::Frame) -> Frame {
    let packed_row = frame.width * 4;
    let packed = match frame.pixel_format {
        cameras::PixelFormat::Bgra8 => Some(true),
        cameras::PixelFormat::Rgba8 => Some(false),
        _ => None,
    };

    if let Some(is_bgra) = packed {
        let data = if frame.stride == packed_row {
            frame.plane_primary.clone()
        } else {
            // Repack: the packed variants assume tight rows and would
            // otherwise shear every row after the first.
            let stride = frame.stride as usize;
            let mut tight = Vec::with_capacity((packed_row * frame.height) as usize);
            for row in 0..frame.height as usize {
                let start = row * stride;
                tight.extend_from_slice(&frame.plane_primary[start..start + packed_row as usize]);
            }
            Bytes::from(tight)
        };
        return Frame {
            width: frame.width,
            height: frame.height,
            timestamp: frame.timestamp,
            pixels: if is_bgra { Pixels::Bgra(data) } else { Pixels::Rgba(data) },
        };
    }

    // NV12, YUYV, MJPEG and friends: the CPU decoder is the only way through.
    // NV12 is what both cameras natively offer and what we would rather have,
    // but `cameras` hardcodes BGRA on macOS and ignores the request.
    match cameras::to_rgba8(frame) {
        Ok(rgba) => Frame {
            width: frame.width,
            height: frame.height,
            timestamp: frame.timestamp,
            pixels: Pixels::Rgba(Bytes::from(rgba)),
        },
        Err(error) => {
            tracing::warn!("frame conversion failed: {error}");
            Frame {
                width: 0,
                height: 0,
                timestamp: frame.timestamp,
                pixels: Pixels::Rgba(Bytes::new()),
            }
        }
    }
}

/// Converts a decoded frame into the neutral type.
///
/// `Surface::into_i420` is a CPU readback on macOS, where the decoder hands
/// back an IOSurface-backed `PixelBuffer`. Importing that handle into a wgpu
/// texture directly would avoid the round trip, at the cost of unsafe
/// per-platform interop.
pub fn from_decoded(frame: moq_video::Frame) -> Option<Frame> {
    let width = frame.surface.width();
    let height = frame.surface.height();
    let timestamp = Duration::from_micros(frame.timestamp.as_micros() as u64);
    match frame.surface.into_i420() {
        Ok(data) => Some(Frame {
            width,
            height,
            timestamp,
            pixels: Pixels::I420(data),
        }),
        Err(error) => {
            tracing::warn!("decoded frame conversion failed: {error}");
            None
        }
    }
}

/// The encoder's end of the pump fan-out.
///
/// [`FrameSlot`] cannot serve both consumers: `take()` is destructive and
/// single-consumer, so a slot shared with an encoder would have the two
/// stealing frames from each other, each seeing roughly half, with nothing
/// raising an error. The two also fail differently, which is why this is a
/// separate type rather than a second handle on the same one.
///
/// **Newest-wins, and only one frame deep.** The obvious alternative -- a short
/// queue -- is actively wrong here, and the way it is wrong is worth recording.
/// A bounded queue drops the *newest* frame when it is full, so a consumer that
/// stops reading leaves the queue holding the oldest frames it ever saw. The
/// consumer is parked for exactly that long: `moq-media` does not pull from a
/// publisher until a peer subscribes to it. It then resumes on frames from
/// before the app finished starting, and because it paces on their presentation
/// timestamps, it sleeps off every second of the gap. A call placed a minute
/// after launch published at about two frames per second.
///
/// Overwriting instead means the consumer always resumes on the present, and
/// matches what `rusty-capture`'s own sources document: `pop_frame` "always
/// returns the most recent frame, avoiding latency buildup".
///
/// Cloning a [`Frame`] in is a refcount bump on `Bytes`, not a pixel copy.
pub struct FrameTap {
    shared: Arc<Tapped>,
}

/// The reading end of a [`FrameTap`], owned by whatever encodes.
pub struct FrameFeed {
    shared: Arc<Tapped>,
}

/// The single frame in flight, and what the reader needs to know about it.
#[derive(Default)]
struct Tapped {
    frame: Mutex<Option<Frame>>,
    /// Wakes [`FrameFeed::wait`], which blocks before an encoder can be built.
    arrived: std::sync::Condvar,
    /// Live [`FrameTap`]s. At zero no frame can ever arrive again, which is
    /// what ends the encoder pipeline downstream.
    taps: AtomicU64,
    /// Frames overwritten before the encoder read them. Only interesting as a
    /// rate: a steady count means the encoder is slower than the camera.
    dropped: AtomicU64,
}

/// Creates a fan-out tap and its reader.
pub fn frame_tap() -> (FrameTap, FrameFeed) {
    let shared = Arc::new(Tapped::default());
    shared.taps.store(1, Ordering::Relaxed);
    (
        FrameTap {
            shared: Arc::clone(&shared),
        },
        FrameFeed { shared },
    )
}

impl FrameTap {
    /// Offers a frame to the encoder, replacing any it has not read yet.
    ///
    /// Never blocks: this runs on the capture thread, and stalling there would
    /// stall the preview too -- the failure the fan-out exists to avoid.
    pub fn publish(&self, frame: Frame) {
        let replaced = self.shared.lock().replace(frame).is_some();
        if replaced {
            let dropped = self.shared.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            // Every 30 is roughly one line per second of sustained overload,
            // rather than one per dropped frame.
            if dropped.is_multiple_of(30) {
                tracing::debug!("encoder tap has passed over {dropped} frames");
            }
        }
        self.shared.arrived.notify_all();
    }
}

/// Counted by hand rather than derived, because the reader distinguishes "no
/// frame yet" from "no tap left" and only the count can tell it which.
impl Clone for FrameTap {
    fn clone(&self) -> Self {
        self.shared.taps.fetch_add(1, Ordering::Relaxed);
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for FrameTap {
    fn drop(&mut self) {
        if self.shared.taps.fetch_sub(1, Ordering::AcqRel) == 1 {
            // The last one: anything blocked in `wait` will never be fed.
            self.shared.arrived.notify_all();
        }
    }
}

impl FrameFeed {
    /// Takes the pending frame, if there is one.
    ///
    /// `Ok(None)` means "nothing new yet, ask again"; an error means every
    /// [`FrameTap`] is gone and no frame will ever arrive. That distinction is
    /// what shuts an encoder pipeline down -- it treats the error as the end of
    /// its source and exits, which in turn closes everything downstream.
    pub fn take(&mut self) -> anyhow::Result<Option<Frame>> {
        match self.shared.lock().take() {
            Some(frame) => Ok(Some(frame)),
            // Ordering matters: a frame published just before the last tap
            // dropped is still worth encoding, so the emptiness is checked
            // first and the closure only after.
            None if self.shared.taps.load(Ordering::Acquire) == 0 => {
                Err(anyhow::anyhow!("capture tap closed"))
            }
            None => Ok(None),
        }
    }

    /// Blocks until a frame is pending, or until every tap is dropped.
    ///
    /// Needed because a source's pixel format and dimensions are only knowable
    /// from a frame: on macOS the device overrides the resolution we asked for
    /// (see `format.rs`), so the request is not evidence of anything.
    pub fn wait(&mut self) -> Option<Frame> {
        let mut pending = self.shared.lock();
        loop {
            if let Some(frame) = pending.take() {
                return Some(frame);
            }
            if self.shared.taps.load(Ordering::Acquire) == 0 {
                return None;
            }
            pending = self
                .shared
                .arrived
                .wait(pending)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Tapped {
    /// A poisoned tap only means a thread panicked mid-publish; the frame it
    /// held is still structurally fine to replace.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Frame>> {
        self.frame.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tap_tests {
    use std::time::Duration;

    use super::*;

    fn frame(width: u32) -> Frame {
        Frame {
            width,
            height: 2,
            timestamp: Duration::ZERO,
            pixels: Pixels::Bgra(Bytes::new()),
        }
    }

    #[test]
    fn an_unread_frame_is_replaced_not_kept() {
        // The bug this type exists to prevent: a full queue rejects the new
        // frame, so a consumer that parks resumes on the oldest frames it ever
        // saw and paces itself off their timestamps.
        let (tap, mut feed) = frame_tap();
        tap.publish(frame(1));
        tap.publish(frame(2));
        tap.publish(frame(3));
        let taken = feed.take().unwrap().expect("a frame is pending");
        assert_eq!(taken.width, 3, "the newest, not the first");
        assert!(feed.take().unwrap().is_none(), "only ever one frame deep");
    }

    #[test]
    fn an_empty_tap_is_not_a_closed_one() {
        let (_tap, mut feed) = frame_tap();
        assert!(
            feed.take().unwrap().is_none(),
            "nothing yet is not the end of the source"
        );
    }

    #[test]
    fn a_dropped_tap_closes_the_feed() {
        let (tap, mut feed) = frame_tap();
        drop(tap);
        assert!(feed.take().is_err());
        assert!(
            feed.wait().is_none(),
            "waiting for a frame that cannot come"
        );
    }

    #[test]
    fn a_frame_published_before_the_close_still_arrives() {
        let (tap, mut feed) = frame_tap();
        tap.publish(frame(1));
        drop(tap);
        assert!(feed.take().unwrap().is_some(), "the frame comes first");
        assert!(feed.take().is_err(), "and the close after");
    }

    #[test]
    fn wait_returns_a_frame_published_before_it() {
        let (tap, mut feed) = frame_tap();
        tap.publish(frame(7));
        assert_eq!(feed.wait().expect("published").width, 7);
    }

    #[test]
    fn wait_wakes_on_a_later_publish() {
        let (tap, mut feed) = frame_tap();
        let publisher = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            tap.publish(frame(9));
        });
        assert_eq!(feed.wait().expect("published late").width, 9);
        publisher.join().unwrap();
    }
}
