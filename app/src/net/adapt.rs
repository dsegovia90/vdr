//! Following the link down, and back up again.
//!
//! An encoder opens at a rate chosen for the frame size, which is a guess
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
//! **One budget, divided.** The transport's estimate covers the whole
//! connection, and the policy expects an estimate that is one sender's alone.
//! So the call divides it with `moq_net::bandwidth::Allocator` (read, not
//! ours): strictly by the tracks' priorities -- the microphone (80), then a
//! shared screen (70), then the camera (60) -- each tier up to what it
//! reserved, and only among tracks somebody is subscribed to. The microphone
//! reserves its rate and keeps it (moq-audio's choice); the camera and the
//! share each run a loop here that follows their own slice.
//!
//! **The share leaves room for the camera.** The allocator gives a tier that
//! does not fit nothing at all, and the camera's encoder would then sit at its
//! floor anyway, over the estimate by that much. So while the camera is
//! watched, the share follows its slice less whatever the camera's slice falls
//! short of the camera's floor ([`share_budget`]), and what is sent fits.
//!
//! **The ceiling is the rate the encoder opened at.** Rate control can only
//! take the target down: an optimistic estimate is not a reason to send more
//! than was configured, and at least one backend rejects a bitrate above its
//! opening rate outright. A share that raises its ceiling (Sharp, a larger
//! window) opens a new encoder for it, and its loop starts a new policy.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use moq_net::bandwidth::{Consumer, Rate};

/// How often the estimate is sampled.
///
/// The estimate is read rather than awaited: the consumer exposes a poll-based
/// interface wanting a `kio::Waiter`, and a timer costs one small dependency
/// less for a loop whose own policy ramps over seconds. 200 ms is far finer
/// than the 25%-per-second climb can use, and a drop is applied on the next
/// tick either way.
const SAMPLE: Duration = Duration::from_millis(200);

/// Which outgoing stream a rate loop steers. It decides the stream's floor and
/// the words its log lines use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Stream {
    #[default]
    Camera,
    Share,
}

impl Stream {
    /// The lowest rate the loop asks this stream's encoder for.
    ///
    /// The camera's is the rate policy's own default, a tenth of the ceiling.
    /// The share's is fixed: text below it is unreadable whatever the size of
    /// the screen, so a squeezed share gives up bitrate down to here and no
    /// further, at its usual frame rate.
    pub(super) fn floor(self, ceiling: u64) -> u64 {
        match self {
            Stream::Camera => ceiling / 10,
            Stream::Share => super::share::SHARE_MIN_BPS,
        }
    }

    fn policy(self, ceiling: u64) -> moq_mux::rate::Policy {
        let mut policy = moq_mux::rate::Policy::new(Rate::from_bps(ceiling));
        policy.min = Rate::from_bps(self.floor(ceiling));
        policy
    }

    /// "this call" and "camera", or "this share" and "screen": the call
    /// summary's subject, and what it calls the stream.
    fn words(self) -> (&'static str, &'static str) {
        match self {
            Stream::Camera => ("this call", "camera"),
            Stream::Share => ("this share", "screen"),
        }
    }
}

/// One outgoing video stream, as its rate loop and its encode thread share it.
///
/// The encoder is owned by its encode thread (it is not `Send`), so the loop
/// cannot touch it: it publishes a target here, and the encode thread applies
/// it before its next frame. Atomics for everything the encode thread reads
/// per frame.
#[derive(Default)]
pub(super) struct Lane {
    /// The bitrate the encoder opened at; zero until there is one. Set by the
    /// encode thread, which builds the encoder only when the first frame says
    /// what size it is -- possibly long after the call connected, or never,
    /// for someone whose camera is off.
    ceiling: AtomicU64,
    /// A bitrate the encoder should move to; zero when nothing is pending.
    /// Swapped out by the encode thread, so a target that arrives while it is
    /// busy replaces the previous one instead of queueing behind it.
    pending: AtomicU64,
    /// The target the loop holds now, whether or not the encoder has applied
    /// it yet; zero before the loop has an encoder to steer.
    current: AtomicU64,
    /// The reader of this stream's slice of the link, once its encoder has
    /// reserved one. A lock because a reader is not a number: taken by the
    /// rate loops every [`SAMPLE`], never by an encode thread.
    slice: Mutex<Option<Consumer>>,
}

impl Lane {
    /// Records the bitrate the encoder (re)opened at.
    pub(super) fn set_ceiling(&self, bps: u64) {
        self.ceiling.store(bps, Ordering::Relaxed);
    }

    pub(super) fn ceiling(&self) -> u64 {
        self.ceiling.load(Ordering::Relaxed)
    }

    /// Takes the pending bitrate, if one was published since the last call.
    pub(super) fn take(&self) -> Option<u64> {
        match self.pending.swap(0, Ordering::Relaxed) {
            0 => None,
            bitrate => Some(bitrate),
        }
    }

    fn publish(&self, bps: u64) {
        self.pending.store(bps, Ordering::Relaxed);
    }

    /// The loop's target now; zero before it has one.
    pub(super) fn current(&self) -> u64 {
        self.current.load(Ordering::Relaxed)
    }

    /// Follows `slice` from now on: the reader of the reservation the encode
    /// thread holds for as long as the stream runs.
    pub(super) fn follow(&self, slice: Consumer) {
        if let Ok(mut held) = self.slice.lock() {
            *held = Some(slice);
        }
    }

    /// This stream's slice now: `None` before it reserved one, while nobody is
    /// subscribed to it, or while the connection has no estimate -- all of
    /// which mean "hold your rate" to the policy.
    pub(super) fn slice(&self) -> Option<u64> {
        let held = self.slice.lock().ok()?;
        held.as_ref()?.peek().map(|rate| rate.as_bps())
    }

    /// The floor the camera's loop holds it to, for the share to leave room
    /// for; zero while there is no camera encoder.
    fn camera_floor(&self) -> u64 {
        match self.ceiling() {
            0 => 0,
            ceiling => Stream::Camera.floor(ceiling),
        }
    }
}

/// What the share's loop follows: its own slice, less whatever the camera's
/// slice falls short of the camera's floor.
///
/// The camera is served after the share, so on a link too small for both its
/// slice is what the share left, down to nothing. Its encoder never goes under
/// its floor whatever it is given, so a camera given less than that still
/// sends the floor, and the two together overshoot by the difference. Taking
/// the difference off the share is what makes them fit. While the camera has
/// its floor or more the share gives nothing back; while the camera has no
/// slice (no encoder, or nobody watching it) there is nothing to make room
/// for, since its reservation then claims nothing.
pub(super) fn share_budget(share: Option<u64>, camera: Option<u64>, camera_floor: u64) -> Option<u64> {
    let share = share?;
    let short = camera.map_or(0, |camera| camera_floor.saturating_sub(camera));
    Some(share.saturating_sub(short))
}

/// After a share's encoder is rebuilt at `new_ceiling`, the bitrate to put it
/// back to, if the link was holding it below `old_ceiling`.
///
/// A new encoder opens at its ceiling, and the loop only notices the new
/// ceiling on its next tick. A share the link was holding back would spend that
/// time at full rate on a link that just said it cannot carry it.
pub(super) fn reapply(current: u64, old_ceiling: u64, new_ceiling: u64) -> Option<u64> {
    (current != 0 && current < old_ceiling).then(|| current.min(new_ceiling))
}

/// The rate policy for one stream, rebuilt whenever its ceiling moves.
///
/// Pure: the loop feeds it the ceiling and what to follow each tick, and
/// publishes what it returns.
#[derive(Debug, Default)]
pub(super) struct Steer {
    stream: Stream,
    control: Option<moq_mux::rate::Control>,
    /// The ceiling `control` was built for; zero for none.
    built_for: u64,
}

impl Steer {
    pub(super) fn new(stream: Stream) -> Self {
        Self {
            stream,
            ..Self::default()
        }
    }

    /// One tick: returns a target to publish, if there is one.
    ///
    /// A ceiling that moves from one encoder to another starts a new policy.
    /// It starts at the new ceiling, unless the link was holding the old one
    /// back: then it starts where it was held ([`reapply`], the same rule the
    /// rebuilt encoder was put back by), and climbs from there at the policy's
    /// pace. A new ceiling is not news about the link. Its target is published
    /// whether or not this tick moved it, so the rebuilt encoder and the
    /// policy agree. The first encoder needs no such word, since it opened at
    /// the ceiling.
    pub(super) fn tick(&mut self, ceiling: u64, follow: Option<u64>, now: Instant) -> Option<u64> {
        let moved_ceiling = ceiling != self.built_for;
        let rebuilt = moved_ceiling && self.built_for != 0 && ceiling != 0;
        if moved_ceiling {
            let held = reapply(self.target(), self.built_for, ceiling);
            self.built_for = ceiling;
            self.control = (ceiling != 0).then(|| {
                let mut control = moq_mux::rate::Control::new(self.stream.policy(ceiling));
                if let Some(held) = held {
                    control.update(Some(Rate::from_bps(held)), now);
                }
                control
            });
        }
        let control = self.control.as_mut()?;
        let moved = control.update(follow.map(Rate::from_bps), now).map(|rate| rate.as_bps());
        moved.or_else(|| rebuilt.then(|| control.target().as_bps()))
    }

    /// The target the policy holds now; zero with no encoder.
    pub(super) fn target(&self) -> u64 {
        self.control.as_ref().map_or(0, |control| control.target().as_bps())
    }
}

/// The call's send budget, as a stream needs it to take part.
#[derive(Clone)]
pub(super) struct Link {
    /// Divides the estimate among whoever reserves from it.
    pub allocator: moq_net::bandwidth::Allocator,
    /// The whole connection's estimate, undivided, for the logs.
    pub estimate: Consumer,
    /// The camera, which a share leaves room for.
    pub camera: Arc<Lane>,
}

impl Link {
    /// A budget per call attempt, which every stream of the attempt shares.
    ///
    /// Made before anything publishes, since the microphone reserves as it
    /// starts. Its estimate is fed from whichever side the attempt turns out
    /// to be: [`forward`] for a listener, [`mirror`] for a dialler.
    pub(super) fn new(estimate: &moq_net::bandwidth::Producer) -> Self {
        Self {
            allocator: moq_net::bandwidth::Allocator::new(estimate.consume()),
            estimate: estimate.consume(),
            camera: Arc::default(),
        }
    }
}

/// Everything a rate loop needs, named rather than positional.
pub(super) struct Follow {
    pub stream: Stream,
    /// The whole connection's estimate: logged, and for the camera, reported.
    pub estimate: Consumer,
    pub lane: Arc<Lane>,
    /// The camera, for the share to leave room for; `None` for the camera.
    pub beside: Option<Arc<Lane>>,
    pub metrics: Arc<super::Metrics>,
    /// Whether this loop's numbers are still the ones to show. A share being
    /// replaced finishes after the next has begun, and must not write over it.
    pub shown: Box<dyn Fn() -> bool + Send>,
}

/// Watches a stream's slice of the link and publishes a bitrate to follow it.
///
/// The estimate is sampled and reported from the start; a target is only
/// published once the lane has a ceiling, since there is no encoder to steer
/// before.
///
/// Runs until aborted, which is when it writes its summary.
pub(super) fn spawn(follow: Follow) -> tokio::task::JoinHandle<()> {
    let Follow {
        stream,
        estimate,
        lane,
        beside,
        metrics,
        shown,
    } = follow;
    tokio::spawn(async move {
        let mut steer = Steer::new(stream);
        let mut ticker = tokio::time::interval(SAMPLE);
        // A missed tick should not be made up for by a burst of catch-up polls:
        // only the newest estimate means anything.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Logged whether or not it moves the target, because "no estimate at
        // all" and "an estimate that never changes" look identical from the
        // outside and mean very different things.
        let mut reported = Instant::now();

        // Owned by this future so its summary is written when the task is
        // aborted: the loop below never ends any other way, so there is no
        // "after the loop" to write it from.
        let mut budget = Summarise(Budget::new(stream));

        loop {
            ticker.tick().await;
            let now = Instant::now();
            // Plain bits per second from here on: the encoder and the stats take
            // numbers, and only the policy speaks `Rate`.
            let estimate_bps = estimate.peek().map(|rate| rate.as_bps());
            // The camera's loop runs for the whole attempt, the share's only
            // while sharing, so the connection's own figure is the camera's to
            // report.
            if stream == Stream::Camera {
                metrics.set_estimate_kbps(estimate_bps.map(|bps| (bps / 1000) as u32));
                if reported.elapsed() >= Duration::from_secs(5) {
                    match estimate_bps {
                        Some(bps) => tracing::debug!("[rate] estimate {} kbps", bps / 1000),
                        None => tracing::debug!("[rate] no estimate from the transport"),
                    }
                    reported = now;
                }
            }
            let slice = lane.slice();
            let follows = match &beside {
                Some(camera) => share_budget(slice, camera.slice(), camera.camera_floor()),
                None => slice,
            };
            let shown = shown();
            if shown {
                metrics.set_slice_kbps(stream, follows.map(|bps| (bps / 1000) as u32));
            }
            let opened = lane.ceiling();
            if let Some(next) = steer.tick(opened, follows, now) {
                match stream {
                    Stream::Camera => tracing::debug!(
                        "[rate] target {} kbps (estimate {} kbps)",
                        next / 1000,
                        estimate_bps.unwrap_or(0) / 1000
                    ),
                    Stream::Share => tracing::debug!(
                        "[rate] share target {} kbps (slice {} kbps)",
                        next / 1000,
                        follows.unwrap_or(0) / 1000
                    ),
                }
                if shown {
                    metrics.set_target_kbps(stream, (next / 1000) as u32);
                }
                lane.publish(next);
            }
            lane.current.store(steer.target(), Ordering::Relaxed);
            // The applied target, not this tick's suggestion: a move inside the
            // policy's deadband is never applied, so the encoder is still at
            // the old rate and that is what "held" has to be judged on.
            let applied = match steer.target() {
                0 => opened,
                target => target,
            };
            // The camera is judged against the connection's estimate, as it
            // always was; the share against its own slice, which is what says
            // whether it has one at all.
            let sample = Sample {
                ceiling: opened,
                target: applied,
                estimate: match stream {
                    Stream::Camera => estimate_bps,
                    Stream::Share => follows,
                },
            };
            let before = budget.0.state();
            if let Some(after) = budget.0.observe(sample, now) {
                log_change(stream, before, after, sample, estimate_bps, follows);
            }
        }
    })
}

/// Whether a stream is getting the rate it opened at, or the link is holding
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

/// One stream's budget over its life: its state now, and how it spent its
/// time.
///
/// Pure: no clock, no logging. The rate loop feeds it each tick and logs what
/// it returns.
#[derive(Clone, Debug, Default)]
pub(super) struct Budget {
    /// Whose budget, for the summary's words.
    stream: Stream,
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
    /// A budget for `stream`; [`Default`] is the camera's.
    pub(super) fn new(stream: Stream) -> Self {
        Self {
            stream,
            ..Self::default()
        }
    }

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

    /// What the stream's life amounted to, for one line at its end.
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

/// [`Budget::summary`]'s answer. Its `Display` is the camera's words;
/// [`Summary::of`] gives another stream's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Summary {
    /// The stream never produced a frame to size an encoder by.
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

impl Summary {
    /// This summary in `stream`'s words.
    pub(super) fn of(self, stream: Stream) -> Said {
        Said(stream, self)
    }
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.of(Stream::Camera).fmt(f)
    }
}

/// A [`Summary`] in one stream's words.
pub(super) struct Said(Stream, Summary);

impl std::fmt::Display for Said {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (when, what) = self.0.words();
        match self.1 {
            Summary::NoEncoder => write!(f, "[rate] {when}: the {what} never encoded, so no rate to report"),
            Summary::NoEstimate { ceiling } => write!(
                f,
                "[rate] {when}: {what} at {} kbps, but the transport gave no estimate, so whether the link held it is unknown",
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
                        "[rate] {when}: {what} at {} kbps, too briefly to say whether the link held it",
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
                    "[rate] {when}: held by the link {share} of {} observed, lowest {} kbps, ceiling {} kbps",
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
        tracing::info!("{}", self.0.summary().of(self.0.stream));
    }
}

/// The `info!` line for a change of state. Only changes are logged, so a
/// stream that sits at the ceiling writes one line, and a log someone sends us
/// says whether their rate was being held without needing `RUST_LOG`.
fn log_change(stream: Stream, from: State, to: State, sample: Sample, estimate: Option<u64>, slice: Option<u64>) {
    tracing::info!("{}", change_line(stream, from, to, sample, estimate, slice));
}

/// [`log_change`]'s words, apart so they can be checked. For the share,
/// `sample.estimate` is its slice and `estimate` the connection's.
///
/// `slice` is what the stream follows. The camera's line names it only when
/// the other streams took a real part of the link: "held at 400 (estimate
/// 3000)" would otherwise read as a mistake, when it is the share being served
/// first.
fn change_line(
    stream: Stream,
    from: State,
    to: State,
    sample: Sample,
    estimate: Option<u64>,
    slice: Option<u64>,
) -> String {
    let kbps = |bps: u64| bps / 1000;
    match (stream, to, sample.estimate) {
        (Stream::Camera, State::Held, Some(estimate))
            if slice.is_some_and(|slice| (slice as f64) < estimate as f64 * HELD_BELOW) =>
        {
            format!(
                "[rate] held by the link: {} kbps of {} (slice {}, estimate {})",
                kbps(sample.target),
                kbps(sample.ceiling),
                kbps(slice.unwrap_or(0)),
                kbps(estimate)
            )
        }
        (Stream::Camera, State::Held, Some(estimate)) => format!(
            "[rate] held by the link: {} kbps of {} (estimate {})",
            kbps(sample.target),
            kbps(sample.ceiling),
            kbps(estimate)
        ),
        (Stream::Camera, State::AtCeiling, _) if from == State::Held => {
            format!("[rate] back at the ceiling: {} kbps", kbps(sample.ceiling))
        }
        (Stream::Camera, State::AtCeiling, _) => format!("[rate] at the ceiling: {} kbps", kbps(sample.ceiling)),
        (Stream::Camera, _, _) => format!(
            "[rate] no estimate from the transport; the camera stays at {} kbps of {}",
            kbps(sample.target),
            kbps(sample.ceiling)
        ),
        (Stream::Share, State::Held, Some(slice)) => format!(
            "[rate] share held by the link: {} kbps of {} (slice {}, estimate {})",
            kbps(sample.target),
            kbps(sample.ceiling),
            kbps(slice),
            estimate.map_or("?".to_string(), |estimate| kbps(estimate).to_string())
        ),
        (Stream::Share, State::AtCeiling, _) if from == State::Held => {
            format!("[rate] share back at the ceiling: {} kbps", kbps(sample.ceiling))
        }
        (Stream::Share, State::AtCeiling, _) => format!("[rate] share at the ceiling: {} kbps", kbps(sample.ceiling)),
        // A share always has an encoder, so its unknown is a missing slice.
        (Stream::Share, _, _) => format!(
            "[rate] no slice from the transport; the share stays at {} kbps of {}",
            kbps(sample.target),
            kbps(sample.ceiling)
        ),
    }
}

/// Mirrors a session's send-bandwidth estimate into the call's own producer,
/// until the session closes.
///
/// A listener has no persistent estimate -- it has a session, and only once
/// somebody has called -- so the call's estimate is fed from whichever session
/// is live. Clears the estimate on close rather than leaving the last value
/// standing: a stale number is worse than none, since the rate policy
/// deliberately holds its target when the estimate goes away and would
/// otherwise hold one measured against a connection that no longer exists.
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

/// Copies a dialling side's estimate into the call's own producer, for as long
/// as the attempt lasts.
///
/// `Reconnect` already keeps one estimate across sessions (`None` between
/// them), so this is only a copy: the allocator has to be built before the
/// attempt knows which side it is, so it divides a producer of ours, and this
/// feeds it. `None` when the source ends, for the same reason as [`forward`].
pub(super) async fn mirror(mut source: Consumer, estimate: moq_net::bandwidth::Producer) {
    // `changed` reports the current value first when there is one, so nothing
    // has to be seeded.
    while let Ok(rate) = source.changed().await {
        let _ = estimate.set(rate);
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

    const CAMERA_FLOOR: u64 = CEILING / 10;

    #[test]
    fn a_starved_camera_takes_its_floor_from_the_share() {
        // The camera got nothing: the share gives back the whole floor.
        assert_eq!(share_budget(Some(2_000_000), Some(0), CAMERA_FLOOR), Some(1_750_000));
        // Some, but short of the floor: only the difference.
        assert_eq!(share_budget(Some(2_000_000), Some(100_000), CAMERA_FLOOR), Some(1_850_000));
        // A camera with its floor or more needs nothing back.
        assert_eq!(share_budget(Some(2_000_000), Some(CAMERA_FLOOR), CAMERA_FLOOR), Some(2_000_000));
        assert_eq!(share_budget(Some(2_000_000), Some(5_000_000), CAMERA_FLOOR), Some(2_000_000));
    }

    #[test]
    fn the_share_never_goes_below_nothing() {
        assert_eq!(share_budget(Some(100_000), Some(0), CAMERA_FLOOR), Some(0));
    }

    #[test]
    fn no_room_is_left_for_a_camera_that_claims_none() {
        // No camera encoder, or nobody watching it: its reservation reports
        // `None`, and claims nothing.
        assert_eq!(share_budget(Some(2_000_000), None, CAMERA_FLOOR), Some(2_000_000));
        assert_eq!(share_budget(Some(2_000_000), None, 0), Some(2_000_000));
        // No slice for the share: hold, whatever the camera has.
        assert_eq!(share_budget(None, Some(0), CAMERA_FLOOR), None);
    }

    #[test]
    fn the_camera_floor_is_the_policys_own() {
        let policy = moq_mux::rate::Policy::new(Rate::from_bps(CEILING));
        assert_eq!(Stream::Camera.policy(CEILING).min, policy.min);
        assert_eq!(Stream::Share.policy(CEILING).min, Rate::from_bps(super::super::share::SHARE_MIN_BPS));
        let lane = Lane::default();
        assert_eq!(lane.camera_floor(), 0, "no encoder, no floor");
        lane.set_ceiling(CEILING);
        assert_eq!(lane.camera_floor(), CAMERA_FLOOR);
    }

    #[test]
    fn a_squeezed_share_stops_at_its_floor() {
        let mut steer = Steer::new(Stream::Share);
        assert_eq!(steer.tick(CEILING, Some(100_000), Instant::now()), Some(600_000));
    }

    #[test]
    fn the_first_encoder_needs_no_word_without_an_estimate() {
        let mut steer = Steer::new(Stream::Camera);
        let start = Instant::now();
        assert_eq!(steer.tick(0, Some(1_000_000), start), None, "nothing to steer yet");
        assert_eq!(steer.tick(CEILING, None, start + SAMPLE), None);
        assert_eq!(steer.target(), CEILING);
    }

    #[test]
    fn sharp_starts_a_new_policy_where_the_link_holds_it() {
        let mut steer = Steer::new(Stream::Share);
        let start = Instant::now();
        assert_eq!(steer.tick(2_500_000, Some(1_000_000), start), Some(1_000_000));
        // Sharp: a new encoder at 5.5 Mbps on the same 1 Mbps link.
        assert_eq!(steer.tick(5_529_600, Some(1_000_000), start + SAMPLE), Some(1_000_000));
        assert_eq!(steer.target(), 1_000_000);
        // And it climbs from there at the policy's pace, not in one jump.
        let raised = steer.tick(5_529_600, Some(9_000_000), start + SAMPLE * 6);
        assert_eq!(raised, Some(1_250_000));
    }

    #[test]
    fn a_new_ceiling_without_an_estimate_is_the_configured_rate() {
        let mut steer = Steer::new(Stream::Share);
        let start = Instant::now();
        assert_eq!(steer.tick(2_500_000, None, start), None);
        // Sharp, then a resize down: said even though nothing moved them.
        assert_eq!(steer.tick(5_529_600, None, start + SAMPLE), Some(5_529_600));
        assert_eq!(steer.tick(1_536_000, None, start + SAMPLE * 2), Some(1_536_000));
        // Then quiet again.
        assert_eq!(steer.tick(1_536_000, None, start + SAMPLE * 3), None);
    }

    #[test]
    fn a_held_share_stays_held_across_a_new_ceiling_when_the_estimate_goes() {
        let mut steer = Steer::new(Stream::Share);
        let start = Instant::now();
        steer.tick(2_500_000, Some(1_000_000), start);
        // The estimate goes; the target holds, as the policy does.
        assert_eq!(steer.tick(2_500_000, None, start + SAMPLE), None);
        assert_eq!(steer.target(), 1_000_000);
        // Sharp: losing the estimate was not news that the link got better.
        assert_eq!(steer.tick(5_529_600, None, start + SAMPLE * 2), Some(1_000_000));
        assert_eq!(steer.tick(5_529_600, None, start + SAMPLE * 3), None);
    }

    #[test]
    fn sharp_on_a_free_link_goes_straight_to_the_new_ceiling() {
        let mut steer = Steer::new(Stream::Share);
        let start = Instant::now();
        assert_eq!(steer.tick(2_500_000, Some(9_000_000), start), None);
        assert_eq!(steer.tick(5_529_600, Some(9_000_000), start + SAMPLE), Some(5_529_600));
    }

    #[test]
    fn a_resize_down_under_the_held_target_takes_the_new_ceiling() {
        let mut steer = Steer::new(Stream::Share);
        let start = Instant::now();
        steer.tick(2_500_000, Some(2_000_000), start);
        assert_eq!(steer.tick(1_536_000, Some(2_000_000), start + SAMPLE), Some(1_536_000));
    }

    #[test]
    fn a_rebuilt_encoder_goes_back_to_where_the_link_held_it() {
        // Held at 1 Mbps, Sharp turned on: back to 1 Mbps.
        assert_eq!(reapply(1_000_000, 2_500_000, 5_529_600), Some(1_000_000));
        // Held, then resized smaller than the hold: the new ceiling.
        assert_eq!(reapply(2_000_000, 2_500_000, 1_536_000), Some(1_536_000));
        // At its ceiling, or no loop yet: the new encoder's rate stands.
        assert_eq!(reapply(2_500_000, 2_500_000, 5_529_600), None);
        assert_eq!(reapply(0, 2_500_000, 5_529_600), None);
    }

    #[test]
    fn a_share_summary_says_this_share() {
        let mut budget = Budget::new(Stream::Share);
        let start = Instant::now();
        budget.observe(at(CEILING, 1_200_000, Some(1_200_000)), start);
        budget.observe(at(CEILING, 1_200_000, Some(1_200_000)), start + Duration::from_secs(30));
        assert_eq!(
            budget.summary().of(Stream::Share).to_string(),
            "[rate] this share: held by the link 100% of 30 s observed, lowest 1200 kbps, ceiling 2500 kbps"
        );
        assert_eq!(
            Summary::NoEstimate { ceiling: CEILING }.of(Stream::Share).to_string(),
            "[rate] this share: screen at 2500 kbps, but the transport gave no estimate, so whether the link held it is unknown"
        );
        assert_eq!(
            Summary::NoEncoder.of(Stream::Share).to_string(),
            "[rate] this share: the screen never encoded, so no rate to report"
        );
        // The camera's words are what they always were.
        assert_eq!(
            Summary::NoEncoder.to_string(),
            "[rate] this call: the camera never encoded, so no rate to report"
        );
        assert_eq!(Budget::new(Stream::Camera).summary().to_string(), Budget::default().summary().to_string());
    }

    #[test]
    fn change_lines_name_the_stream() {
        let held = at(CEILING, 900_000, Some(1_000_000));
        let at_ceiling = at(CEILING, CEILING, Some(5_000_000));
        let unknown = at(CEILING, 900_000, None);
        let line = |stream, from, to, sample| change_line(stream, from, to, sample, Some(3_000_000), None);
        // Squeezed by the streams served before it, the camera says so.
        assert_eq!(
            change_line(
                Stream::Camera,
                State::AtCeiling,
                State::Held,
                at(CEILING, 400_000, Some(3_000_000)),
                Some(3_000_000),
                Some(436_000)
            ),
            "[rate] held by the link: 400 kbps of 2500 (slice 436, estimate 3000)"
        );
        // With the link to itself (its slice is the estimate, less the microphone), as before.
        assert_eq!(
            change_line(Stream::Camera, State::AtCeiling, State::Held, held, Some(1_000_000), Some(960_000)),
            "[rate] held by the link: 900 kbps of 2500 (estimate 1000)"
        );
        // The camera's, byte for byte as before.
        assert_eq!(
            line(Stream::Camera, State::AtCeiling, State::Held, held),
            "[rate] held by the link: 900 kbps of 2500 (estimate 1000)"
        );
        assert_eq!(
            line(Stream::Camera, State::Held, State::AtCeiling, at_ceiling),
            "[rate] back at the ceiling: 2500 kbps"
        );
        assert_eq!(line(Stream::Camera, State::Unknown, State::AtCeiling, at_ceiling), "[rate] at the ceiling: 2500 kbps");
        assert_eq!(
            line(Stream::Camera, State::Held, State::Unknown, unknown),
            "[rate] no estimate from the transport; the camera stays at 900 kbps of 2500"
        );
        // The share's: its slice in the estimate's place, and the whole link beside it.
        assert_eq!(
            line(Stream::Share, State::AtCeiling, State::Held, held),
            "[rate] share held by the link: 900 kbps of 2500 (slice 1000, estimate 3000)"
        );
        assert_eq!(
            line(Stream::Share, State::Held, State::AtCeiling, at_ceiling),
            "[rate] share back at the ceiling: 2500 kbps"
        );
        assert_eq!(line(Stream::Share, State::Unknown, State::AtCeiling, at_ceiling), "[rate] share at the ceiling: 2500 kbps");
        assert_eq!(
            line(Stream::Share, State::Held, State::Unknown, unknown),
            "[rate] no slice from the transport; the share stays at 900 kbps of 2500"
        );
    }

    /// The whole budget in process: our own tracks, at the priorities our
    /// catalog gives them, divided by the real allocator. A local `consume`
    /// counts as a subscriber, which is how moq-net's own tests do it.
    #[tokio::test]
    async fn audio_then_share_then_camera_divide_one_estimate() {
        use super::super::call::{SCREEN_PRIORITY, VIDEO_PRIORITY};
        const AUDIO_PRIORITY: u8 = 80;

        let estimate = moq_net::bandwidth::Producer::new();
        let link = Link::new(&estimate);
        let mut broadcast = moq_net::broadcast::Info::default().produce();
        let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
        assert_eq!(catalog.track_info(VIDEO_PRIORITY).priority, 60, "the camera");
        assert_eq!(catalog.track_info(SCREEN_PRIORITY).priority, 70, "the share");
        let track = |name: &str, priority| broadcast.create_track(name.to_string(), catalog.track_info(priority)).unwrap();
        let audio = track("audio", AUDIO_PRIORITY);
        let share = track("screen", SCREEN_PRIORITY);
        let camera = track("camera", VIDEO_PRIORITY);
        let _audio_sub = audio.consume();
        let _camera_sub = camera.consume();
        let share_sub = share.consume();

        let _audio = link.allocator.reserve(&audio.demand(), Rate::from_bps(64_000));
        let share_reserved = link.allocator.reserve(&share.demand(), Rate::from_bps(2_500_000));
        let camera_reserved = link.allocator.reserve(&camera.demand(), Rate::from_bps(2_000_000));
        link.camera.follow(camera_reserved.consumer());
        link.camera.set_ceiling(2_000_000);
        let share_lane = Lane::default();
        share_lane.follow(share_reserved.consumer());

        // No estimate yet: every slice says "hold".
        assert_eq!((share_lane.slice(), link.camera.slice()), (None, None));

        // Room for the share, less for the camera; in priority order, summing
        // to the estimate.
        estimate.set(Some(Rate::from_bps(3_000_000))).unwrap();
        assert_eq!(share_lane.slice(), Some(2_500_000));
        assert_eq!(link.camera.slice(), Some(436_000));

        // Too little for both: the camera is starved, and the share makes room
        // for its floor, so the three together still fit.
        estimate.set(Some(Rate::from_bps(2_000_000))).unwrap();
        assert_eq!(share_lane.slice(), Some(1_936_000));
        assert_eq!(link.camera.slice(), Some(0));
        let follows = share_budget(share_lane.slice(), link.camera.slice(), link.camera.camera_floor());
        assert_eq!(follows, Some(1_736_000));
        assert_eq!(64_000 + follows.unwrap() + link.camera.camera_floor(), 2_000_000);

        // Nobody watching the share: it claims nothing, the camera has it all.
        drop(share_sub);
        assert_eq!(share_lane.slice(), None);
        assert_eq!(link.camera.slice(), Some(1_936_000));
    }

    impl Sample {
        /// Pairs a sample with its time, for tables.
        fn with_time(self, time: Duration) -> (Sample, Duration) {
            (self, time)
        }
    }
}
