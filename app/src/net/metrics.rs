//! What the connection is doing, for the person on it to see.
//!
//! Everything here was already being computed and then thrown at the log, where
//! it is no use to anyone who is not running the app from a terminal with
//! `RUST_LOG` set. None of it costs a measurement of its own: the rates are
//! counters the decode loops already keep, and the connection figures come from
//! a struct QUIC maintains whether or not we read it.
//!
//! Shared as atomics rather than messaged. Several threads write single numbers
//! -- the call thread, the rate loop, both decode tasks -- and one reader
//! samples the lot a couple of times a second. A channel per number would be
//! more machinery than the numbers are worth, and a lock would put the decode
//! loops behind the interface's sampling.
//!
//! **Zero means "not known yet"** throughout, which is why the getters return
//! `Option`. A connection reports nothing until it has something to report, and
//! showing a confident `0 ms` in the meantime would be a lie about the link
//! rather than an absence of information.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

/// The echo canceller's grade of itself, from sonora's statistics, in the
/// units a person reads: dB and ms.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AecStats {
    /// Echo-return-loss enhancement: how much the canceller removes, in dB.
    /// Under ~10 dB, a far end hears itself.
    pub erle_db: Option<f64>,
    /// Echo return loss: how much quieter the echo already was than what
    /// played, in dB. Low means loud speakers close to the mic.
    pub erl_db: Option<f64>,
    /// Where it has aligned the speaker signal against the microphone, in ms.
    /// A value that keeps moving is the sign of a mic and speaker on
    /// different clocks.
    pub delay_ms: Option<i32>,
    /// What it was told the speakers played over the last second: frames
    /// (about 100 when playback runs) and their RMS level. Zero frames means
    /// it is cancelling against nothing.
    pub reference_frames: Option<u32>,
    pub reference_rms: Option<f32>,
}

impl AecStats {
    /// From sonora's statistics and the patch's reference counter.
    ///
    /// The delay is sonora's `delay_ms`, the one it fills; its median and
    /// standard-deviation fields, and the residual-echo and divergence ones,
    /// stay empty in this configuration (no echo detector), so they are left
    /// out rather than printed as "?".
    pub fn new(stats: &moq_audio::aec::AudioProcessingStats, reference: Option<(u32, f32)>) -> Self {
        Self {
            erle_db: stats.echo_return_loss_enhancement,
            erl_db: stats.echo_return_loss,
            delay_ms: stats.delay_ms,
            reference_frames: reference.map(|(frames, _)| frames),
            reference_rms: reference.map(|(_, rms)| rms),
        }
    }
}

impl std::fmt::Display for AecStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let db = |v: Option<f64>| v.map_or("?".to_string(), |v| format!("{v:.1} dB"));
        write!(
            f,
            "erle {}, erl {}, delay {} ms, speaker reference {} frames/s at rms {}",
            db(self.erle_db),
            db(self.erl_db),
            self.delay_ms.map_or("?".to_string(), |v| v.to_string()),
            self.reference_frames.map_or("?".to_string(), |v| v.to_string()),
            self.reference_rms.map_or("?".to_string(), |v| format!("{v:.4}")),
        )
    }
}

/// Live numbers for the current call.
#[derive(Default)]
pub struct Metrics {
    /// The echo canceller's latest grade of itself, if it has given one.
    aec: std::sync::Mutex<Option<AecStats>>,
    rtt_ms: AtomicU32,
    /// Loss in hundredths of a percent, so a tenth of a percent survives.
    loss_bp: AtomicU32,
    send_kbps: AtomicU32,
    estimate_kbps: AtomicU32,
    width: AtomicU32,
    height: AtomicU32,
}

impl Metrics {
    /// Round-trip time to the peer.
    pub fn rtt(&self) -> Option<u32> {
        nonzero(&self.rtt_ms)
    }

    /// Packet loss as a percentage.
    pub fn loss(&self) -> Option<f32> {
        nonzero(&self.loss_bp).map(|bp| bp as f32 / 100.0)
    }

    /// What we are currently encoding at.
    pub fn send_kbps(&self) -> Option<u32> {
        nonzero(&self.send_kbps)
    }

    /// What the transport thinks the link will carry.
    pub fn estimate_kbps(&self) -> Option<u32> {
        nonzero(&self.estimate_kbps)
    }

    /// The size we encode at, which is not the size the camera captures.
    pub fn encode_size(&self) -> Option<(u32, u32)> {
        Some((nonzero(&self.width)?, nonzero(&self.height)?))
    }

    pub(super) fn set_encode_size(&self, width: u32, height: u32) {
        self.width.store(width, Ordering::Relaxed);
        self.height.store(height, Ordering::Relaxed);
    }

    pub(super) fn set_send_kbps(&self, kbps: u32) {
        self.send_kbps.store(kbps, Ordering::Relaxed);
    }

    pub(super) fn set_estimate_kbps(&self, kbps: Option<u32>) {
        self.estimate_kbps.store(kbps.unwrap_or(0), Ordering::Relaxed);
    }

    /// Records a connection snapshot.
    ///
    /// Loss is cumulative over the connection rather than recent, because that
    /// is what QUIC counts. It answers "has this link been dropping packets",
    /// not "is it dropping them now" -- a call that struggled for ten seconds
    /// and recovered keeps a non-zero figure, which is the honest reading of a
    /// total.
    pub(super) fn set_connection(&self, stats: &moq_net::ConnectionStats) {
        if let Some(rtt) = stats.rtt {
            self.rtt_ms.store(rtt.as_millis() as u32, Ordering::Relaxed);
        }
        if let (Some(lost), Some(sent)) = (stats.packets_lost, stats.packets_sent) {
            if sent > 0 {
                let bp = (lost as f64 / sent as f64 * 10_000.0).round() as u32;
                self.loss_bp.store(bp, Ordering::Relaxed);
            }
        }
    }

    /// The echo canceller's latest numbers.
    pub fn aec(&self) -> Option<AecStats> {
        self.aec.lock().ok().and_then(|aec| *aec)
    }

    pub(super) fn set_aec(&self, stats: AecStats) {
        if let Ok(mut aec) = self.aec.lock() {
            *aec = Some(stats);
        }
    }

    /// Forgets everything, for when a call ends.
    pub(super) fn clear(&self) {
        if let Ok(mut aec) = self.aec.lock() {
            *aec = None;
        }
        for field in [
            &self.rtt_ms,
            &self.loss_bp,
            &self.send_kbps,
            &self.estimate_kbps,
            &self.width,
            &self.height,
        ] {
            field.store(0, Ordering::Relaxed);
        }
    }
}

impl PartialEq for Metrics {
    /// Identity, never contents -- see `audio::Controls` for why.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

fn nonzero(field: &AtomicU32) -> Option<u32> {
    match field.load(Ordering::Relaxed) {
        0 => None,
        value => Some(value),
    }
}

/// Live numbers for one participant, as their tile shows them.
///
/// **Per participant, because the call-wide figure was a lie in a room.** Each
/// peer's decoder wrote the same incoming-fps number, so the screen showed
/// whichever had written last -- a stalled peer was invisible behind a healthy
/// one. Everything here is about one stream; what is about the link as a whole
/// stays in [`Metrics`].
pub struct PeerStats {
    /// Incoming video, in tenths of a frame per second, or [`UNKNOWN`] before
    /// the first measurement. **Not zero for unknown**, unlike [`Metrics`]: a
    /// stalled stream measures zero, and that is exactly the number a tile has
    /// to be able to show.
    fps_tenths: AtomicU32,
    /// Whether they are talking. Shared with the audio task's `Speaking` guard,
    /// which owns turning it off however that task ends.
    talking: Arc<AtomicBool>,
    /// Whether their camera is on, as they say in their presence document --
    /// or, for our own tile, as we set it. On until told otherwise.
    camera_on: AtomicBool,
    /// Whether their microphone is on, the same way.
    mic_on: AtomicBool,
    /// How their camera clock maps to wall-clock time, as they last said.
    /// For our own entry, the mapping we say. See [`Clock`].
    clock: std::sync::Mutex<Option<Clock>>,
    /// Delays of their frames this second, in ms, for the stats file.
    delays: std::sync::Mutex<Vec<u32>>,
    /// Audio frames decoded this second.
    audio_frames: AtomicU32,
    /// Gaps of more than [`AUDIO_GAP`] between their decoded audio frames, ever.
    audio_gaps: AtomicU32,
    /// Times their playback buffer was found empty while frames still came.
    audio_dry: AtomicU32,
}

/// "Camera time `media_us` was wall-clock time `wall_us`" -- what lets another
/// machine read a frame's timestamp, whose epoch is the sender's camera's.
///
/// Frame timestamps are the capturing camera's own clock, meaningless on any
/// other machine. A sender says this pairing in its presence document; a
/// receiver then turns each frame's timestamp into the wall-clock moment it was
/// captured, and the difference from its own clock is the delay. That only
/// holds while both machines' clocks agree, which NTP keeps to within a few ms
/// -- the measurement is only as good as that, and says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Clock {
    pub wall_us: u64,
    pub media_us: u64,
}

impl Clock {
    /// Pairs a camera timestamp with the wall clock now.
    pub fn now(media_us: u64) -> Self {
        Self {
            wall_us: wall_us(),
            media_us,
        }
    }

    /// How long ago, by our own wall clock, a frame stamped `media_us` was
    /// captured. `None` when the answer is negative beyond clock noise -- the
    /// clocks disagree, and a delay of less than nothing is not a number.
    pub fn delay_ms(&self, media_us: u64, now_wall_us: u64) -> Option<u32> {
        let captured = self.wall_us as i128 + media_us as i128 - self.media_us as i128;
        let delay = (now_wall_us as i128 - captured) / 1000;
        u32::try_from(delay).ok()
    }
}

/// Microseconds since the Unix epoch, by this machine's wall clock.
pub fn wall_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_micros() as u64)
}

/// A pause between audio frames that a listener would hear: three missing 20 ms
/// frames, minus some scheduling slack.
pub const AUDIO_GAP: Duration = Duration::from_millis(60);

/// No measurement yet.
const UNKNOWN: u32 = u32::MAX;

impl PartialEq for PeerStats {
    /// Identity, never contents -- see `audio::Controls` for why.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Default for PeerStats {
    fn default() -> Self {
        Self {
            fps_tenths: AtomicU32::new(UNKNOWN),
            talking: Arc::default(),
            camera_on: AtomicBool::new(true),
            mic_on: AtomicBool::new(true),
            clock: std::sync::Mutex::new(None),
            delays: std::sync::Mutex::new(Vec::new()),
            audio_frames: AtomicU32::new(0),
            audio_gaps: AtomicU32::new(0),
            audio_dry: AtomicU32::new(0),
        }
    }
}

impl PeerStats {
    /// Their video, as it arrives: `None` before the first second is up, and
    /// `Some(0.0)` for a stream that has stalled.
    pub fn video_fps(&self) -> Option<f32> {
        match self.fps_tenths.load(Ordering::Relaxed) {
            UNKNOWN => None,
            tenths => Some(tenths as f32 / 10.0),
        }
    }

    /// Whether they are talking right now.
    pub fn talking(&self) -> bool {
        self.talking.load(Ordering::Relaxed)
    }

    /// Whether their camera is on.
    pub fn camera_on(&self) -> bool {
        self.camera_on.load(Ordering::Relaxed)
    }

    /// Whether their microphone is on.
    pub fn mic_on(&self) -> bool {
        self.mic_on.load(Ordering::Relaxed)
    }

    /// Records whether their camera is on.
    pub fn set_camera(&self, on: bool) {
        self.camera_on.store(on, Ordering::Relaxed);
    }

    /// Records whether their microphone is on.
    pub fn set_mic(&self, on: bool) {
        self.mic_on.store(on, Ordering::Relaxed);
    }

    /// Their latest camera-to-wall-clock mapping, if they have said one.
    pub fn clock(&self) -> Option<Clock> {
        self.clock.lock().ok().and_then(|clock| *clock)
    }

    /// Records a camera-to-wall-clock mapping.
    pub fn set_clock(&self, clock: Clock) {
        if let Ok(mut slot) = self.clock.lock() {
            *slot = Some(clock);
        }
    }

    /// Notes a frame of theirs stamped `media_us` arriving now. Without a clock
    /// from them yet, there is nothing to measure against, and nothing is noted.
    pub(super) fn frame_arrived(&self, media_us: u64) {
        let Some(delay) = self.clock().and_then(|clock| clock.delay_ms(media_us, wall_us())) else {
            return;
        };
        if let Ok(mut delays) = self.delays.lock() {
            delays.push(delay);
        }
    }

    /// This second's delays, emptied for the next.
    pub(super) fn take_delays(&self) -> Vec<u32> {
        self.delays.lock().map(|mut d| std::mem::take(&mut *d)).unwrap_or_default()
    }

    /// One of their audio frames was decoded.
    pub(super) fn audio_frame(&self) {
        self.audio_frames.fetch_add(1, Ordering::Relaxed);
    }

    /// Audio frames decoded since the last call.
    pub(super) fn take_audio_frames(&self) -> u32 {
        self.audio_frames.swap(0, Ordering::Relaxed)
    }

    /// A hearable gap opened in their audio.
    pub(super) fn audio_gap(&self) {
        self.audio_gaps.fetch_add(1, Ordering::Relaxed);
    }

    /// Their playback ran dry.
    pub(super) fn audio_ran_dry(&self) {
        self.audio_dry.fetch_add(1, Ordering::Relaxed);
    }

    /// Audio gaps and dry spells so far.
    pub(super) fn audio_trouble(&self) -> (u32, u32) {
        (
            self.audio_gaps.load(Ordering::Relaxed),
            self.audio_dry.load(Ordering::Relaxed),
        )
    }

    /// The flag the audio side raises while they talk.
    pub(super) fn talking_flag(&self) -> Arc<AtomicBool> {
        self.talking.clone()
    }

    fn set_video_fps(&self, fps: f32) {
        self.fps_tenths
            .store((fps * 10.0).round().max(0.0) as u32, Ordering::Relaxed);
    }
}

/// Counts decoded frames and publishes the rate once a second.
///
/// **Counted where frames arrive, timed somewhere else.** Publishing the rate
/// from inside a decode loop would update it only when a frame arrives, so a
/// stalled stream would leave its last healthy number on screen for as long as
/// it was stalled -- showing 30 fps through the freeze it is meant to reveal. A
/// ticker reading a counter reports the truth, which during a stall is zero.
///
/// Must be made inside the tokio runtime; it spawns the ticker, and stops it
/// when dropped.
pub(super) struct FpsMeter {
    counted: Arc<AtomicU32>,
    ticker: tokio::task::JoinHandle<()>,
}

/// How often a meter publishes. Faster than the log on purpose: the log is for
/// comparing runs afterwards, this is for watching a link misbehave.
const FPS_WINDOW: Duration = Duration::from_secs(1);

impl FpsMeter {
    pub(super) fn new(stats: Arc<PeerStats>) -> Self {
        let counted = Arc::new(AtomicU32::new(0));
        let ticker = tokio::spawn({
            let counted = counted.clone();
            async move {
                let mut ticker = tokio::time::interval(FPS_WINDOW);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    let frames = counted.swap(0, Ordering::Relaxed);
                    stats.set_video_fps(frames as f32 / FPS_WINDOW.as_secs_f32());
                }
            }
        });
        Self { counted, ticker }
    }

    /// One frame arrived.
    pub(super) fn frame(&self) {
        self.counted.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for FpsMeter {
    fn drop(&mut self) {
        self.ticker.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_maps_through_the_senders_clock() {
        // Camera time 5 s was wall time 1000 s; a frame stamped 5.1 s,
        // arriving at wall 1000.25 s, took 150 ms.
        let clock = Clock {
            wall_us: 1_000_000_000,
            media_us: 5_000_000,
        };
        assert_eq!(clock.delay_ms(5_100_000, 1_000_250_000), Some(150));
    }

    #[test]
    fn disagreeing_clocks_give_no_number_rather_than_a_wrong_one() {
        let clock = Clock {
            wall_us: 1_000_000_000,
            media_us: 0,
        };
        assert_eq!(clock.delay_ms(2_000_000, 1_000_000_000), None);
    }

    #[test]
    fn no_clock_no_delays() {
        let stats = PeerStats::default();
        stats.frame_arrived(123);
        assert!(stats.take_delays().is_empty());
    }

    #[test]
    fn camera_and_mic_are_on_until_told_otherwise() {
        let stats = PeerStats::default();
        assert!(stats.camera_on() && stats.mic_on());
        stats.set_camera(false);
        assert!(!stats.camera_on() && stats.mic_on());
    }

    #[test]
    fn a_new_peer_has_no_rate_yet() {
        assert_eq!(PeerStats::default().video_fps(), None);
    }

    #[test]
    fn a_stalled_peer_reads_zero_not_unknown() {
        let stats = PeerStats::default();
        stats.set_video_fps(0.0);
        assert_eq!(stats.video_fps(), Some(0.0));
    }

    #[test]
    fn a_rate_keeps_its_tenths() {
        let stats = PeerStats::default();
        stats.set_video_fps(23.84);
        assert_eq!(stats.video_fps(), Some(23.8));
    }
}
