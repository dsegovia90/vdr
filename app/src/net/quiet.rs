//! Noticing that one way into the room has gone quiet while the others still
//! talk, so a join can leave a relay that died without saying so before the
//! transport gives up on it.
//!
//! **Why this exists.** A relay that is killed, crashes, or loses its network
//! sends no close: our session to it simply stops hearing anything, and the
//! transport declares it dead only at the idle timeout, ten seconds later.
//! Until then the relay still holds the best route to everyone, so every
//! video freezes for those ten seconds although each member already holds a
//! working session to a host. The spare offer makes what follows a splice;
//! this makes it come sooner.
//!
//! **The rule is relative.** A session is quiet when it has received nothing
//! for its limit ([`limit`]) *while another session of ours has been heard*,
//! at least [`AFTER`] past the quiet one's last byte. If every session is
//! silent the fault is our own link (a stalled uplink, a laptop lid, a
//! roaming phone), leaving any of them would gain nothing, and nothing is
//! left. Evidence is any other live session, and, when we host, our guests'
//! sessions to us ([`Heard`]).
//!
//! Two guards keep a link that stalls and recovers from passing for a relay
//! that died, because after a stall each session starts hearing again at its
//! own moment (its own retransmission timer), up to a second or two apart:
//!
//! - The evidence must be *steady*: a session that was itself silent for its
//!   own limit since the quiet one's last byte is recovering from the same
//!   stall, not evidence that only one way died.
//! - The quiet session gets [`GRACE`] from the first evidence to speak again.
//!
//! **The limits** come from how often a healthy session is guaranteed to hear
//! something with nothing else to say: its keep-alive. A raw QUIC session
//! (an `http(s)` relay) pings every [`KEEP_ALIVE`], which a join sets on its
//! client config, so two seconds of silence means a ping went unanswered. An
//! iroh session pings every five seconds and that cannot be changed through
//! moq-tokio (its iroh backend has no keep-alive knob; iroh 1.1's default is
//! its five-second heartbeat), so its limit is six. A host judging its own
//! relay waits a second longer than its guests do ([`HOSTING_EXTRA`]), so that
//! when the relay dies the members have moved their media to the host before
//! the host itself lets go of the relay.
//!
//! **Why not just lower the idle timeout.** The idle timeout is absolute: it
//! ends a session that hears nothing, whatever the others do, and a call
//! needs it long. A train tunnel, a Wi-Fi roam or a stalled mobile uplink is
//! silence everywhere for several seconds, and a call that redials every way
//! in at once during it comes back slower than one that waited. Only a
//! comparison tells a dead relay from a dead link, and it is the comparison
//! that makes a short limit safe.
//!
//! **Only the relay is judged; a host's session is only evidence.** A host
//! is reached over iroh, whose five-second heartbeat cannot be shortened from
//! here, so a host that froze would be left at the earliest a second or two
//! before its idle timeout ends it anyway, and a heartbeat lost on a working
//! host would leave it for nothing: its spare goes, it rests fifteen seconds,
//! and a call carried there moves. Not worth it.
//!
//! **Known limit: a relay that is alive but no longer forwarding** (a black
//! hole) still answers our keep-alives, so it is never quiet and is not left.
//! Telling that apart needs the content, not the transport: someone's
//! presence going silent on one session while it arrives on another. Not
//! built.
//!
//! Everything here is pure: the join samples each session's received bytes
//! on its own tick and asks.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How often a join's raw QUIC sessions ping, so that a healthy one is never
/// silent much longer than this.
pub(crate) const KEEP_ALIVE: Duration = Duration::from_secs(1);

/// The silence after which a raw QUIC session (an `http(s)` relay) is quiet:
/// two keep-alives.
pub(crate) const QUIET_QUIC: Duration = Duration::from_secs(2);

/// The same for an iroh session: one of iroh's five-second heartbeats, and a
/// second for its answer.
pub(crate) const QUIET_IROH: Duration = Duration::from_secs(6);

/// What a host adds to every limit, so that its members leave a dead relay
/// first.
pub(crate) const HOSTING_EXTRA: Duration = Duration::from_secs(1);

/// How long after a quiet session's last byte another must have been heard to
/// count: a moment both heard together says nothing.
pub(crate) const AFTER: Duration = Duration::from_secs(1);

/// How long a quiet session has, from the first evidence against it, to speak
/// again: what separates a session still recovering from a stall from one
/// that died.
pub(crate) const GRACE: Duration = Duration::from_secs(1);

/// How much of each session's history is kept: longer than any limit.
const MEMORY: Duration = Duration::from_secs(30);

/// The silence after which a session to `url` is quiet, by its transport.
pub(crate) fn limit(url: &url::Url, hosting: bool) -> Duration {
    let base = if url.scheme() == "iroh" { QUIET_IROH } else { QUIET_QUIC };
    if hosting { base + HOSTING_EXTRA } else { base }
}

/// When one source was heard, as far back as [`MEMORY`].
struct Watch {
    /// The last byte count read, to tell when it moves.
    bytes: Option<u64>,
    /// When the count was seen to move, oldest first. Never empty.
    heard: VecDeque<Instant>,
    /// This source's own limit: how long it may be silent and still be well.
    limit: Duration,
    /// Whether this source may be found quiet, or only counts as evidence.
    judged: bool,
}

impl Watch {
    fn new(at: Instant, bytes: Option<u64>, limit: Duration, judged: bool) -> Self {
        Self {
            bytes,
            heard: VecDeque::from([at]),
            limit,
            judged,
        }
    }

    fn last(&self) -> Instant {
        *self.heard.back().expect("a watch is made with one")
    }

    fn push(&mut self, at: Instant) {
        if at > self.last() {
            self.heard.push_back(at);
        }
        while self.heard.len() > 1 && self.heard.front().is_some_and(|first| at.saturating_duration_since(*first) > MEMORY) {
            self.heard.pop_front();
        }
    }

    /// The first time this source was heard at or after `from`, if it was
    /// heard steadily from `since` on: never silent for its own limit, now
    /// included.
    fn steady_from(&self, since: Instant, from: Instant, now: Instant) -> Option<Instant> {
        if now.saturating_duration_since(self.last()) >= self.limit {
            return None;
        }
        let mut previous: Option<Instant> = None;
        for &at in &self.heard {
            if let Some(before) = previous {
                if at > since && at.duration_since(before) >= self.limit {
                    return None;
                }
            }
            previous = Some(at);
        }
        self.heard.iter().copied().find(|&at| at >= from)
    }
}

/// Which session was found quiet, for how long, and what was heard instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Verdict {
    pub(crate) quiet: usize,
    pub(crate) silent: Duration,
    pub(crate) heard: usize,
}

/// Each source's history, by the index the join gives it: its sessions in
/// order, and one past them for its guests.
#[derive(Default)]
pub(crate) struct Quiet {
    watched: Vec<Option<Watch>>,
}

impl Quiet {
    fn slot(&mut self, index: usize) -> &mut Option<Watch> {
        if self.watched.len() <= index {
            self.watched.resize_with(index + 1, || None);
        }
        &mut self.watched[index]
    }

    /// A session's received bytes as read `now`. A session first seen is
    /// heard now: it has just answered. A count that changes, either way, is
    /// hearing (a smaller one is a new session). One not `judged` is only
    /// ever evidence.
    pub(crate) fn observe(&mut self, index: usize, bytes: Option<u64>, limit: Duration, judged: bool, now: Instant) {
        let slot = self.slot(index);
        match slot {
            None => *slot = Some(Watch::new(now, bytes, limit, judged)),
            Some(watch) => {
                watch.limit = limit;
                watch.judged = judged;
                if bytes.is_some() && bytes != watch.bytes {
                    watch.bytes = bytes;
                    watch.push(now);
                }
            }
        }
    }

    /// Evidence only, never judged: `at`, when something else of ours was
    /// last heard (our guests' sessions, when we host).
    pub(crate) fn heard(&mut self, index: usize, at: Option<Instant>, limit: Duration) {
        let Some(at) = at else { return };
        let slot = self.slot(index);
        match slot {
            None => *slot = Some(Watch::new(at, None, limit, false)),
            Some(watch) => {
                watch.limit = limit;
                watch.push(at);
            }
        }
    }

    /// Stops watching a source: it is down, or it is new and starts afresh.
    pub(crate) fn forget(&mut self, index: usize) {
        if let Some(slot) = self.watched.get_mut(index) {
            *slot = None;
        }
    }

    /// The first session that has gone quiet while another was heard, if any.
    pub(crate) fn quiet(&self, now: Instant) -> Option<Verdict> {
        for (index, watch) in self.watched.iter().enumerate() {
            let Some(watch) = watch.as_ref().filter(|watch| watch.judged) else { continue };
            let last = watch.last();
            let silent = now.saturating_duration_since(last);
            if silent < watch.limit {
                continue;
            }
            for (other, evidence) in self.watched.iter().enumerate() {
                let Some(evidence) = evidence.as_ref().filter(|_| other != index) else { continue };
                let Some(first) = evidence.steady_from(last, last + AFTER, now) else { continue };
                if now >= first + GRACE {
                    return Some(Verdict {
                        quiet: index,
                        silent,
                        heard: other,
                    });
                }
            }
        }
        None
    }
}

/// When any of our guests' sessions last received anything: a host's
/// evidence that its own link works while its relay is silent. Written by
/// each accepted session's sampler, read on the join's tick.
#[derive(Default)]
pub(crate) struct Heard {
    last: std::sync::Mutex<Option<Instant>>,
}

impl Heard {
    pub(crate) fn heard(&self, at: Instant) {
        if let Ok(mut last) = self.last.lock() {
            if last.is_none_or(|last| at > last) {
                *last = Some(at);
            }
        }
    }

    pub(crate) fn last(&self) -> Option<Instant> {
        self.last.lock().ok().and_then(|last| *last)
    }
}

/// How long a session we left as quiet must hold, once it is back, before our
/// media moves up to it again: [`Hold::BASE`], doubling with each time it is
/// left again within [`Hold::CHAIN`] of the last, to [`Hold::MOST`], and back
/// to the base once it has held for [`Hold::MOST`]. A relay that keeps
/// stalling then drags the call back and forth less and less often.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hold {
    hold: Duration,
    left: Option<Instant>,
}

impl Hold {
    pub(crate) const BASE: Duration = Duration::from_secs(2);
    pub(crate) const MOST: Duration = Duration::from_secs(60);
    pub(crate) const CHAIN: Duration = Duration::from_secs(120);

    pub(crate) fn new() -> Self {
        Self {
            hold: Self::BASE,
            left: None,
        }
    }

    /// The session was left as quiet `now`.
    pub(crate) fn left(&mut self, now: Instant) {
        self.hold = match self.left {
            Some(before) if now.saturating_duration_since(before) <= Self::CHAIN => (self.hold * 2).min(Self::MOST),
            _ => Self::BASE,
        };
        self.left = Some(now);
    }

    /// The session has been up since `since`: once that is [`Hold::MOST`], it
    /// has proved itself and starts again from the base.
    pub(crate) fn up(&mut self, since: Instant, now: Instant) {
        if now.saturating_duration_since(since) >= Self::MOST {
            *self = Self::new();
        }
    }

    pub(crate) fn hold(&self) -> Duration {
        self.hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = QUIET_QUIC;

    fn ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    /// Samples `index` as heard every `every` ms from `from` to `to`, with
    /// a growing byte count.
    fn talk(quiet: &mut Quiet, index: usize, start: Instant, from: u64, to: u64, every: u64) {
        let mut at = from;
        while at <= to {
            quiet.observe(index, Some(at + 1), T, true, ms(start, at));
            at += every;
        }
    }

    /// Samples `index` as unchanged from `from` to `to`.
    fn hush(quiet: &mut Quiet, index: usize, start: Instant, from: u64, to: u64, bytes: u64) {
        let mut at = from;
        while at <= to {
            quiet.observe(index, Some(bytes), T, true, ms(start, at));
            at += 250;
        }
    }

    #[test]
    fn a_session_silent_while_another_is_heard_is_quiet() {
        let start = Instant::now();
        let mut quiet = Quiet::default();
        talk(&mut quiet, 0, start, 0, 1000, 250);
        talk(&mut quiet, 1, start, 0, 4000, 250);
        hush(&mut quiet, 0, start, 1250, 4000, 1001);
        // Last heard at 1 s: quiet from 3 s, evidence from 2 s, grace to 3 s.
        assert_eq!(quiet.quiet(ms(start, 2900)), None, "not yet its limit");
        let verdict = quiet.quiet(ms(start, 3000)).expect("quiet");
        assert_eq!((verdict.quiet, verdict.heard), (0, 1));
        assert_eq!(verdict.silent, Duration::from_secs(2));
    }

    #[test]
    fn when_everything_is_silent_nothing_is_quiet() {
        let start = Instant::now();
        let mut quiet = Quiet::default();
        talk(&mut quiet, 0, start, 0, 1000, 250);
        talk(&mut quiet, 1, start, 0, 1000, 250);
        hush(&mut quiet, 0, start, 1250, 9000, 1001);
        hush(&mut quiet, 1, start, 1250, 9000, 1001);
        assert_eq!(quiet.quiet(ms(start, 9000)), None, "our own link: nobody is left");
    }

    #[test]
    fn evidence_only_from_before_the_silence_is_none() {
        let start = Instant::now();
        let mut quiet = Quiet::default();
        talk(&mut quiet, 0, start, 0, 1000, 250);
        // The other is heard until 1.5 s, under a second past the first's
        // last byte, then nothing.
        talk(&mut quiet, 1, start, 0, 1500, 250);
        hush(&mut quiet, 0, start, 1250, 5000, 1001);
        hush(&mut quiet, 1, start, 1750, 5000, 1501);
        assert_eq!(quiet.quiet(ms(start, 5000)), None);
    }

    #[test]
    fn a_session_recovering_from_the_same_stall_is_no_evidence() {
        let start = Instant::now();
        let mut quiet = Quiet::default();
        // Both stall from 1 s to 3.5 s; the other comes back first, the
        // first 600 ms later.
        talk(&mut quiet, 0, start, 0, 1000, 250);
        talk(&mut quiet, 1, start, 0, 1000, 250);
        hush(&mut quiet, 0, start, 1250, 4000, 1001);
        talk(&mut quiet, 1, start, 3500, 6000, 250);
        assert_eq!(quiet.quiet(ms(start, 4000)), None, "the other was silent too, for its own limit");
        talk(&mut quiet, 0, start, 4100, 6000, 250);
        assert_eq!(quiet.quiet(ms(start, 6000)), None);
    }

    #[test]
    fn a_quiet_session_gets_its_grace_after_the_first_evidence() {
        let start = Instant::now();
        let mut quiet = Quiet::default();
        // An iroh-like other, heard only every 5 s, first at 4 s.
        quiet.observe(1, Some(1), QUIET_IROH, false, ms(start, 0));
        talk(&mut quiet, 0, start, 0, 1000, 250);
        hush(&mut quiet, 0, start, 1250, 4500, 1001);
        quiet.observe(1, Some(2), QUIET_IROH, false, ms(start, 4000));
        assert_eq!(quiet.quiet(ms(start, 4500)), None, "within its grace");
        assert_eq!(quiet.quiet(ms(start, 5000)).map(|verdict| verdict.quiet), Some(0));
    }

    #[test]
    fn a_hosts_guests_are_evidence_and_are_never_judged() {
        let start = Instant::now();
        let mut quiet = Quiet::default();
        let limit = QUIET_QUIC + HOSTING_EXTRA;
        quiet.observe(0, Some(1), limit, true, ms(start, 0));
        quiet.heard(1, Some(ms(start, 0)), QUIET_IROH);
        for at in (250..=6000).step_by(250) {
            quiet.observe(0, Some(1), limit, true, ms(start, at));
        }
        // The guests went silent too: still our own link.
        assert_eq!(quiet.quiet(ms(start, 6000)), None);
        quiet.heard(1, Some(ms(start, 2000)), QUIET_IROH);
        quiet.heard(1, Some(ms(start, 4500)), QUIET_IROH);
        let verdict = quiet.quiet(ms(start, 6000)).expect("the relay is quiet while guests talk");
        assert_eq!((verdict.quiet, verdict.heard), (0, 1));
        assert_eq!(quiet.quiet(ms(start, 2900)), None, "the host waits its extra second");
    }

    /// Our own pass stalling is not the relay going quiet. Nothing is
    /// sampled for four seconds while our guests' sessions, timed by their
    /// own samplers, are heard throughout; the relay heard things meanwhile
    /// too, and the first pass after the stall sees its count moved. It is
    /// heard at that pass, however long the gap since the last one.
    #[test]
    fn a_late_pass_is_not_silence() {
        let start = Instant::now();
        let mut quiet = Quiet::default();
        let limit = QUIET_QUIC + HOSTING_EXTRA;
        for at in (0..=1000).step_by(250) {
            quiet.observe(0, Some(at + 1), limit, true, ms(start, at));
        }
        for at in (0..=5000).step_by(250) {
            quiet.heard(1, Some(ms(start, at)), QUIET_IROH);
        }
        // The pass after the stall: samples first, then asks, as the join does.
        quiet.observe(0, Some(9_999), limit, true, ms(start, 5000));
        assert_eq!(quiet.quiet(ms(start, 5000)), None, "heard during the stall");
        // And a relay that truly heard nothing across the same stall is quiet.
        let mut silent = Quiet::default();
        for at in (0..=1000).step_by(250) {
            silent.observe(0, Some(at + 1), limit, true, ms(start, at));
        }
        for at in (0..=5000).step_by(250) {
            silent.heard(1, Some(ms(start, at)), QUIET_IROH);
        }
        silent.observe(0, Some(1001), limit, true, ms(start, 5000));
        assert_eq!(silent.quiet(ms(start, 5000)).map(|verdict| verdict.quiet), Some(0));
    }

    #[test]
    fn a_session_only_heard_as_evidence_is_never_found_quiet() {
        let start = Instant::now();
        let mut quiet = Quiet::default();
        talk(&mut quiet, 0, start, 0, 9000, 250);
        quiet.observe(1, Some(1), QUIET_IROH, false, ms(start, 0));
        quiet.observe(1, Some(1), QUIET_IROH, false, ms(start, 9000));
        assert_eq!(quiet.quiet(ms(start, 9000)), None);
    }

    #[test]
    fn a_forgotten_session_starts_afresh() {
        let start = Instant::now();
        let mut quiet = Quiet::default();
        talk(&mut quiet, 1, start, 0, 6000, 250);
        quiet.observe(0, Some(5), T, true, ms(start, 0));
        quiet.forget(0);
        quiet.observe(0, Some(5), T, true, ms(start, 5000));
        assert_eq!(quiet.quiet(ms(start, 6000)), None, "watched again from 5 s");
    }

    #[test]
    fn limits_follow_the_transport_and_a_host_waits_longer() {
        let relay: url::Url = "https://relay.example/team".parse().unwrap();
        let iroh: url::Url = "iroh://abc/team".parse().unwrap();
        assert_eq!(limit(&relay, false), Duration::from_secs(2));
        assert_eq!(limit(&relay, true), Duration::from_secs(3));
        assert_eq!(limit(&iroh, false), Duration::from_secs(6));
        assert_eq!(limit(&iroh, true), Duration::from_secs(7));
    }

    #[test]
    fn the_hold_doubles_with_each_leave_inside_two_minutes_and_resets_once_it_held() {
        let start = Instant::now();
        let at = |s: u64| start + Duration::from_secs(s);
        let mut hold = Hold::new();
        assert_eq!(hold.hold(), Duration::from_secs(2));
        hold.left(at(0));
        assert_eq!(hold.hold(), Duration::from_secs(2), "the first leave");
        let mut expected = 2;
        for n in 1..8 {
            hold.left(at(n * 30));
            expected = (expected * 2).min(60);
            assert_eq!(hold.hold(), Duration::from_secs(expected), "leave {n}");
        }
        assert_eq!(hold.hold(), Duration::from_secs(60), "capped");
        hold.up(at(300), at(359));
        assert_eq!(hold.hold(), Duration::from_secs(60), "not held long enough");
        hold.up(at(300), at(360));
        assert_eq!(hold.hold(), Duration::from_secs(2), "held a minute: back to the base");
        hold.left(at(400));
        hold.left(at(521));
        assert_eq!(hold.hold(), Duration::from_secs(2), "over two minutes apart: no chain");
    }
}
