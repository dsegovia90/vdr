//! Retractions held back a moment, so a change of route is never shown as a
//! person leaving.
//!
//! **The library ends a read whenever its route changes, and says so with a
//! bounce**: the path is retracted and at once announced again, and whoever
//! holds the old broadcast asks for a fresh one. It never carries a read
//! across to another route, because two routes to one path can carry two
//! different broadcasts. So every relay going, every move of the call and
//! every spare taking over reaches the attendance loop as a retraction then
//! an announcement of the same person. Acted on as it arrives, that is a
//! tile leaving and coming back, a presence forgotten and heard again, and,
//! in a call that is not a room, a hang-up.
//!
//! So a retraction starts a [`GRACE`] instead, and an announcement of the
//! same thing within it is a change of route: the readers are started again
//! on the new broadcast and nothing else changes. Only a grace that runs out
//! is a leave. A goodbye said in presence ends every grace of theirs at once,
//! so a deliberate leave is as quick as it ever was; an abrupt one already
//! waits out the session's linger, and now this as well.
//!
//! Pure bookkeeping, so the rule is tested without a network.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a retraction waits for its announcement to return. A bounce's
/// two halves arrive together, a relay's forwarded one a round trip later,
/// but a route that ranks worse than the one lost is held back by the
/// library for two seconds after a retraction (its quarantine of a stale
/// path, read in moq-net's source): this outlasts that, so even then the
/// route change is a freeze, not a leave.
pub(super) const GRACE: Duration = Duration::from_millis(2500);

/// What of a person's a retraction was for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Strand {
    /// Their control broadcast: their presence, and so their being here.
    Presence,
    Camera,
    Screen,
}

/// The retractions waiting out their grace, by person and strand, with when
/// each was retracted.
#[derive(Debug, Default)]
pub(super) struct Departures {
    due: HashMap<(String, Strand), Instant>,
}

impl Departures {
    /// `id`'s `strand` was retracted at `now`. A second retraction before
    /// the announcement returns keeps the first deadline, so a route that
    /// keeps bouncing cannot hold a leave off for ever.
    pub(super) fn retracted(&mut self, id: &str, strand: Strand, now: Instant) {
        self.due.entry((id.to_string(), strand)).or_insert(now);
    }

    /// `id`'s `strand` was announced. When that ends a grace, which makes it
    /// a change of route rather than an arrival, when the retraction was.
    pub(super) fn announced(&mut self, id: &str, strand: Strand) -> Option<Instant> {
        self.due.remove(&(id.to_string(), strand))
    }

    /// Whether `id`'s `strand` is waiting out a grace.
    #[cfg(test)]
    pub(super) fn waiting(&self, id: &str, strand: Strand) -> bool {
        self.due.contains_key(&(id.to_string(), strand))
    }

    /// Ends `id`'s grace for `strand` now, if it had one: whether it did.
    pub(super) fn release(&mut self, id: &str, strand: Strand) -> bool {
        self.due.remove(&(id.to_string(), strand)).is_some()
    }

    /// Every grace of `id`'s ends now: they said goodbye, or their presence
    /// went, and nothing of theirs is coming back on another route.
    pub(super) fn release_all(&mut self, id: &str) -> Vec<Strand> {
        let mut strands: Vec<Strand> = self.due.keys().filter(|(who, _)| who == id).map(|(_, strand)| *strand).collect();
        for strand in &strands {
            self.due.remove(&(id.to_string(), *strand));
        }
        // Presence last, so a caller acting in order takes their media down
        // before deciding whether the call is over.
        strands.sort_by_key(|strand| *strand == Strand::Presence);
        strands
    }

    /// When the next grace runs out.
    pub(super) fn next(&self) -> Option<Instant> {
        self.due.values().min().map(|at| *at + GRACE)
    }

    /// The graces that have run out by `now`, taken: each is a leave.
    pub(super) fn expired(&mut self, now: Instant) -> Vec<(String, Strand)> {
        let mut gone: Vec<(String, Strand)> =
            self.due.iter().filter(|(_, at)| **at + GRACE <= now).map(|(key, _)| key.clone()).collect();
        for key in &gone {
            self.due.remove(key);
        }
        gone.sort_by_key(|(id, strand)| (*strand == Strand::Presence, id.clone()));
        gone
    }

    /// The people with a grace running for `strand`.
    pub(super) fn of(&self, strand: Strand) -> Vec<String> {
        self.due.keys().filter(|(_, of)| *of == strand).map(|(id, _)| id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bounce_is_no_leave() {
        let mut departures = Departures::default();
        let now = Instant::now();
        departures.retracted("a", Strand::Camera, now);
        assert!(departures.waiting("a", Strand::Camera));
        assert_eq!(departures.announced("a", Strand::Camera), Some(now), "the return ends the grace");
        assert!(departures.expired(now + GRACE * 2).is_empty());
        assert_eq!(departures.next(), None);
    }

    #[test]
    fn an_arrival_with_no_retraction_before_it_is_not_a_return() {
        let mut departures = Departures::default();
        assert_eq!(departures.announced("a", Strand::Camera), None);
    }

    #[test]
    fn a_grace_that_runs_out_is_a_leave() {
        let mut departures = Departures::default();
        let now = Instant::now();
        departures.retracted("a", Strand::Camera, now);
        assert_eq!(departures.next(), Some(now + GRACE));
        assert!(departures.expired(now + GRACE - Duration::from_millis(1)).is_empty());
        assert_eq!(departures.expired(now + GRACE), vec![("a".to_string(), Strand::Camera)]);
        assert_eq!(departures.announced("a", Strand::Camera), None, "a return after the leave is a new arrival");
    }

    #[test]
    fn a_route_that_keeps_bouncing_keeps_its_first_deadline() {
        let mut departures = Departures::default();
        let now = Instant::now();
        departures.retracted("a", Strand::Camera, now);
        departures.retracted("a", Strand::Camera, now + Duration::from_secs(2));
        assert_eq!(departures.next(), Some(now + GRACE));
    }

    #[test]
    fn strands_and_people_wait_apart() {
        let mut departures = Departures::default();
        let now = Instant::now();
        departures.retracted("a", Strand::Camera, now);
        departures.retracted("a", Strand::Screen, now);
        departures.retracted("b", Strand::Camera, now);
        assert!(departures.announced("a", Strand::Screen).is_some());
        assert!(departures.waiting("a", Strand::Camera));
        assert!(departures.waiting("b", Strand::Camera));
        assert_eq!(departures.of(Strand::Screen), Vec::<String>::new());
    }

    #[test]
    fn a_goodbye_ends_every_grace_of_theirs_presence_last() {
        let mut departures = Departures::default();
        let now = Instant::now();
        departures.retracted("a", Strand::Presence, now);
        departures.retracted("a", Strand::Camera, now);
        departures.retracted("a", Strand::Screen, now);
        departures.retracted("b", Strand::Camera, now);
        let strands = departures.release_all("a");
        assert_eq!(strands.len(), 3);
        assert_eq!(strands.last(), Some(&Strand::Presence));
        assert!(departures.waiting("b", Strand::Camera));
        assert!(!departures.release("a", Strand::Camera));
    }

    #[test]
    fn expiries_put_presence_after_media() {
        let mut departures = Departures::default();
        let now = Instant::now();
        departures.retracted("a", Strand::Presence, now);
        departures.retracted("a", Strand::Camera, now);
        let gone = departures.expired(now + GRACE);
        assert_eq!(gone, vec![("a".to_string(), Strand::Camera), ("a".to_string(), Strand::Presence)]);
    }
}
