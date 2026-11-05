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

/// Live numbers for the current call.
#[derive(Default)]
pub struct Metrics {
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

    /// Forgets everything, for when a call ends.
    pub(super) fn clear(&self) {
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
}

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
