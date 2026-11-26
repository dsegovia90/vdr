//! The timestamps the camera's encode loop publishes under, kept rising.
//!
//! **A track's timeline only goes forward.** moq-mux refuses a frame stamped
//! below what its track has already reached (`TimestampRewind`), and its own
//! advice for a source that has to re-anchor, a capture restart, is to raise
//! the stamp to that edge rather than rewind. The encode loop keeps one
//! producer for as long as the camera's stream lasts, across the camera being
//! turned off and on, so a capture whose clock starts again would have every
//! frame after the restart refused. The test pattern's clock did that, and
//! the call ended over it; a camera driver could do the same.
//!
//! So the stamps are carried on an offset: a frame stamped below the last one
//! moves the offset up so that it lands one frame after it, and every frame
//! after keeps that offset, so the timeline rises from there at the capture's
//! own pace. Nothing is fixed one frame at a time, which would leave the next
//! frame below the edge again.
//!
//! **A refusal is a frame's, not the call's.** If the track still refuses a
//! frame (it had reached further than we knew), that frame is skipped and
//! the stamps jump on; a frame refused for not following a keyframe, which is
//! what comes after a refused keyframe, is skipped until the encoder makes
//! one.

/// A frame's length at the rate we encode at, in microseconds: how far past
/// the last frame a re-anchored one lands.
const FRAME_US: u64 = 1_000_000 / super::call::FRAMERATE as u64;

/// How far the stamps jump when the track refuses one anyway: past anything
/// it can have reached that we did not see, at the cost of a second's gap in
/// a timeline nobody watches as one.
const JUMP_US: u64 = 1_000_000;

/// The offset a capture's stamps are published under.
#[derive(Debug, Default)]
pub(super) struct Rising {
    offset: u64,
    last: Option<u64>,
}

impl Rising {
    /// The stamp, in microseconds, to publish a frame captured at `raw` under.
    pub(super) fn stamp(&mut self, raw: u64) -> u64 {
        let mut at = raw.saturating_add(self.offset);
        if let Some(last) = self.last {
            if at < last {
                at = last.saturating_add(FRAME_US);
                self.offset = at - raw;
            }
        }
        self.last = Some(at);
        at
    }

    /// The track refused a stamp below what it had reached: move every later
    /// one past it.
    pub(super) fn refused(&mut self) {
        self.offset = self.offset.saturating_add(JUMP_US);
    }
}

/// Why the track turned a frame away, when it is the frame's fault and not
/// the track's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// Stamped below what the track had already reached.
    Rewind,
    /// Not a keyframe, with no group open for it: the keyframe before it was
    /// refused.
    NoKeyframe,
}

/// Whether a publish error turned away one frame, leaving the track usable.
/// Anything else is the track's own failure and ends the stream.
pub(super) fn refusal(error: &moq_video::Error) -> Option<Refusal> {
    match error {
        moq_video::Error::Mux(moq_mux::Error::TimestampRewind(_)) => Some(Refusal::Rewind),
        moq_video::Error::Mux(moq_mux::Error::MissingKeyframe(_)) => Some(Refusal::NoKeyframe),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rising_capture_is_published_as_it_is() {
        let mut rising = Rising::default();
        let stamps: Vec<u64> = [0, 33_333, 66_666, 100_000].iter().map(|&raw| rising.stamp(raw)).collect();
        assert_eq!(stamps, [0, 33_333, 66_666, 100_000]);
    }

    #[test]
    fn a_restarted_capture_carries_on_one_frame_after_the_last() {
        let mut rising = Rising::default();
        for raw in [5_000_000, 5_033_333, 5_066_666] {
            rising.stamp(raw);
        }
        // The camera off and on: its clock starts again near zero.
        assert_eq!(rising.stamp(0), 5_066_666 + FRAME_US);
        // And the offset stays, so the next frame does not fall back below.
        assert_eq!(rising.stamp(33_333), 5_066_666 + FRAME_US + 33_333);
        assert_eq!(rising.stamp(66_666), 5_066_666 + FRAME_US + 66_666);
    }

    #[test]
    fn every_restart_keeps_the_stamps_rising() {
        let mut rising = Rising::default();
        let mut last = None;
        for run in 0..4u64 {
            for n in 0..10u64 {
                let at = rising.stamp(n * 33_333 + run * 7);
                assert!(last.is_none_or(|last| at > last), "run {run} frame {n} went back to {at} from {last:?}");
                last = Some(at);
            }
        }
    }

    #[test]
    fn a_later_clock_needs_no_offset() {
        let mut rising = Rising::default();
        rising.stamp(1_000_000);
        // Off for a minute on a clock that kept running: nothing to move.
        assert_eq!(rising.stamp(61_000_000), 61_000_000);
    }

    #[test]
    fn a_refusal_moves_every_later_stamp_on() {
        let mut rising = Rising::default();
        rising.stamp(1_000_000);
        rising.refused();
        assert_eq!(rising.stamp(1_033_333), 1_033_333 + JUMP_US);
        assert_eq!(rising.stamp(1_066_666), 1_066_666 + JUMP_US);
    }

    #[test]
    fn only_a_frames_own_refusal_is_skipped() {
        let rewind = moq_video::Error::Mux(moq_mux::Error::TimestampRewind(moq_mux::container::TimestampRewind));
        let keyless = moq_video::Error::Mux(moq_mux::Error::MissingKeyframe(moq_mux::container::MissingKeyframe));
        assert_eq!(refusal(&rewind), Some(Refusal::Rewind));
        assert_eq!(refusal(&keyless), Some(Refusal::NoKeyframe));
        assert_eq!(refusal(&moq_video::Error::Net(moq_net::Error::Cancel)), None);
        assert_eq!(refusal(&moq_video::Error::Mux(moq_mux::Error::Moq(moq_net::Error::Cancel))), None);
    }
}
