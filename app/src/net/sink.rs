//! Where decoded frames land, whichever pipeline produced them.

use moq_media::subscribe::VideoTrack;

use crate::camera::FrameSlot;

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
        slot.publish(frame);
    }
    // The producer is gone, so the last frame must come off the screen rather
    // than sit there looking live.
    slot.clear();
}
