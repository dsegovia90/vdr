//! Following the link down, and back up again.
//!
//! The encoder opens at a rate chosen for the frame size, which is a guess
//! about a connection nobody has measured yet. QUIC's congestion controller
//! does measure it, and the transport will happily drop what the encoder
//! oversupplies -- so without this the two never speak, and a link that cannot
//! carry the stream shows up as video that lags and then jumps rather than as
//! video that quietly got softer.
//!
//! Almost none of the policy is ours. `moq_video::encode::rate` decides what a
//! bandwidth estimate should mean, and its defaults suit a call: target 90% of
//! the estimate so audio and overhead have room, give bandwidth back the
//! instant the pipe closes, take it back at 25% per second, and ignore moves
//! under 5% so the encoder is not reconfigured at every twitch. What is here is
//! the wiring and the cadence.
//!
//! **The ceiling is the rate the encoder opened at.** Rate control can only
//! take the target down: an optimistic estimate is not a reason to send more
//! than was configured, and at least one backend rejects a bitrate above its
//! opening rate outright.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the estimate is sampled.
///
/// The estimate is read rather than awaited: the consumer exposes a poll-based
/// interface wanting a `kio::Waiter`, and a timer costs one small dependency
/// less for a loop whose own policy ramps over seconds. 200 ms is far finer
/// than the 25%-per-second climb can use, and a drop is applied on the next
/// tick either way.
const SAMPLE: Duration = Duration::from_millis(200);

/// A bitrate the encoder should move to, handed across threads.
///
/// The encoder is owned by the blocking encode loop, so this side cannot touch
/// it. Zero means nothing is pending; the encode loop swaps the value out and
/// applies it, which also means a target that arrives while it is busy replaces
/// the previous one instead of queueing behind it.
pub(super) type Target = Arc<AtomicU64>;

/// Watches the send-bandwidth estimate and publishes a bitrate to follow it.
///
/// Runs until aborted. Only the dialling side can do this today: the estimate
/// hangs off the `Reconnect` a client gets back, and the listening side's
/// `serve_both` hands out nothing equivalent, so a callee keeps sending at its
/// opening rate.
pub(super) fn spawn(
    bandwidth: moq_net::bandwidth::Consumer,
    ceiling: u64,
    target: Target,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let policy = moq_video::encode::rate::Policy::new(ceiling);
        let mut control = moq_video::encode::rate::Control::new(policy);
        let mut ticker = tokio::time::interval(SAMPLE);
        // A missed tick should not be made up for by a burst of catch-up polls:
        // only the newest estimate means anything.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Logged whether or not it moves the target, because "no estimate at
        // all" and "an estimate that never changes" look identical from the
        // outside and mean very different things.
        let mut reported = Instant::now();

        loop {
            ticker.tick().await;
            let estimate = bandwidth.peek();
            if reported.elapsed() >= Duration::from_secs(5) {
                match estimate {
                    Some(bps) => tracing::debug!("[rate] estimate {} kbps", bps / 1000),
                    None => tracing::debug!("[rate] no estimate from the transport"),
                }
                reported = Instant::now();
            }
            let Some(next) = control.update(estimate, Instant::now()) else {
                continue;
            };
            tracing::debug!(
                "[rate] target {} kbps (estimate {} kbps)",
                next / 1000,
                estimate.unwrap_or(0) / 1000
            );
            target.store(next, Ordering::Relaxed);
        }
    })
}

/// Takes the pending bitrate, if one was published since the last call.
pub(super) fn take(target: &Target) -> Option<u64> {
    match target.swap(0, Ordering::Relaxed) {
        0 => None,
        bitrate => Some(bitrate),
    }
}
