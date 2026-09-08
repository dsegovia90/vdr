//! Where decoded frames land, whichever pipeline produced them.

use moq_media::subscribe::VideoTrack;

use crate::camera::{Frame, FrameSlot, Pixels};

/// Bridges a `moq-media` decoded frame into the neutral frame type.
///
/// **Temporary**, for the same reason as `source.rs`'s counterpart: it serves
/// `call.rs` until that moves to the new stack. `rgba_image()` is the CPU
/// planar->RGBA conversion the paint path used to do inline, kept here so the
/// call path behaves exactly as it did before.
fn from_moq_media(frame: moq_media::format::VideoFrame) -> Frame {
    let (width, height) = (frame.width(), frame.height());
    let timestamp = frame.timestamp;
    let pixels = match &frame.data {
        moq_media::format::FrameData::Packed {
            pixel_format: moq_media::format::PixelFormat::Bgra,
            data,
        } => Pixels::Bgra(data.clone()),
        moq_media::format::FrameData::Packed {
            pixel_format: moq_media::format::PixelFormat::Rgba,
            data,
        } => Pixels::Rgba(data.clone()),
        _ => Pixels::Rgba(bytes::Bytes::from(frame.rgba_image().as_raw().clone())),
    };
    Frame { width, height, timestamp, pixels }
}

/// Publishes every decoded frame into `slot` until the track closes.
///
/// The same loop serves the loopback and a call, which is the point: by this
/// stage a frame carries no trace of how it arrived, and the renderer has never
/// known either.
///
/// `next_frame` already returns only the newest decoded frame, dropping any
/// that queued behind it, so this cannot build latency by falling behind --
/// the same newest-wins contract [`FrameSlot`] has.
pub(super) async fn drain(mut track: VideoTrack, slot: FrameSlot) {
    while let Some(frame) = track.next_frame().await {
        slot.publish(from_moq_media(frame));
    }
    // The producer is gone, so the last frame must come off the screen rather
    // than sit there looking live.
    slot.clear();
}
