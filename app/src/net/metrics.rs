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
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

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
    /// Our video encoder and the last peer's video decoder, by backend name
    /// ("videotoolbox", "vaapi", "openh264"...), so a tester can say which
    /// they got without opening a log.
    codecs: std::sync::Mutex<(Option<String>, Option<String>)>,
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

    /// The encoder and decoder backends in use, as `(encoder, decoder)`.
    pub fn codecs(&self) -> (Option<String>, Option<String>) {
        self.codecs.lock().map(|codecs| codecs.clone()).unwrap_or_default()
    }

    pub(super) fn set_encoder(&self, name: &str) {
        if let Ok(mut codecs) = self.codecs.lock() {
            codecs.0 = Some(name.to_string());
        }
    }

    pub(super) fn set_decoder(&self, name: &str) {
        if let Ok(mut codecs) = self.codecs.lock() {
            codecs.1 = Some(name.to_string());
        }
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
    pub(super) fn set_connection(&self, stats: &moq_net::session::Stats) {
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
        if let Ok(mut codecs) = self.codecs.lock() {
            *codecs = (None, None);
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
    /// The subroom they are in, as they say in their presence document, or
    /// `None` for the main room. For our own entry, the one we chose.
    room: std::sync::Mutex<Option<String>>,
    /// What we are asking of other peers, each with when we asked. Our own
    /// entry only; a peer's requests are acted on as they arrive, not kept.
    asks: std::sync::Mutex<Vec<(Request, Instant)>>,
    /// Who invites us, to which room (`None` is main). Our own entry only.
    /// Keyed by inviter so each inviter's task writes only its own invite.
    invites: std::sync::Mutex<BTreeMap<String, Option<String>>>,
    /// The peer whose request turned our microphone off. Our own entry only;
    /// cleared by our own unmute, nobody else's.
    muted_by: std::sync::Mutex<Option<String>>,
    /// Delays of their frames this second, in ms, for the stats file.
    delays: std::sync::Mutex<Vec<u32>>,
    /// Audio frames decoded this second.
    audio_frames: AtomicU32,
    /// Gaps of more than [`AUDIO_GAP`] between their decoded audio frames, ever.
    audio_gaps: AtomicU32,
    /// Times their playback buffer was found empty while frames still came.
    audio_dry: AtomicU32,
    /// Whether they are sharing a screen: a peer's from their presence
    /// document, ours set by the share thread while it publishes.
    sharing: AtomicBool,
    /// Why our last share did not start or ended early, in words for the bar.
    /// Our own entry only; cleared when the next share is attempted.
    share_problem: std::sync::Mutex<Option<String>>,
    /// Which of our shares may write `sharing` and `share_problem`: the
    /// newest one begun. Our own entry only. A share being replaced finishes
    /// on its own thread after the next has begun, and must not clear the
    /// new one's flag or report into it.
    share_turn: std::sync::atomic::AtomicU64,
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

/// One peer asking one peer to do something, said in the asker's presence
/// document. Every peer reads it; only `to` acts on it, once per nonce.
///
/// An invite carries no room: it means "join the room I am in", which the
/// asker's own presence already says, so the two can never disagree.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub to: String,
    pub kind: Ask,
    /// The asker's wall clock in µs when it asked: unique per asker without a
    /// counter, and distinct across its restarts.
    pub nonce: u64,
}

/// What a [`Request`] asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ask {
    /// Join the asker's room.
    Invite,
    /// Turn your microphone off.
    Mute,
    /// A kind from a later build. Without this, an unknown kind would fail
    /// the whole presence document, and a failed parse ends listening to
    /// that peer.
    #[serde(other)]
    Unknown,
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
            room: std::sync::Mutex::new(None),
            asks: std::sync::Mutex::new(Vec::new()),
            invites: std::sync::Mutex::new(BTreeMap::new()),
            muted_by: std::sync::Mutex::new(None),
            delays: std::sync::Mutex::new(Vec::new()),
            audio_frames: AtomicU32::new(0),
            audio_gaps: AtomicU32::new(0),
            audio_dry: AtomicU32::new(0),
            sharing: AtomicBool::new(false),
            share_problem: std::sync::Mutex::new(None),
            share_turn: std::sync::atomic::AtomicU64::new(0),
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

    /// The subroom they are in, or `None` for the main room.
    pub fn room(&self) -> Option<String> {
        self.room.lock().ok().and_then(|room| room.clone())
    }

    /// Records which subroom they are in; `None` is the main room.
    ///
    /// On our own entry, a change of room also withdraws our invites: an
    /// invite means "join the room I am in", and that room is no longer it.
    /// Mutes stand. A peer's entry has no asks, so this costs it nothing.
    pub fn set_room(&self, room: Option<String>) {
        let changed = match self.room.lock() {
            Ok(mut slot) => {
                let changed = *slot != room;
                *slot = room;
                changed
            }
            Err(_) => false,
        };
        if changed {
            self.retain_asks(|request| request.kind != Ask::Invite);
        }
    }

    /// Whether they are sharing a screen.
    pub fn sharing(&self) -> bool {
        self.sharing.load(Ordering::Relaxed)
    }

    /// Records whether they are sharing a screen.
    pub fn set_sharing(&self, on: bool) {
        self.sharing.store(on, Ordering::Relaxed);
    }

    /// Makes `turn` our current share, clearing the last one's problem. Turns
    /// must be unique; `0` is nobody's, which is how a leave ends them all.
    pub fn begin_share(&self, turn: u64) {
        self.share_turn.store(turn, Ordering::SeqCst);
        self.set_share_problem(None);
        if turn == 0 {
            self.set_sharing(false);
        }
    }

    /// Whether `turn` is still our current share.
    pub fn share_is_current(&self, turn: u64) -> bool {
        turn != 0 && self.share_turn.load(Ordering::SeqCst) == turn
    }

    /// Records whether we share, if `turn` is still the current share.
    pub fn set_own_sharing(&self, turn: u64, on: bool) {
        if self.share_is_current(turn) {
            self.set_sharing(on);
        }
    }

    /// Records why our share failed, if `turn` is still the current share.
    pub fn set_own_share_problem(&self, turn: u64, problem: String) {
        if self.share_is_current(turn) {
            self.set_share_problem(Some(problem));
        }
    }

    /// Why our last share failed, if it did.
    pub fn share_problem(&self) -> Option<String> {
        self.share_problem.lock().ok().and_then(|problem| problem.clone())
    }

    /// Records why our share failed; `None` as a new one is attempted.
    pub fn set_share_problem(&self, problem: Option<String>) {
        if let Ok(mut slot) = self.share_problem.lock() {
            *slot = problem;
        }
    }

    /// Asks `to` for something. Replaces a standing request of the same kind
    /// to the same peer with a fresh nonce and time, so asking again both
    /// refreshes the timeout and counts as a new request (a second mute after
    /// they unmuted works).
    pub fn ask(&self, to: &str, kind: Ask) {
        if let Ok(mut asks) = self.asks.lock() {
            asks.retain(|(request, _)| !(request.to == to && request.kind == kind));
            let request = Request {
                to: to.to_string(),
                kind,
                nonce: wall_us(),
            };
            asks.push((request, Instant::now()));
        }
    }

    /// What we are asking, as our presence document says it.
    pub fn requests(&self) -> Vec<Request> {
        self.asks
            .lock()
            .map(|asks| asks.iter().map(|(request, _)| request.clone()).collect())
            .unwrap_or_default()
    }

    /// Drops requests older than `ttl`.
    pub fn expire_asks(&self, ttl: Duration) {
        if let Ok(mut asks) = self.asks.lock() {
            asks.retain(|(_, asked)| asked.elapsed() < ttl);
        }
    }

    /// Drops what `to` has already done: our invite once they are in our
    /// room, our mute once their mic is off. A satisfied request stops being
    /// said within one document.
    pub fn settle(&self, to: &str, their_room: &Option<String>, their_mic: bool) {
        let ours = self.room();
        self.retain_asks(|request| {
            request.to != to
                || match request.kind {
                    Ask::Invite => *their_room != ours,
                    Ask::Mute => their_mic,
                    Ask::Unknown => true,
                }
        });
    }

    fn retain_asks(&self, keep: impl Fn(&Request) -> bool) {
        if let Ok(mut asks) = self.asks.lock() {
            asks.retain(|(request, _)| keep(request));
        }
    }

    /// Who invites us, to which room (`None` is main).
    pub fn invites(&self) -> BTreeMap<String, Option<String>> {
        self.invites.lock().map(|invites| invites.clone()).unwrap_or_default()
    }

    /// Records that `from` invites us to `room`.
    pub fn set_invite(&self, from: &str, room: Option<String>) {
        if let Ok(mut invites) = self.invites.lock() {
            if invites.get(from) != Some(&room) {
                invites.insert(from.to_string(), room);
            }
        }
    }

    /// Forgets `from`'s invite, if any.
    pub fn drop_invite(&self, from: &str) {
        if let Ok(mut invites) = self.invites.lock() {
            invites.remove(from);
        }
    }

    /// The peer whose request muted us, until we unmute.
    pub fn muted_by(&self) -> Option<String> {
        self.muted_by.lock().ok().and_then(|by| by.clone())
    }

    /// Records who muted us; `None` once we unmute ourselves.
    pub fn set_muted_by(&self, by: Option<String>) {
        if let Ok(mut slot) = self.muted_by.lock() {
            *slot = by;
        }
    }

    /// Forgets our asks, our invites and who muted us, for when a call ends:
    /// none of them means anything in the next one.
    pub fn clear_asks(&self) {
        if let Ok(mut asks) = self.asks.lock() {
            asks.clear();
        }
        if let Ok(mut invites) = self.invites.lock() {
            invites.clear();
        }
        self.set_muted_by(None);
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

    /// Records whether they are talking. For our own tile, from the
    /// microphone meter; a remote peer's comes from their decoded audio.
    pub fn set_talking(&self, on: bool) {
        self.talking.store(on, Ordering::Relaxed);
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
    fn a_replaced_share_cannot_touch_the_next() {
        let local = PeerStats::default();
        local.begin_share(1);
        local.begin_share(2);
        local.set_own_sharing(2, true);
        // The old share finishing late, after the new one said `sharing`.
        local.set_own_sharing(1, false);
        local.set_own_share_problem(1, "old".into());
        assert!(local.sharing());
        assert_eq!(local.share_problem(), None);
    }

    #[test]
    fn a_leave_ends_every_turn() {
        let local = PeerStats::default();
        local.begin_share(3);
        local.set_own_share_problem(3, "Screen recording is off".into());
        local.begin_share(0);
        assert_eq!(local.share_problem(), None);
        assert!(!local.sharing());
        local.set_own_sharing(3, true);
        local.set_own_sharing(0, true);
        assert!(!local.sharing(), "turn 0 is nobody's");
    }

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

    fn kinds(stats: &PeerStats) -> Vec<(String, Ask)> {
        stats.requests().into_iter().map(|r| (r.to, r.kind)).collect()
    }

    #[test]
    fn asking_again_replaces_the_same_request_with_a_new_nonce() {
        let stats = PeerStats::default();
        stats.ask("a", Ask::Mute);
        stats.ask("a", Ask::Invite);
        let first = stats.requests()[0].nonce;
        std::thread::sleep(Duration::from_millis(2));
        stats.ask("a", Ask::Mute);
        let requests = stats.requests();
        assert_eq!(requests.len(), 2);
        let mute = requests.iter().find(|r| r.kind == Ask::Mute).unwrap();
        assert_ne!(mute.nonce, first);
    }

    #[test]
    fn settling_drops_only_what_was_done() {
        let stats = PeerStats::default();
        stats.set_room(Some("Kitchen".into()));
        for to in ["a", "b"] {
            stats.ask(to, Ask::Invite);
            stats.ask(to, Ask::Mute);
        }
        // `a` is still elsewhere with the mic on: nothing settles.
        stats.settle("a", &None, true);
        assert_eq!(stats.requests().len(), 4);
        // `a` joins us: the invite goes, the mute stays.
        stats.settle("a", &Some("Kitchen".into()), true);
        // `b` turns the mic off: the mute goes, the invite stays.
        stats.settle("b", &None, false);
        assert_eq!(
            kinds(&stats),
            vec![("a".into(), Ask::Mute), ("b".into(), Ask::Invite)]
        );
    }

    #[test]
    fn asks_expire() {
        let stats = PeerStats::default();
        stats.ask("a", Ask::Invite);
        stats.expire_asks(Duration::from_secs(30));
        assert_eq!(stats.requests().len(), 1);
        stats.expire_asks(Duration::ZERO);
        assert!(stats.requests().is_empty());
    }

    #[test]
    fn changing_room_withdraws_invites_but_not_mutes() {
        let stats = PeerStats::default();
        stats.ask("a", Ask::Invite);
        stats.ask("a", Ask::Mute);
        stats.set_room(None);
        assert_eq!(stats.requests().len(), 2, "the same room keeps both");
        stats.set_room(Some("Kitchen".into()));
        assert_eq!(kinds(&stats), vec![("a".into(), Ask::Mute)]);
    }

    #[test]
    fn invites_are_per_inviter_and_clear_at_teardown() {
        let stats = PeerStats::default();
        stats.set_invite("a", Some("Kitchen".into()));
        stats.set_invite("b", None);
        stats.drop_invite("a");
        assert_eq!(stats.invites().into_iter().collect::<Vec<_>>(), vec![("b".into(), None)]);
        stats.set_muted_by(Some("b".into()));
        stats.ask("b", Ask::Mute);
        stats.clear_asks();
        assert!(stats.invites().is_empty() && stats.requests().is_empty());
        assert_eq!(stats.muted_by(), None);
    }

    #[test]
    fn a_rate_keeps_its_tenths() {
        let stats = PeerStats::default();
        stats.set_video_fps(23.84);
        assert_eq!(stats.video_fps(), Some(23.8));
    }
}
