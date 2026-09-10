//! The microphone as the interface sees it: mute, a chosen input, a level, and
//! whether the other person is talking.
//!
//! Shared directly rather than through a channel. `moq_audio`'s publication
//! handle is `Send + Sync` and its controls only flip a flag that the capture
//! driver picks up, so the interface thread can call them itself and does not
//! need anything carried across for it. What *does* need carrying is the other
//! direction -- a level worth drawing changes faster than anything wants to be
//! pushed a message about -- so that half is polled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use moq_audio::{capture, encode};

/// Loudness, as RMS on a linear scale, above which a frame counts as speech.
///
/// Speech at ordinary levels sits an order of magnitude above this; a quiet room
/// arriving from a sender that runs noise suppression sits well below it. The
/// gap between the two is wide, which is why a plain threshold is enough and no
/// calibration is offered.
const TALKING: f32 = 0.02;

/// How many frames the indicator stays on after the last one above threshold.
///
/// Frames are 20 ms, so this is 300 ms. It exists because speech is not
/// continuous: the gaps between words and either side of a plosive fall below
/// any threshold, and an indicator that followed them exactly would flicker
/// through every sentence.
const HOLD: u32 = 15;

/// What the microphone is doing.
///
/// Ours rather than `moq_audio`'s, because the interface needs one state the
/// crate does not have -- there is no publication at all between calls -- and
/// because muted is a thing the user did, not a thing the device is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MicStatus {
    /// No call, so nothing is published.
    #[default]
    Idle,
    /// Muted. The device is released, so the hardware light goes out.
    Muted,
    /// Published and waiting for someone to listen. The device is not open:
    /// capture is on demand, so an unheard microphone is never opened at all.
    Waiting,
    /// Opening the device.
    Starting,
    /// Capturing and publishing.
    Live,
    /// The input is unavailable. Retried automatically.
    Failed,
}

impl MicStatus {
    /// Whether this should read as a problem rather than as progress.
    pub fn is_error(self) -> bool {
        self == Self::Failed
    }
}

impl std::fmt::Display for MicStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "Microphone idle"),
            Self::Muted => write!(f, "Muted"),
            Self::Waiting => write!(f, "Microphone ready"),
            Self::Starting => write!(f, "Opening the microphone..."),
            Self::Live => write!(f, "Microphone live"),
            Self::Failed => write!(f, "Microphone unavailable"),
        }
    }
}

/// What the user has asked for, plus the publication currently honouring it.
///
/// The two are separate because the intent outlives any one call: muting while
/// idle has to still be true when the next call starts, and the publication is
/// built fresh each attempt.
struct Mic {
    enabled: bool,
    device: Option<String>,
    publication: Option<encode::Publication>,
}

impl Default for Mic {
    fn default() -> Self {
        Self {
            // Unmuted, like every other calling app. A call that starts silent
            // and says so only in a control the user has not looked at yet is a
            // call that starts with "you're on mute".
            enabled: true,
            device: None,
            publication: None,
        }
    }
}

/// The shared handle: one per process, held by the interface and by the call.
#[derive(Default)]
pub struct Controls {
    mic: Mutex<Mic>,
    peer_talking: AtomicBool,
}

impl PartialEq for Controls {
    /// Identity, never contents.
    ///
    /// Dioxus wants this so it can skip re-rendering a component whose props
    /// have not changed. Comparing contents would be both wrong and useless
    /// here: there is exactly one of these per process, so the answer is always
    /// "the same one", while its contents change fifty times a second without
    /// the prop having changed at all. What is drawn from those contents is
    /// sampled into a signal instead.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Controls {
    /// Mute or unmute.
    ///
    /// Muting releases the device rather than publishing silence, so the track
    /// and its catalog rendition survive: the peer's decoder stays open and
    /// simply stops being fed, and the microphone light goes out, which is the
    /// only honest way to show a mute.
    pub fn set_enabled(&self, enabled: bool) {
        let Ok(mut mic) = self.mic.lock() else { return };
        mic.enabled = enabled;
        let Some(publication) = mic.publication.as_ref() else {
            return;
        };
        if enabled {
            publication.start();
        } else {
            publication.stop();
        }
    }

    /// Choose an input by the id [`devices`](super::devices) reports, or `None`
    /// for the system default.
    ///
    /// Swapping does not change the track: the publication keeps its identity
    /// across a replace, so the peer hears a new microphone rather than a new
    /// participant.
    pub fn set_device(&self, device: Option<String>) {
        let Ok(mut mic) = self.mic.lock() else { return };
        if mic.device == device {
            return;
        }
        mic.device = device.clone();
        if let Some(publication) = mic.publication.as_ref() {
            publication.replace(capture::Source::Microphone(device));
        }
    }

    /// Post-processing loudness of the most recent buffer, `0.0..=1.0`.
    ///
    /// Measured after echo cancellation and noise suppression, so it is what the
    /// peer will actually hear rather than what the room is doing. Zero while
    /// the input is closed, which includes muted and between calls.
    pub fn level(&self) -> f32 {
        self.mic
            .lock()
            .ok()
            .and_then(|mic| mic.publication.as_ref().map(|p| p.level().rms()))
            .unwrap_or(0.0)
    }

    /// What the microphone is doing.
    pub fn status(&self) -> MicStatus {
        let Ok(mic) = self.mic.lock() else {
            return MicStatus::Idle;
        };
        let Some(publication) = mic.publication.as_ref() else {
            return MicStatus::Idle;
        };
        match publication.state().status() {
            encode::Status::Stopped => MicStatus::Muted,
            encode::Status::Waiting => MicStatus::Waiting,
            encode::Status::Starting => MicStatus::Starting,
            encode::Status::Live => MicStatus::Live,
            encode::Status::Failed => MicStatus::Failed,
            // `Ended` means the track closed, which from here is the same as
            // there being no call. Non-exhaustive upstream, so this also catches
            // whatever is added next.
            _ => MicStatus::Idle,
        }
    }

    /// Whether the peer is talking.
    pub fn peer_talking(&self) -> bool {
        self.peer_talking.load(Ordering::Relaxed)
    }

    /// Hand over the publication a new call just created, and apply the intent
    /// that was set while there was nothing to apply it to.
    pub(crate) fn attach(&self, publication: encode::Publication) {
        let Ok(mut mic) = self.mic.lock() else { return };
        if let Some(device) = mic.device.clone() {
            publication.replace(capture::Source::Microphone(Some(device)));
        }
        if mic.enabled {
            publication.start();
        } else {
            publication.stop();
        }
        mic.publication = Some(publication);
    }

    /// Forget the publication when its call ends.
    pub(crate) fn detach(&self) {
        if let Ok(mut mic) = self.mic.lock() {
            mic.publication = None;
        }
        self.peer_talking.store(false, Ordering::Relaxed);
    }

    /// Record what the talking window concluded.
    pub(crate) fn set_peer_talking(&self, talking: bool) {
        self.peer_talking.store(talking, Ordering::Relaxed);
    }
}

/// Turns decoded audio into a stable answer to "is that person talking".
///
/// **Measured from the samples, not from the codec.** Opus can mark the frames
/// a sender withheld while silent, and reading that is free -- no decoding, no
/// threshold, and it works on senders that are not us. We used it, and it cost
/// more than it saved: getting the mark requires the *sender* to enable
/// discontinuous transmission, and doing that put an audible knock in the
/// stream roughly twice a second through every silence, as Opus refreshed the
/// decoder's comfort-noise model. A talking dot is not worth a sound.
///
/// It was also never quite right. The mark says what the sender withheld, which
/// is only the same thing as silence when the sender chose to withhold at all --
/// so a peer with that setting off read as talking for as long as they were
/// connected, and there was nothing a receiver could do about it. Loudness has
/// no such asymmetry: it is a property of the audio rather than of how the
/// other end was configured.
#[derive(Debug, Default)]
pub(crate) struct Talking {
    /// Frames left before the indicator goes out.
    hold: u32,
}

impl Talking {
    /// Adds a frame's loudness and returns whether this reads as talking.
    pub(crate) fn push(&mut self, rms: f32) -> bool {
        if rms >= TALKING {
            self.hold = HOLD;
        } else {
            self.hold = self.hold.saturating_sub(1);
        }
        self.hold > 0
    }
}

/// Loudness of one decoded frame, as RMS on a linear scale.
///
/// The bytes are interleaved `f32`, which is what the consumer is built to emit;
/// a trailing partial sample cannot occur, and `chunks_exact` would ignore one
/// anyway. Channels are summed rather than handled separately: the question is
/// whether anyone is talking, not which side of the stereo field they are on.
pub(crate) fn loudness(samples: &[u8]) -> f32 {
    let mut sum = 0.0f64;
    let mut count = 0u32;
    for chunk in samples.as_chunks::<4>().0 {
        let sample = f32::from_le_bytes(*chunk);
        sum += (sample as f64) * (sample as f64);
        count += 1;
    }
    if count == 0 {
        return 0.0;
    }
    (sum / count as f64).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(frames: impl IntoIterator<Item = f32>) -> bool {
        let mut talking = Talking::default();
        let mut last = false;
        for rms in frames {
            last = talking.push(rms);
        }
        last
    }

    fn pcm(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn speech_reads_as_talking() {
        assert!(run(std::iter::repeat_n(0.1, 10)));
    }

    #[test]
    fn a_quiet_room_reads_as_quiet() {
        assert!(!run(std::iter::repeat_n(0.001, 100)));
    }

    /// The gaps between words fall below any threshold, and must not flicker
    /// the indicator off mid-sentence.
    #[test]
    fn a_gap_between_words_does_not_read_as_stopping() {
        let mut talking = Talking::default();
        for _ in 0..20 {
            talking.push(0.1);
        }
        // Shorter than the hold: still talking.
        let mut last = true;
        for _ in 0..HOLD - 1 {
            last = talking.push(0.0);
        }
        assert!(last, "a pause shorter than the hold is still talking");
    }

    #[test]
    fn talking_goes_out_once_the_hold_expires() {
        let mut talking = Talking::default();
        talking.push(0.1);
        let mut last = true;
        for _ in 0..HOLD {
            last = talking.push(0.0);
        }
        assert!(!last);
    }

    #[test]
    fn silence_never_starts_the_indicator() {
        let mut talking = Talking::default();
        assert!(!talking.push(0.0), "no audio must not read as talking");
    }

    #[test]
    fn loudness_of_silence_is_zero() {
        assert_eq!(loudness(&pcm(&[0.0, 0.0, 0.0, 0.0])), 0.0);
    }

    #[test]
    fn loudness_is_rms_not_peak() {
        // A full-scale sample among three silent ones is a quarter of the
        // energy, so RMS is 0.5 -- not the 1.0 a peak meter would report.
        let measured = loudness(&pcm(&[1.0, 0.0, 0.0, 0.0]));
        assert!((measured - 0.5).abs() < 1e-6, "got {measured}");
    }

    #[test]
    fn loudness_ignores_sign() {
        let positive = loudness(&pcm(&[0.5, 0.5]));
        let alternating = loudness(&pcm(&[0.5, -0.5]));
        assert!((positive - alternating).abs() < 1e-6);
    }

    #[test]
    fn loudness_of_nothing_is_zero_rather_than_a_division_by_zero() {
        assert_eq!(loudness(&[]), 0.0);
    }
}
