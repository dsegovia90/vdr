//! A camera that is not one: generated frames, for scripted tests
//! (`--test-pattern`).
//!
//! **It enters where a camera's frames do**, the slot the local tile paints
//! and the tap the encode thread reads, so everything after that point -- the
//! encoder, rate control, publishing, a viewer's decoder -- runs exactly as it
//! does for a real camera. Nothing before that point is exercised: no device
//! is enumerated for it, opened, or asked for permission, which is the point.
//! Several test instances on one machine then never touch the camera of the
//! person at it, and never change its format under an app that is using it.
//!
//! **The picture moves everywhere, every frame.** A still or a slowly changing
//! picture compresses to almost nothing after the first keyframe, so rate
//! control, bitrate and decode figures from it would say little about a call.
//! A sliding colour field, a block crossing it, and the frame number in large
//! digits (so a frozen or skipping viewer can be told from a person's glance
//! at two tiles) keep the encoder working on something that changes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::{Frame, FrameSlot, FrameTap, Pixels};

/// The size a pattern is drawn at: 360p, the middle rung's height, so the
/// default quality encodes it without scaling.
pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 360;

/// Frames a second, as the camera is asked for.
pub const FPS: u32 = 30;

/// What the device menu and the stream status call it.
pub const LABEL: &str = "Test pattern";

/// Draws frame `n` of the pattern at `width` x `height`, stamped `timestamp`.
///
/// BGRA, as the macOS camera delivers, so the encode path takes its usual
/// conversion.
pub fn frame(width: u32, height: u32, n: u64, timestamp: Duration) -> Frame {
    let (w, h) = (width as usize, height as usize);
    let mut pixels = vec![0u8; w * h * 4];
    // The field slides four pixels a frame, the block eight.
    let shift = (n as usize).wrapping_mul(4);
    let side = (h / 4).max(2);
    let span = w.saturating_sub(side).max(1);
    let block_x = (n as usize * 8) % span;
    let block_y = h.saturating_sub(side) / 2;
    for y in 0..h {
        for x in 0..w {
            let at = (y * w + x) * 4;
            let along = (x + shift) % 512;
            // A triangle wave per channel, offset from each other, so the
            // field is colour everywhere and never one flat area.
            let wave = |offset: usize| {
                let t = (along + offset) % 512;
                (if t < 256 { t } else { 511 - t }) as u8
            };
            let (mut r, mut g, mut b) = (wave(0), wave(170), ((y * 255) / h.max(1)) as u8);
            if x >= block_x && x < block_x + side && y >= block_y && y < block_y + side {
                (r, g, b) = (255, 255, 255);
            }
            pixels[at] = b;
            pixels[at + 1] = g;
            pixels[at + 2] = r;
            pixels[at + 3] = 255;
        }
    }
    number(&mut pixels, w, h, n);
    Frame {
        width,
        height,
        timestamp,
        pixels: Pixels::Bgra(Bytes::from(pixels)),
    }
}

/// Draws `n` in the top-left corner, black on white, big enough to read
/// after a 3x downscale.
fn number(pixels: &mut [u8], w: usize, h: usize, n: u64) {
    // Three by five cells per digit, a row of bits each, top first.
    const DIGITS: [[u8; 5]; 10] = [
        [0b111, 0b101, 0b101, 0b101, 0b111],
        [0b010, 0b110, 0b010, 0b010, 0b111],
        [0b111, 0b001, 0b111, 0b100, 0b111],
        [0b111, 0b001, 0b111, 0b001, 0b111],
        [0b101, 0b101, 0b111, 0b001, 0b001],
        [0b111, 0b100, 0b111, 0b001, 0b111],
        [0b111, 0b100, 0b111, 0b101, 0b111],
        [0b111, 0b001, 0b010, 0b010, 0b010],
        [0b111, 0b101, 0b111, 0b101, 0b111],
        [0b111, 0b101, 0b111, 0b001, 0b111],
    ];
    let cell = (h / 48).max(1);
    let text = n.to_string();
    // A white plate behind the digits, one cell of margin all round.
    let plate_w = (text.len() * 4 + 1) * cell;
    let plate_h = 7 * cell;
    let paint = |pixels: &mut [u8], x: usize, y: usize, value: u8| {
        if x < w && y < h {
            let at = (y * w + x) * 4;
            pixels[at..at + 3].fill(value);
        }
    };
    for y in 0..plate_h {
        for x in 0..plate_w {
            paint(pixels, x, y, 255);
        }
    }
    for (i, digit) in text.bytes().enumerate() {
        let rows = DIGITS[(digit - b'0') as usize];
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..3 {
                if bits & (0b100 >> col) == 0 {
                    continue;
                }
                let (x0, y0) = ((1 + i * 4 + col) * cell, (1 + row) * cell);
                for y in y0..y0 + cell {
                    for x in x0..x0 + cell {
                        paint(pixels, x, y, 0);
                    }
                }
            }
        }
    }
}

/// The pattern running on a thread of its own, at [`FPS`], into `slot` and
/// `tap` as a camera's pump feeds them. Stops when dropped.
pub struct Running {
    live: Arc<AtomicBool>,
}

impl Drop for Running {
    /// Effective before it returns, as a camera's `silence` is: the thread
    /// may draw one more frame, but publishes nothing after this. Never
    /// joined, so the interface thread never waits on it.
    fn drop(&mut self) {
        self.live.store(false, Ordering::Relaxed);
    }
}

/// What every run of the pattern stamps its frames from, for the life of the
/// process.
///
/// **Stamps keep rising across runs, as a camera's clock does.** Turning the
/// camera off and on starts a new run, but the call keeps publishing on the
/// same track, which refuses a frame stamped below what it has already
/// reached; a run stamping from its own start sent the stamps back to zero,
/// and the call ended on the refusal.
fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Starts the pattern. Each run counts from frame zero, and stamps from
/// [`epoch`].
pub fn spawn(slot: FrameSlot, tap: Option<FrameTap>) -> std::io::Result<Running> {
    let live = Arc::new(AtomicBool::new(true));
    let running = live.clone();
    std::thread::Builder::new().name("camera-pattern".into()).spawn(move || {
        let epoch = epoch();
        let start = Instant::now();
        let every = Duration::from_secs(1) / FPS;
        for n in 0u64.. {
            // Paced against the start rather than by sleeping a frame's
            // length after each, so drawing time does not slow the rate.
            let due = start + every * n as u32;
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            let frame = frame(WIDTH, HEIGHT, n, epoch.elapsed());
            if !running.load(Ordering::Relaxed) {
                return;
            }
            if let Some(tap) = &tap {
                tap.publish(frame.clone());
            }
            slot.publish(frame);
        }
    })?;
    Ok(Running { live })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(frame: &Frame) -> &[u8] {
        match &frame.pixels {
            Pixels::Bgra(bytes) => bytes,
            _ => panic!("the pattern is BGRA"),
        }
    }

    #[test]
    fn a_frame_is_tightly_packed_bgra_of_the_size_asked() {
        let frame = frame(WIDTH, HEIGHT, 0, Duration::ZERO);
        assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));
        assert_eq!(bytes(&frame).len(), (WIDTH * HEIGHT * 4) as usize);
        assert!(bytes(&frame).chunks(4).all(|pixel| pixel[3] == 255), "opaque");
    }

    #[test]
    fn every_frame_differs_from_the_last() {
        let a = frame(320, 180, 7, Duration::ZERO);
        let b = frame(320, 180, 8, Duration::ZERO);
        let changed = bytes(&a).chunks(4).zip(bytes(&b).chunks(4)).filter(|(a, b)| a != b).count();
        // Most of the picture, not a corner: the field moves everywhere.
        let pixels = (320 * 180) as usize;
        assert!(changed > pixels * 9 / 10, "only {changed} of {pixels} pixels changed");
    }

    #[test]
    fn it_converts_for_the_encoder_as_a_camera_frame_does() {
        let frame = frame(WIDTH, HEIGHT, 3, Duration::from_millis(100));
        assert!(frame.to_i420_surface(1).is_some());
        assert!(frame.to_i420_surface(2).is_some());
    }

    #[test]
    fn a_later_run_stamps_after_an_earlier_one() {
        let before = epoch().elapsed();
        std::thread::sleep(Duration::from_millis(5));
        // As a second run would: the base is the process's, not the run's.
        assert!(epoch().elapsed() > before);
        assert_eq!(epoch(), epoch());
    }

    #[test]
    fn an_odd_or_tiny_size_still_draws() {
        for (w, h) in [(1, 1), (3, 5), (17, 9)] {
            let frame = frame(w, h, 12345, Duration::ZERO);
            assert_eq!(bytes(&frame).len(), (w * h * 4) as usize);
        }
    }
}
