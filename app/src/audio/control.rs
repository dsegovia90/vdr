//! The microphone as the interface sees it: mute, a chosen input, a level, and
//! whether anyone else is talking.
//!
//! Shared directly rather than through a channel. `moq_audio`'s publication
//! handle is `Send + Sync` and its controls only flip a flag that the capture
//! driver picks up, so the interface thread can call them itself and does not
//! need anything carried across for it. What *does* need carrying is the other
//! direction -- a level worth drawing changes faster than anything wants to be
//! pushed a message about -- so that half is polled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use moq_audio::{capture, encode, playback};

use crate::net::runtime::runtime;

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

/// Where the call is heard: the chosen output, and the engine playing into it
/// while a call is up. Separate for the reason [`Mic`] is.
#[derive(Default)]
struct Output {
    /// `None` is the system default.
    device: Option<String>,
    engine: Option<playback::Engine>,
}

/// How loud everyone outside our subroom is played, as the user set it.
///
/// A listener setting: it never crosses the wire, and it applies to every
/// out-of-room peer at once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crosstalk {
    /// `0.0..=1.0`. At `1.0` a subroom is only a label.
    pub volume: f32,
    /// Silent, whatever `volume` says; unmuting returns to it.
    pub muted: bool,
}

impl Default for Crosstalk {
    fn default() -> Self {
        Self {
            // A guess: quiet enough to talk over, loud enough to follow what
            // the main room is doing.
            volume: 0.2,
            muted: false,
        }
    }
}

/// The shared handle: one per process, held by the interface and by the call.
#[derive(Default)]
pub struct Controls {
    mic: Mutex<Mic>,
    output: Mutex<Output>,
    crosstalk: Mutex<Crosstalk>,
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
    /// Whether the microphone is on: the last [`set_enabled`](Self::set_enabled),
    /// whoever called it -- our own button, or a peer's mute request.
    pub fn enabled(&self) -> bool {
        self.mic.lock().map(|mic| mic.enabled).unwrap_or(true)
    }

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

    /// Choose an output by the id [`devices`](super::devices) reports, or
    /// `None` for the system default.
    ///
    /// **Live during a call.** The engine moves to the new device and keeps
    /// every peer's sink, so nobody is reconnected; the echo canceller's
    /// reference follows the engine, not the device.
    pub fn set_output(&self, device: Option<String>) {
        let Ok(mut output) = self.output.lock() else { return };
        if output.device == device {
            return;
        }
        output.device = device.clone();
        let Some(engine) = output.engine.clone() else { return };
        let Some(runtime) = runtime() else { return };
        runtime.spawn(switch_output(engine, device));
    }

    /// The output a new call should open: the user's choice, or `None` for the
    /// system default.
    pub(crate) fn output(&self) -> Option<String> {
        self.output.lock().ok().and_then(|output| output.device.clone())
    }

    /// Hand over the engine a new call just opened, so a choice made during
    /// the call can move it.
    pub(crate) fn attach_output(&self, engine: Option<&playback::Engine>) {
        if let Ok(mut output) = self.output.lock() {
            output.engine = engine.cloned();
        }
    }

    /// The volume a peer outside our subroom plays at: `0.0` while muted.
    ///
    /// Read by every peer's audio task on every frame, so a change here is
    /// heard within one frame.
    pub fn crosstalk_volume(&self) -> f32 {
        let crosstalk = self.crosstalk();
        if crosstalk.muted { 0.0 } else { crosstalk.volume }
    }

    /// The out-of-room volume and mute, for the menu to show.
    pub fn crosstalk(&self) -> Crosstalk {
        self.crosstalk.lock().map(|c| *c).unwrap_or_default()
    }

    /// Sets the out-of-room volume, clamped to `0.0..=1.0`. A non-finite value
    /// is ignored: it would otherwise reach every sink.
    pub fn set_crosstalk_volume(&self, volume: f32) {
        if !volume.is_finite() {
            return;
        }
        if let Ok(mut crosstalk) = self.crosstalk.lock() {
            crosstalk.volume = volume.clamp(0.0, 1.0);
        }
    }

    /// Mutes or unmutes everyone outside our subroom.
    pub fn set_crosstalk_muted(&self, muted: bool) {
        if let Ok(mut crosstalk) = self.crosstalk.lock() {
            crosstalk.muted = muted;
        }
    }

    /// Forget the publication and the engine when their call ends.
    pub(crate) fn detach(&self) {
        if let Ok(mut mic) = self.mic.lock() {
            mic.publication = None;
        }
        if let Ok(mut output) = self.output.lock() {
            output.engine = None;
        }
    }
}

/// Moves a live engine to `device`.
///
/// **A failed switch falls back to the system default.** `moq_audio` leaves the
/// engine on no device at all when a switch fails, and a call that goes silent
/// because a headset was unplugged between the scan and the click is worse
/// than one that plays through the speakers.
async fn switch_output(engine: playback::Engine, device: Option<String>) {
    let mut config = playback::Config::default();
    config.device = device.clone();
    let Err(error) = engine.switch(config).await else {
        tracing::info!("[audio] output switched to {}", device.as_deref().unwrap_or("the system default"));
        return;
    };
    tracing::warn!("[audio] could not switch output to {device:?}: {error:#}");
    if device.is_none() {
        return;
    }
    if let Err(error) = engine.switch(playback::Config::default()).await {
        tracing::warn!("[audio] the system default output failed too, so peers are silent: {error:#}");
    } else {
        tracing::info!("[audio] fell back to the system default output");
    }
}

/// One peer's talking dot, which goes out however their audio task ends.
///
/// **A guard, because a peer's audio task does not get to finish.** A peer
/// leaving aborts the task reading their audio, so nothing written after its
/// loop ever runs -- a flag cleared on the way out stayed on whenever someone
/// left mid-word. Dropping this clears it whichever way the task ends.
pub(crate) struct Speaking {
    /// The flag their tile reads.
    own: Arc<AtomicBool>,
    on: bool,
}

impl Speaking {
    pub(crate) fn new(own: Arc<AtomicBool>) -> Self {
        Self { own, on: false }
    }

    /// Records what this peer's talking window concluded.
    pub(crate) fn set(&mut self, talking: bool) {
        if talking != self.on {
            self.on = talking;
            self.own.store(talking, Ordering::Relaxed);
        }
    }
}

impl Drop for Speaking {
    fn drop(&mut self) {
        self.set(false);
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
#[derive(Debug)]
pub(crate) struct Talking {
    /// Frames left before the indicator goes out.
    hold: u32,
    /// What a frame above threshold resets `hold` to.
    full: u32,
}

impl Default for Talking {
    /// For decoded audio, one push per 20 ms frame: [`HOLD`] frames is 300 ms.
    fn default() -> Self {
        Self { hold: 0, full: HOLD }
    }
}

impl Talking {
    /// For a caller that pushes once every `every`, holding the same 300 ms
    /// as decoded audio does -- the microphone meter samples at its own pace,
    /// and 15 of *its* samples would hold the dot for a second.
    pub(crate) fn every(every: std::time::Duration) -> Self {
        let hold_ms = u128::from(HOLD) * 20;
        let full = (hold_ms / every.as_millis().max(1)).max(1) as u32;
        Self { hold: 0, full }
    }

    /// Adds a frame's loudness and returns whether this reads as talking.
    pub(crate) fn push(&mut self, rms: f32) -> bool {
        if rms >= TALKING {
            self.hold = self.full;
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
    fn crosstalk_starts_quiet_and_unmuted() {
        let controls = Controls::default();
        assert_eq!(controls.crosstalk_volume(), 0.2);
    }

    #[test]
    fn muted_crosstalk_is_silent_whatever_the_volume() {
        let controls = Controls::default();
        controls.set_crosstalk_volume(0.8);
        controls.set_crosstalk_muted(true);
        assert_eq!(controls.crosstalk_volume(), 0.0);
        controls.set_crosstalk_muted(false);
        assert_eq!(controls.crosstalk_volume(), 0.8, "unmuting returns to the volume");
    }

    #[test]
    fn crosstalk_volume_is_clamped() {
        let controls = Controls::default();
        controls.set_crosstalk_volume(1.7);
        assert_eq!(controls.crosstalk_volume(), 1.0);
        controls.set_crosstalk_volume(-0.3);
        assert_eq!(controls.crosstalk_volume(), 0.0);
        controls.set_crosstalk_volume(f32::NAN);
        assert_eq!(controls.crosstalk_volume(), 0.0, "NaN is ignored");
    }

    #[test]
    fn each_peer_has_their_own_dot() {
        let (alice_dot, bob_dot) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let mut alice = Speaking::new(alice_dot.clone());
        let mut bob = Speaking::new(bob_dot.clone());
        alice.set(true);
        bob.set(true);
        alice.set(false);
        assert!(!alice_dot.load(Ordering::Relaxed));
        assert!(bob_dot.load(Ordering::Relaxed), "bob is still talking");
    }

    #[test]
    fn a_peer_leaving_mid_word_turns_their_dot_off() {
        let own = Arc::new(AtomicBool::new(false));
        let mut alice = Speaking::new(own.clone());
        alice.set(true);
        assert!(own.load(Ordering::Relaxed));
        // What aborting their audio task does: the guard drops while on.
        drop(alice);
        assert!(!own.load(Ordering::Relaxed));
    }

    #[test]
    fn a_slower_sampler_holds_the_same_time() {
        // At 66 ms a sample, 300 ms is about 4 samples, not 15.
        let mut talking = Talking::every(std::time::Duration::from_millis(66));
        talking.push(0.1);
        let mut on_for = 0;
        while talking.push(0.0) {
            on_for += 1;
        }
        assert!((3..=5).contains(&on_for), "held for {on_for} samples");
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
