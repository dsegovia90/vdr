//! What a host uploads for other people: their media, passed on through us to
//! the guests who reach us directly, and to the relay when we bridge.
//!
//! **Measured by the library, per broadcast path.** Every session of a host is
//! tagged with a `moq_net::stats` context: one tier for the sessions we dial
//! (the relay, other hosts) and one per guest we accept. moq-net then counts
//! the payload bytes it hands each session for each broadcast path, so what we
//! forward is simply the egress on paths that are not ours. No subtraction
//! from a connection's totals is needed, and none of QUIC's or MoQ's framing
//! is in it, which makes it the same kind of figure as the encoded bytes the
//! Network tab already charts for our own streams. A frame is counted as it is
//! handed to the transport, so on a congested link this reads a little high.
//!
//! **Only visible, not acted on.** Our own send budget does not give way to
//! what we forward: the estimate it divides is the one session carrying our
//! media, and guests' traffic mostly rides other connections with estimates of
//! their own, so subtracting it would step our camera down on a guess. Turning
//! guests away under load belongs with deciding who hosts at all. And the
//! library has no way to rank one session's traffic against another's: each
//! session has its own send queue, each connection its own congestion
//! controller. What this does instead is say, once per episode, when what we
//! forward and send together goes over the link estimate.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_net::stats::{Registry, Report, Tier};

use crate::rates::Hosting;

/// The tier every session we dial counts under.
const OUT: &str = "out";

/// Each guest's tier is this, then a number.
const GUEST: &str = "guest/";

/// Below this, a second's egress is control traffic and presence, not media:
/// a guest is not pulling anything, and nothing is being forwarded.
const MEDIA_KBPS: u32 = 16;

/// Seconds over [`MEDIA_KBPS`] before forwarding is said to have started:
/// one second can be a keyframe someone fetched once.
const STARTED_AFTER: u32 = 2;

/// Seconds under it before forwarding is said to have stopped, so a stream
/// that pauses for a moment does not read as two episodes.
const STOPPED_AFTER: u32 = 5;

/// Seconds over the link estimate before it is said, for the same reason.
const OVER_AFTER: u32 = 5;

/// A host's counters, and what they have added up to.
pub(super) struct Forwarded {
    registry: Registry,
    me: String,
    /// Guests accepted so far, to give each its own tier.
    guests: AtomicU64,
    /// Whether we are serving guests now. A join that meant to host and could
    /// not still has counters; nothing it shows is about hosting.
    serving: AtomicBool,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    report: Report,
    counts: Counts,
    tally: Tally,
}

impl Forwarded {
    pub(super) fn new(me: String) -> Arc<Self> {
        Arc::new(Self {
            registry: Registry::new(Default::default()),
            me,
            guests: AtomicU64::new(0),
            serving: AtomicBool::new(false),
            state: Mutex::default(),
        })
    }

    /// The context for every session we dial.
    pub(super) fn outgoing(&self) -> moq_net::stats::Session {
        self.registry.tier(Tier::new(OUT)).session("")
    }

    /// A context of its own for one guest's session, so that guest can be
    /// counted, and seen to be pulling or not, apart from the others.
    pub(super) fn guest(&self) -> moq_net::stats::Session {
        let n = self.guests.fetch_add(1, Ordering::Relaxed);
        self.registry.tier(Tier::new(format!("{GUEST}{n}"))).session("")
    }

    pub(super) fn set_serving(&self, on: bool) {
        self.serving.store(on, Ordering::Relaxed);
    }

    /// The last `elapsed` of hosting, or `None` while not serving guests.
    ///
    /// Read every second whether or not we serve: the library's per-path
    /// entries are pruned only by a read, and a reading skipped would land
    /// in the next second as a burst.
    pub(super) fn sample(&self, elapsed: Duration, estimate: Option<u32>) -> Option<Hosting> {
        let Ok(mut state) = self.state.lock() else { return None };
        let state = &mut *state;
        self.registry.report(&mut state.report);
        let rows: Vec<Row> = state
            .report
            .traffic
            .iter()
            .map(|entry| Row {
                path: entry.path.as_str().to_string(),
                tier: entry.tier.as_str().to_string(),
                bytes: entry.publisher.bytes,
            })
            .collect();
        let connected = state
            .report
            .sessions
            .iter()
            .filter(|entry| entry.tier.as_str().starts_with(GUEST) && entry.presence.active() > 0)
            .map(|entry| entry.tier.as_str().to_string())
            .collect::<HashSet<_>>()
            .len() as u32;
        let moved = state.counts.fold(&rows, &self.me);
        if !self.serving.load(Ordering::Relaxed) {
            return None;
        }
        let hosting = moved.rates(elapsed, connected);
        for event in state.tally.observe(hosting, estimate, elapsed) {
            match event {
                Event::Over { .. } => tracing::warn!("{event}"),
                _ => tracing::info!("{event}"),
            }
        }
        Some(hosting)
    }

    /// The one line a call's hosting comes to, or `None` if we never hosted.
    pub(super) fn summary(&self) -> Option<String> {
        self.state.lock().ok().and_then(|state| state.tally.summary())
    }
}

/// One `(path, tier)` row of the library's report: egress payload so far.
#[derive(Clone, Debug)]
struct Row {
    path: String,
    tier: String,
    bytes: u64,
}

/// The last cumulative figure seen for each `(path, tier)`.
#[derive(Default)]
struct Counts {
    last: HashMap<(String, String), u64>,
}

/// Bytes handed out since the last read.
#[derive(Debug, Default, PartialEq, Eq)]
struct Moved {
    forwarded: u64,
    own: u64,
    /// Per guest tier, everything that guest was sent.
    guests: HashMap<String, u64>,
}

impl Counts {
    /// Turns cumulative rows into what moved since the last call.
    ///
    /// A figure lower than the last one is an entry the library pruned and
    /// made again, which restarts from zero: all of it is new. A row that
    /// is gone is forgotten, since its last reading was already counted.
    fn fold(&mut self, rows: &[Row], me: &str) -> Moved {
        let ours = format!("{me}/");
        let mut moved = Moved::default();
        let mut next = HashMap::with_capacity(rows.len());
        for row in rows {
            let key = (row.path.clone(), row.tier.clone());
            let delta = match self.last.get(&key) {
                Some(&last) if row.bytes >= last => row.bytes - last,
                _ => row.bytes,
            };
            next.insert(key, row.bytes);
            if row.path.starts_with(&ours) {
                moved.own += delta;
            } else {
                moved.forwarded += delta;
            }
            if row.tier.starts_with(GUEST) {
                *moved.guests.entry(row.tier.clone()).or_default() += delta;
            }
        }
        self.last = next;
        moved
    }
}

impl Moved {
    fn rates(&self, elapsed: Duration, guests: u32) -> Hosting {
        let kbps = |bytes: u64| kbps(bytes, elapsed);
        Hosting {
            forwarded: kbps(self.forwarded),
            own: kbps(self.own),
            guests,
            pulling: self.guests.values().filter(|bytes| kbps(**bytes) > MEDIA_KBPS).count() as u32,
        }
    }
}

fn kbps(bytes: u64, elapsed: Duration) -> u32 {
    let ms = elapsed.as_millis().max(1) as u64;
    // Bits per millisecond is kilobits per second.
    (bytes * 8 / ms).min(u32::MAX as u64) as u32
}

/// Whether we are forwarding, and the episode's running figures.
#[derive(Debug, Default)]
struct Tally {
    forwarding: bool,
    above: u32,
    below: u32,
    over: u32,
    warned: bool,
    hosted: Duration,
    /// Time in the forwarding state, and kilobits forwarded in it.
    forwarding_for: Duration,
    kilobits: f64,
    peak: u32,
    most_guests: u32,
}

/// What a second of hosting changed, in the log's words.
#[derive(Debug, PartialEq, Eq)]
enum Event {
    Started { kbps: u32, pulling: u32, guests: u32 },
    Stopped,
    Over { total: u32, estimate: u32 },
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Started { kbps, pulling, guests } => write!(
                f,
                "[host] forwarding started: {kbps} kbps to {pulling} of {guests} {}",
                plural(*guests, "guest", "guests")
            ),
            Event::Stopped => write!(f, "[host] forwarding stopped"),
            Event::Over { total, estimate } => write!(
                f,
                "[host] forwarded and own {total} kbps is over the link estimate of {estimate} kbps"
            ),
        }
    }
}

fn plural(n: u32, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

impl Tally {
    fn observe(&mut self, second: Hosting, estimate: Option<u32>, elapsed: Duration) -> Vec<Event> {
        let mut events = Vec::new();
        self.hosted += elapsed;
        self.most_guests = self.most_guests.max(second.guests);
        if second.forwarded > MEDIA_KBPS {
            self.above += 1;
            self.below = 0;
        } else {
            self.below += 1;
            self.above = 0;
        }
        if !self.forwarding && self.above >= STARTED_AFTER {
            self.forwarding = true;
            events.push(Event::Started {
                kbps: second.forwarded,
                pulling: second.pulling,
                guests: second.guests,
            });
        } else if self.forwarding && self.below >= STOPPED_AFTER {
            self.forwarding = false;
            self.over = 0;
            self.warned = false;
            events.push(Event::Stopped);
        }
        if self.forwarding {
            self.forwarding_for += elapsed;
            self.kilobits += second.forwarded as f64 * elapsed.as_secs_f64();
            self.peak = self.peak.max(second.forwarded);
            let total = second.forwarded.saturating_add(second.own);
            match estimate {
                Some(estimate) if total > estimate => {
                    self.over += 1;
                    if self.over >= OVER_AFTER && !self.warned {
                        self.warned = true;
                        events.push(Event::Over { total, estimate });
                    }
                }
                _ => {
                    self.over = 0;
                    self.warned = false;
                }
            }
        }
        events
    }

    fn summary(&self) -> Option<String> {
        if self.hosted.is_zero() {
            return None;
        }
        let hosted = self.hosted.as_secs();
        let guests = format!("at most {} {}", self.most_guests, plural(self.most_guests, "guest", "guests"));
        let forwarding = self.forwarding_for.as_secs_f64();
        Some(if forwarding < 1.0 {
            format!("[host] this call: nothing forwarded in {hosted} s hosting, {guests}")
        } else {
            format!(
                "[host] this call: forwarded {} kbps on average while forwarding ({} s of {hosted} s hosting), peak {} kbps, {guests}",
                (self.kilobits / forwarding).round() as u32,
                self.forwarding_for.as_secs(),
                self.peak,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn row(path: &str, tier: &str, bytes: u64) -> Row {
        Row {
            path: path.to_string(),
            tier: tier.to_string(),
            bytes,
        }
    }

    #[test]
    fn our_paths_are_ours_and_everyone_elses_is_forwarded() {
        let mut counts = Counts::default();
        let first = counts.fold(
            &[
                row("me/camera.hang", "guest/0", 1000),
                row("a/camera.hang", "guest/1", 4000),
                row("a/camera.hang", "out", 500),
                // Someone whose id merely starts like ours is not us.
                row("meadow/camera.hang", "out", 100),
            ],
            "me",
        );
        assert_eq!(first.own, 1000);
        assert_eq!(first.forwarded, 4600);
        assert_eq!(first.guests.get("guest/0"), Some(&1000));
        assert_eq!(first.guests.get("guest/1"), Some(&4000));
        assert_eq!(first.guests.get("out"), None);

        let second = counts.fold(&[row("me/camera.hang", "guest/0", 1500), row("a/camera.hang", "guest/1", 4000)], "me");
        assert_eq!((second.own, second.forwarded), (500, 0));
    }

    #[test]
    fn a_counter_that_went_back_is_a_fresh_one() {
        let mut counts = Counts::default();
        counts.fold(&[row("a/camera.hang", "guest/0", 9000)], "me");
        let after = counts.fold(&[row("a/camera.hang", "guest/0", 300)], "me");
        assert_eq!(after.forwarded, 300);
        // Gone and back: counted from zero again, not against the old figure.
        counts.fold(&[], "me");
        let again = counts.fold(&[row("a/camera.hang", "guest/0", 9500)], "me");
        assert_eq!(again.forwarded, 9500);
    }

    #[test]
    fn only_a_guest_sent_media_is_pulling() {
        let moved = Moved {
            forwarded: 50_000,
            own: 0,
            guests: HashMap::from([("guest/0".to_string(), 50_000), ("guest/1".to_string(), 200)]),
        };
        let hosting = moved.rates(SECOND, 2);
        assert_eq!(hosting, Hosting { forwarded: 400, own: 0, guests: 2, pulling: 1 });
    }

    fn second(forwarded: u32, own: u32) -> Hosting {
        Hosting { forwarded, own, guests: 2, pulling: 1 }
    }

    #[test]
    fn forwarding_starts_after_two_seconds_and_stops_after_five() {
        let mut tally = Tally::default();
        assert!(tally.observe(second(300, 0), None, SECOND).is_empty());
        let started = tally.observe(second(340, 0), None, SECOND);
        assert_eq!(started, vec![Event::Started { kbps: 340, pulling: 1, guests: 2 }]);
        assert_eq!(started[0].to_string(), "[host] forwarding started: 340 kbps to 1 of 2 guests");
        for _ in 0..4 {
            assert!(tally.observe(second(0, 0), None, SECOND).is_empty());
        }
        assert_eq!(tally.observe(second(0, 0), None, SECOND), vec![Event::Stopped]);
        assert_eq!(Event::Stopped.to_string(), "[host] forwarding stopped");
    }

    #[test]
    fn going_over_the_estimate_is_said_once_per_episode() {
        let mut tally = Tally::default();
        tally.observe(second(900, 0), Some(1000), SECOND);
        tally.observe(second(900, 0), Some(1000), SECOND);
        let mut said = Vec::new();
        for _ in 0..10 {
            said.extend(tally.observe(second(900, 200), Some(1000), SECOND));
        }
        assert_eq!(said, vec![Event::Over { total: 1100, estimate: 1000 }]);
        assert_eq!(
            said[0].to_string(),
            "[host] forwarded and own 1100 kbps is over the link estimate of 1000 kbps"
        );
        // Back under, then over again: a new episode.
        tally.observe(second(500, 0), Some(1000), SECOND);
        let mut again = Vec::new();
        for _ in 0..5 {
            again.extend(tally.observe(second(900, 200), Some(1000), SECOND));
        }
        assert_eq!(again.len(), 1);
    }

    #[test]
    fn the_summary_says_what_hosting_came_to() {
        assert_eq!(Tally::default().summary(), None);

        let mut idle = Tally::default();
        for _ in 0..120 {
            idle.observe(second(2, 1), None, SECOND);
        }
        assert_eq!(
            idle.summary().unwrap(),
            "[host] this call: nothing forwarded in 120 s hosting, at most 2 guests"
        );

        let mut busy = Tally::default();
        for forwarded in [300, 300, 400, 300] {
            busy.observe(second(forwarded, 0), None, SECOND);
        }
        assert_eq!(
            busy.summary().unwrap(),
            "[host] this call: forwarded 333 kbps on average while forwarding (3 s of 4 s hosting), peak 400 kbps, at most 2 guests"
        );
    }
}
