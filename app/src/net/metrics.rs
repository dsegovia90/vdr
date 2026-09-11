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

use std::sync::atomic::{AtomicU32, Ordering};

/// Live numbers for the current call.
#[derive(Default)]
pub struct Metrics {
    rtt_ms: AtomicU32,
    /// Loss in hundredths of a percent, so a tenth of a percent survives.
    loss_bp: AtomicU32,
    send_kbps: AtomicU32,
    estimate_kbps: AtomicU32,
    /// Incoming video, in tenths of a frame per second.
    video_fps_tenths: AtomicU32,
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

    /// Video arriving from the peer.
    pub fn video_fps(&self) -> Option<f32> {
        nonzero(&self.video_fps_tenths).map(|t| t as f32 / 10.0)
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

    pub(super) fn set_video_fps(&self, fps: f32) {
        self.video_fps_tenths
            .store((fps * 10.0).round().max(0.0) as u32, Ordering::Relaxed);
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
            &self.video_fps_tenths,
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
