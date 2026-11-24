//! Following the link down, and back up again.
//!
//! The encoder opens at a rate chosen for the frame size, which is a guess
//! about a connection nobody has measured yet. QUIC's congestion controller
//! does measure it, and the transport will happily drop what the encoder
//! oversupplies -- so without this the two never speak, and a link that cannot
//! carry the stream shows up as video that lags and then jumps rather than as
//! video that quietly got softer.
//!
//! Almost none of the policy is ours. `moq_mux::rate` decides what a bandwidth
//! estimate should mean, and its behaviour suits a call: give bandwidth back
//! the instant the pipe closes, take it back at 25% per second, and ignore
//! moves under 5% so the encoder is not reconfigured at every twitch. What is
//! here is the wiring and the cadence.
//!
//! **The camera aims at the whole connection's estimate, and nothing is held
//! back for the other streams.** The policy expects an estimate that is this
//! sender's alone; dividing one connection among its senders is
//! `moq_net::bandwidth::Allocator`'s job, and we do not use one. So while a
//! screen is shared, the camera and the share together can ask for more than
//! the link carries, and the transport sheds the difference by priority.
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

/// The bitrate the encoder opened at, once there is one; zero until then.
///
/// Set by the encode thread, which builds the encoder only when the camera's
/// first frame says what size it is -- possibly long after the call connected,
/// or never, for someone whose camera is off.
pub(super) type Ceiling = Arc<AtomicU64>;

/// Watches the send-bandwidth estimate and publishes a bitrate to follow it.
///
/// The estimate is sampled and reported from the start; a target is only
/// published once `ceiling` is set, since there is no encoder to steer before.
///
/// Runs until aborted. A dialling side reads the estimate off the `Reconnect`
/// it gets back; a listening side has none until somebody calls, so
/// [`forward`] feeds one of ours from the session.
pub(super) fn spawn(
    bandwidth: moq_net::bandwidth::Consumer,
    ceiling: Ceiling,
    target: Target,
    metrics: Arc<super::Metrics>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut control: Option<moq_mux::rate::Control> = None;
        let mut ticker = tokio::time::interval(SAMPLE);
        // A missed tick should not be made up for by a burst of catch-up polls:
        // only the newest estimate means anything.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Logged whether or not it moves the target, because "no estimate at
        // all" and "an estimate that never changes" look identical from the
        // outside and mean very different things.
        let mut reported = Instant::now();

        // Owned by this future so its summary is written when the call aborts
        // the task: the loop below never ends any other way, so there is no
        // "after the loop" to write it from.
        let mut budget = Summarise(Budget::default());

        loop {
            ticker.tick().await;
            let now = Instant::now();
            let estimate = bandwidth.peek();
            // Plain bits per second from here on: the encoder and the stats take
            // numbers, and only the policy speaks `Rate`.
            let estimate_bps = estimate.map(|rate| rate.as_bps());
            metrics.set_estimate_kbps(estimate_bps.map(|bps| (bps / 1000) as u32));
            if reported.elapsed() >= Duration::from_secs(5) {
                match estimate_bps {
                    Some(bps) => tracing::debug!("[rate] estimate {} kbps", bps / 1000),
                    None => tracing::debug!("[rate] no estimate from the transport"),
                }
                reported = now;
            }
            let opened = ceiling.load(Ordering::Relaxed);
            if control.is_none() && opened != 0 {
                let policy = moq_mux::rate::Policy::new(moq_net::bandwidth::Rate::from_bps(opened));
                control = Some(moq_mux::rate::Control::new(policy));
            }
            if let Some(control) = control.as_mut() {
                if let Some(next) = control.update(estimate, now).map(|rate| rate.as_bps()) {
                    tracing::debug!(
                        "[rate] target {} kbps (estimate {} kbps)",
                        next / 1000,
                        estimate_bps.unwrap_or(0) / 1000
                    );
                    metrics.set_send_kbps((next / 1000) as u32);
                    target.store(next, Ordering::Relaxed);
                }
            }
            // The applied target, not this tick's suggestion: a move inside the
            // policy's deadband is never applied, so the encoder is still at
            // the old rate and that is what "held" has to be judged on.
            let applied = control.as_ref().map_or(opened, |control| control.target().as_bps());
            let sample = Sample {
                ceiling: opened,
                target: applied,
                estimate: estimate_bps,
            };
            let before = budget.0.state();
            if let Some(after) = budget.0.observe(sample, now) {
                log_change(before, after, sample);
            }
        }
    })
}

/// Whether the camera is getting the rate it opened at, or the link is holding
/// it lower.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum State {
    /// No encoder yet, or no estimate from the transport: nothing to judge by.
    #[default]
    Unknown,
    /// The target is at (or within [`HELD_BELOW`] of) the ceiling.
    AtCeiling,
    /// The target is below the ceiling because the estimate put it there.
    Held,
}

/// One tick of the rate loop, in bits per second. A `ceiling` of zero means
/// no encoder exists yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Sample {
    pub ceiling: u64,
    pub target: u64,
    pub estimate: Option<u64>,
}

/// A target under this fraction of the ceiling is held by the link.
///
/// The same 5% as the rate policy's own deadband (`moq_mux::rate`, read): it
/// never applies a move smaller than that, so a target a few percent under the
/// ceiling is one the policy itself treats as "not worth changing", and the
/// picture is no different. It also covers the one place the policy can stall
/// short of the ceiling -- an estimate just under it, where the last raise is
/// inside the deadband.
const HELD_BELOW: f64 = 0.95;

/// How long the target has to stay back near the ceiling before it counts as
/// back.
///
/// Drops are applied at once and raises climb at 25% a second (the policy's
/// ramp, read), so a link that wobbles around the threshold produces a target
/// that dips and recovers within a second or two. Waiting this long on the way
/// up, and not on the way down, means a wobble writes one "held" line and stays
/// held instead of writing a pair every couple of seconds. Three seconds is
/// also about how long the ramp takes to recover from half the ceiling, so a
/// genuine recovery is reported promptly.
const BACK_AFTER: Duration = Duration::from_secs(3);

/// The camera's budget over a call: its state now, and how it spent its time.
///
/// Pure: no clock, no logging. The rate loop feeds it each tick and logs what
/// it returns.
#[derive(Clone, Debug, Default)]
pub(super) struct Budget {
    state: State,
    /// The previous sample's time, while the state is known. Time is only
    /// counted between two known samples, so a gap with no encoder or no
    /// estimate is neither "held" nor "at the ceiling".
    last: Option<Instant>,
    /// While held, when the target came back within [`HELD_BELOW`], if it has
    /// stayed there since.
    near: Option<Instant>,
    observed: Duration,
    held: Duration,
    lowest: Option<u64>,
    /// The latest nonzero ceiling seen: once there was an encoder, the summary
    /// says so even if the estimate never came.
    ceiling: u64,
}

impl Budget {
    /// The state as of the last sample.
    pub(super) fn state(&self) -> State {
        self.state
    }

    /// Takes one sample, and returns the new state if it changed.
    pub(super) fn observe(&mut self, sample: Sample, now: Instant) -> Option<State> {
        if let Some(last) = self.last {
            let elapsed = now.saturating_duration_since(last);
            self.observed += elapsed;
            if self.state == State::Held {
                self.held += elapsed;
            }
        }
        if sample.ceiling != 0 {
            self.ceiling = sample.ceiling;
        }
        let next = if sample.ceiling == 0 || sample.estimate.is_none() {
            self.near = None;
            State::Unknown
        } else {
            self.lowest = Some(self.lowest.map_or(sample.target, |lowest| lowest.min(sample.target)));
            let near = sample.target as f64 >= sample.ceiling as f64 * HELD_BELOW;
            match (self.state, near) {
                (State::Held, true) => {
                    let since = *self.near.get_or_insert(now);
                    if now.saturating_duration_since(since) >= BACK_AFTER {
                        // The wait was the target being back, not held; it was
                        // only counted as held until that was certain.
                        self.held = self.held.saturating_sub(now.saturating_duration_since(since));
                        self.near = None;
                        State::AtCeiling
                    } else {
                        State::Held
                    }
                }
                (_, true) => State::AtCeiling,
                (_, false) => {
                    self.near = None;
                    State::Held
                }
            }
        };
        self.last = (next != State::Unknown).then_some(now);
        let changed = next != self.state;
        self.state = next;
        changed.then_some(next)
    }

    /// What the call amounted to, for one line at its end.
    pub(super) fn summary(&self) -> Summary {
        match (self.ceiling, self.lowest) {
            (0, _) => Summary::NoEncoder,
            (ceiling, None) => Summary::NoEstimate { ceiling },
            (ceiling, Some(lowest)) => Summary::Observed {
                observed: self.observed,
                held: self.held,
                lowest,
                ceiling,
            },
        }
    }
}

/// [`Budget::summary`]'s answer, in words by its `Display`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Summary {
    /// The camera never produced a frame to size an encoder by.
    NoEncoder,
    /// An encoder, but never an estimate to hold it against.
    NoEstimate { ceiling: u64 },
    Observed {
        observed: Duration,
        held: Duration,
        lowest: u64,
        ceiling: u64,
    },
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Summary::NoEncoder => write!(f, "[rate] this call: the camera never encoded, so no rate to report"),
            Summary::NoEstimate { ceiling } => write!(
                f,
                "[rate] this call: camera at {} kbps, but the transport gave no estimate, so whether the link held it is unknown",
                ceiling / 1000
            ),
            Summary::Observed {
                observed,
                held,
                lowest,
                ceiling,
            } => {
                // A stay of a second or two, or one where the estimate came and
                // went at once: "0% of 0 s" would read as a measurement.
                if observed < Duration::from_secs(1) {
                    return write!(
                        f,
                        "[rate] this call: camera at {} kbps, too briefly to say whether the link held it",
                        ceiling / 1000
                    );
                }
                let share = if observed.is_zero() {
                    0.0
                } else {
                    held.as_secs_f64() / observed.as_secs_f64() * 100.0
                };
                // "0%" for a call that was held at all would read as "never".
                let share = if held > Duration::ZERO && share < 1.0 {
                    "<1%".to_string()
                } else {
                    format!("{share:.0}%")
                };
                write!(
                    f,
                    "[rate] this call: held by the link {share} of {} observed, lowest {} kbps, ceiling {} kbps",
                    span(observed),
                    lowest / 1000,
                    ceiling / 1000
                )
            }
        }
    }
}

/// A duration as a person reads it in a log: "42 s", "5 min 12 s".
fn span(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        0..60 => format!("{secs} s"),
        _ => format!("{} min {} s", secs / 60, secs % 60),
    }
}

/// Writes the summary when the rate loop is dropped, which is how it ends.
struct Summarise(Budget);

impl Drop for Summarise {
    fn drop(&mut self) {
        tracing::info!("{}", self.0.summary());
    }
}

/// The `info!` line for a change of state. Only changes are logged, so a call
/// that sits at the ceiling writes one line, and a log someone sends us says
/// whether their rate was being held without needing `RUST_LOG`.
fn log_change(from: State, to: State, sample: Sample) {
    let kbps = |bps: u64| bps / 1000;
    match (to, sample.estimate) {
        (State::Held, Some(estimate)) => tracing::info!(
            "[rate] held by the link: {} kbps of {} (estimate {})",
            kbps(sample.target),
            kbps(sample.ceiling),
            kbps(estimate)
        ),
        (State::AtCeiling, _) if from == State::Held => {
            tracing::info!("[rate] back at the ceiling: {} kbps", kbps(sample.ceiling))
        }
        (State::AtCeiling, _) => tracing::info!("[rate] at the ceiling: {} kbps", kbps(sample.ceiling)),
        _ => tracing::info!(
            "[rate] no estimate from the transport; the camera stays at {} kbps of {}",
            kbps(sample.target),
            kbps(sample.ceiling)
        ),
    }
}

/// Takes the pending bitrate, if one was published since the last call.
pub(super) fn take(target: &Target) -> Option<u64> {
    match target.swap(0, Ordering::Relaxed) {
        0 => None,
        bitrate => Some(bitrate),
    }
}

/// Mirrors a session's send-bandwidth estimate into a persistent producer,
/// until the session closes.
///
/// The dialling side needs nothing like this: `Reconnect` hands out an estimate
/// that exists before the connection does and survives reconnects, because it
/// does this forwarding internally. A listener has no such object -- it has a
/// session, and only once somebody has called. So the estimate handed to
/// [`spawn`] is one of ours, and this keeps it fed.
///
/// Clears the estimate on close rather than leaving the last value standing: a
/// stale number is worse than none, since the rate policy deliberately holds
/// its target when the estimate goes away and would otherwise hold one measured
/// against a connection that no longer exists.
pub(super) async fn forward(
    session: moq_net::Session,
    estimate: moq_net::bandwidth::Producer,
    metrics: Arc<super::Metrics>,
) {
    let source = session.send_bandwidth();
    if source.is_none() {
        tracing::debug!("[rate] this session reports no bandwidth estimate");
    }
    let mut ticker = tokio::time::interval(SAMPLE);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            error = session.closed() => {
                tracing::debug!("[rate] session closed, dropping the estimate: {error}");
                break;
            }
            _ = ticker.tick() => {
                if let Some(source) = source.as_ref() {
                    let _ = estimate.set(source.peek());
                }
                metrics.set_connection(&session.stats());
            }
        }
    }
    let _ = estimate.set(None);
}

#[cfg(test)]
mod tests {
    use super::*;

    const CEILING: u64 = 2_500_000;

    fn at(ceiling: u64, target: u64, estimate: Option<u64>) -> Sample {
        Sample {
            ceiling,
            target,
            estimate,
        }
    }

    /// Feeds `samples` 200 ms apart from `start`, returning every change.
    fn feed(budget: &mut Budget, start: Instant, samples: &[Sample]) -> Vec<State> {
        samples
            .iter()
            .enumerate()
            .filter_map(|(tick, sample)| budget.observe(*sample, start + SAMPLE * tick as u32))
            .collect()
    }

    #[test]
    fn nothing_is_known_without_an_encoder_or_an_estimate() {
        let mut budget = Budget::default();
        let start = Instant::now();
        let changes = feed(
            &mut budget,
            start,
            &[at(0, 0, Some(5_000_000)), at(CEILING, CEILING, None), at(0, 0, None)],
        );
        assert!(changes.is_empty());
        assert_eq!(budget.state(), State::Unknown);
    }

    #[test]
    fn a_drop_is_held_at_once_and_a_recovery_waits() {
        let mut budget = Budget::default();
        let start = Instant::now();
        let mut changes = feed(&mut budget, start, &[at(CEILING, CEILING, Some(5_000_000))]);
        assert_eq!(changes, vec![State::AtCeiling]);

        // The estimate falls: held on the very next tick.
        changes = vec![budget.observe(at(CEILING, 900_000, Some(1_000_000)), start + SAMPLE).unwrap()];
        assert_eq!(changes, vec![State::Held]);

        // Back at the ceiling, but not until it has stayed there BACK_AFTER.
        let back = start + SAMPLE * 2;
        assert_eq!(budget.observe(at(CEILING, CEILING, Some(5_000_000)), back), None);
        assert_eq!(
            budget.observe(at(CEILING, CEILING, Some(5_000_000)), back + BACK_AFTER - SAMPLE),
            None
        );
        assert_eq!(
            budget.observe(at(CEILING, CEILING, Some(5_000_000)), back + BACK_AFTER),
            Some(State::AtCeiling)
        );
    }

    #[test]
    fn a_few_percent_under_the_ceiling_is_not_held() {
        let mut budget = Budget::default();
        let start = Instant::now();
        let changes = feed(
            &mut budget,
            start,
            &[at(CEILING, CEILING, Some(9_000_000)), at(CEILING, 2_400_000, Some(2_400_000))],
        );
        assert_eq!(changes, vec![State::AtCeiling]);
    }

    #[test]
    fn a_target_hovering_at_the_threshold_does_not_flap() {
        let mut budget = Budget::default();
        let start = Instant::now();
        // Dips under 95% and back every 600 ms for a minute.
        let samples: Vec<Sample> = (0..300)
            .map(|tick| match tick % 3 {
                0 => at(CEILING, 2_300_000, Some(2_300_000)),
                _ => at(CEILING, 2_450_000, Some(2_600_000)),
            })
            .collect();
        let changes = feed(&mut budget, start, &samples);
        assert_eq!(changes, vec![State::Held], "one line for the whole wobble");
    }

    #[test]
    fn losing_the_estimate_is_unknown_and_not_counted() {
        let mut budget = Budget::default();
        let start = Instant::now();
        budget.observe(at(CEILING, 900_000, Some(1_000_000)), start);
        assert_eq!(
            budget.observe(at(CEILING, 900_000, None), start + Duration::from_secs(1)),
            Some(State::Unknown)
        );
        // Ten seconds with no estimate, then it returns.
        assert_eq!(
            budget.observe(at(CEILING, 900_000, Some(1_000_000)), start + Duration::from_secs(11)),
            Some(State::Held)
        );
        budget.observe(at(CEILING, 900_000, Some(1_000_000)), start + Duration::from_secs(12));
        assert_eq!(
            budget.summary(),
            Summary::Observed {
                observed: Duration::from_secs(2),
                held: Duration::from_secs(2),
                lowest: 900_000,
                ceiling: CEILING,
            }
        );
    }

    #[test]
    fn a_stay_too_short_to_measure_says_so() {
        let summary = Summary::Observed {
            observed: Duration::from_millis(400),
            held: Duration::ZERO,
            lowest: 345_000,
            ceiling: 345_000,
        };
        assert_eq!(
            summary.to_string(),
            "[rate] this call: camera at 345 kbps, too briefly to say whether the link held it"
        );
    }

    #[test]
    fn the_summary_counts_held_time_and_the_lowest_target() {
        let mut budget = Budget::default();
        let start = Instant::now();
        let s = Duration::from_secs;
        let ceiling = |t| at(CEILING, CEILING, Some(5_000_000)).with_time(t);
        let held = |target, t| at(CEILING, target, Some(target + 100_000)).with_time(t);
        // 10 s at the ceiling, 20 s held (lowest 600), then back for 10 s.
        for (sample, t) in [
            ceiling(s(0)),
            ceiling(s(10)),
            held(1_200_000, s(10)),
            held(600_000, s(20)),
            held(1_800_000, s(30)),
            ceiling(s(30)),
            ceiling(s(30) + BACK_AFTER),
            ceiling(s(40)),
        ] {
            budget.observe(sample, start + t);
        }
        let summary = budget.summary();
        assert_eq!(
            summary,
            Summary::Observed {
                observed: s(40),
                held: s(20),
                lowest: 600_000,
                ceiling: CEILING,
            }
        );
        assert_eq!(
            summary.to_string(),
            "[rate] this call: held by the link 50% of 40 s observed, lowest 600 kbps, ceiling 2500 kbps"
        );
    }

    #[test]
    fn a_call_with_no_encoder_says_so_rather_than_printing_zeros() {
        let mut budget = Budget::default();
        let start = Instant::now();
        feed(&mut budget, start, &[at(0, 0, Some(5_000_000)); 50]);
        assert_eq!(budget.summary(), Summary::NoEncoder);
        assert_eq!(Budget::default().summary(), Summary::NoEncoder);
        assert!(!budget.summary().to_string().contains(" 0 "));
    }

    #[test]
    fn an_encoder_without_an_estimate_is_unknown_not_unheld() {
        let mut budget = Budget::default();
        feed(&mut budget, Instant::now(), &[at(CEILING, CEILING, None); 50]);
        assert_eq!(budget.summary(), Summary::NoEstimate { ceiling: CEILING });
    }

    #[test]
    fn a_brief_hold_is_not_rounded_to_never() {
        let summary = Summary::Observed {
            observed: Duration::from_secs(600),
            held: Duration::from_secs(1),
            lowest: 1_000_000,
            ceiling: CEILING,
        };
        assert!(summary.to_string().contains("<1% of 10 min 0 s"));
    }

    impl Sample {
        /// Pairs a sample with its time, for tables.
        fn with_time(self, time: Duration) -> (Sample, Duration) {
            (self, time)
        }
    }
}
