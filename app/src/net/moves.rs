//! Moving the call, an experimental instrument: where the room's media should
//! go, as one person said it, and the rule every app with the switch on uses
//! to agree on it.
//!
//! **Routes are chosen per viewer and offers per publisher**, so no one app
//! can move a call by itself. The choice is said in presence instead, and
//! every app that has the switch on applies it to what it controls: where its
//! own media is offered, and what its link to the chosen host costs (see
//! `call::sessions`). Apps with the switch off, and builds that predate it,
//! ignore the field and stay on the relay; the chosen host bridges them in.
//!
//! **The newest choice wins: the highest turn, then the lowest id.** A turn is
//! a counter, one past the newest heard, not a clock, so two machines whose
//! clocks disagree still agree on the winner. Every following app repeats the
//! winner in its own presence, so a choice outlives the person who made it
//! and someone joining late hears it from anyone.
//!
//! **Anyone in the room can pin it**, by sending the largest turn we accept.
//! Accepted while this is an experiment behind a switch: the turn is capped at
//! [`MAX_TURN`] so that one past it cannot overflow, and a choice at the cap
//! can still be overridden by a lower id.

use std::time::Duration;

/// The largest turn we follow or repeat: far beyond any real number of
/// moves, and small enough to survive a trip through a JSON number as an
/// `f64` exactly.
pub(crate) const MAX_TURN: u64 = 1 << 53;

/// The shortest bounce we will do. A move takes a redial of the host link
/// (about a second), the one-second overlap of the old offer, and up to two
/// seconds of the library holding back a route it just retracted, plus the
/// time presence takes to reach everyone; a bounce faster than this would
/// measure moves that never finished.
pub(crate) const BOUNCE_MIN: Duration = Duration::from_secs(10);

/// The bounce when `--bounce` is given no usable number.
pub(crate) const BOUNCE_DEFAULT: Duration = Duration::from_secs(30);

/// Where the call is, as someone said it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CallOn {
    /// The host the call is on, by endpoint id. Absent is the relay, which
    /// is also the automatic order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) to: Option<String>,
    /// The turn: one past the newest choice its sayer had heard.
    pub(crate) n: u64,
    /// Who chose it. A claim like a name, used only to break a tie.
    pub(crate) by: String,
}

impl CallOn {
    /// As we keep or send it: well-formed ids and a turn within the cap, or
    /// nothing at all.
    pub(crate) fn clean(self) -> Option<Self> {
        let ids = super::quality::valid_id(&self.by) && self.to.as_deref().is_none_or(super::quality::valid_id);
        (ids && self.n <= MAX_TURN).then_some(self)
    }

    /// Whether this choice replaces `other`.
    fn beats(&self, other: &Self) -> bool {
        self.n > other.n || (self.n == other.n && self.by < other.by)
    }
}

/// Keeps whichever of `current` and `heard` wins, and says whether that
/// changed what we follow.
pub(crate) fn hear(current: &mut Option<CallOn>, heard: CallOn) -> bool {
    if current.as_ref().is_some_and(|current| !heard.beats(current)) {
        return false;
    }
    *current = Some(heard);
    true
}

/// A new choice of ours, made to win over `current`.
pub(crate) fn choose(current: Option<&CallOn>, to: Option<String>, me: &str) -> CallOn {
    CallOn {
        to,
        n: current.map_or(1, |current| current.n.saturating_add(1).min(MAX_TURN)),
        by: me.to_string(),
    }
}

/// A presence field read leniently: anything unreadable, or anything that
/// does not clean, is no choice, never a failed document.
pub(crate) fn lenient<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<CallOn>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    Ok(serde_json::from_value::<CallOn>(value).ok().and_then(CallOn::clean))
}

/// Where a bounce goes next: the stop after `now` in `stops` (the relay, then
/// the hosts, in the order sessions are), round and round.
pub(crate) fn next_stop(stops: &[Option<String>], now: Option<&str>) -> Option<String> {
    let at = stops.iter().position(|stop| stop.as_deref() == now);
    let next = at.map_or(0, |at| (at + 1) % stops.len().max(1));
    stops.get(next).cloned().flatten()
}

/// How often to bounce, from what `--bounce` said: never under
/// [`BOUNCE_MIN`].
pub(crate) fn bounce_every(secs: Option<u64>) -> Duration {
    secs.map_or(BOUNCE_DEFAULT, Duration::from_secs).max(BOUNCE_MIN)
}

/// What `--move-call` says: `relay`, or a host's endpoint id.
pub(crate) fn parse_target(text: &str) -> Result<Option<String>, String> {
    match text {
        "relay" => Ok(None),
        id if super::quality::valid_id(id) => Ok(Some(id.to_string())),
        _ => Err("is neither relay nor an endpoint id".to_string()),
    }
}

/// Where the call is, for a log line.
pub(crate) fn describe(to: Option<&str>) -> String {
    match to {
        None => "the relay".to_string(),
        Some(id) => format!("host {id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    fn on(to: Option<char>, n: u64, by: char) -> CallOn {
        CallOn {
            to: to.map(id),
            n,
            by: id(by),
        }
    }

    #[test]
    fn the_highest_turn_wins_then_the_lowest_id() {
        let mut current = None;
        assert!(hear(&mut current, on(Some('c'), 2, 'b')));
        assert!(!hear(&mut current, on(None, 1, 'a')), "an older turn");
        assert!(hear(&mut current, on(None, 2, 'a')), "the same turn from a lower id");
        assert!(!hear(&mut current, on(Some('c'), 2, 'b')), "the same turn from a higher id");
        assert!(hear(&mut current, on(Some('c'), 3, 'f')));
        assert_eq!(current, Some(on(Some('c'), 3, 'f')));
    }

    #[test]
    fn hearing_our_own_winner_repeated_back_changes_nothing() {
        let mut current = Some(on(Some('c'), 4, 'b'));
        assert!(!hear(&mut current, on(Some('c'), 4, 'b')));
    }

    #[test]
    fn a_new_choice_wins_over_what_we_follow_and_the_turn_cannot_overflow() {
        let mine = choose(Some(&on(Some('c'), 4, 'a')), None, &id('f'));
        assert_eq!(mine, on(None, 5, 'f'));
        let mut current = Some(on(Some('c'), 4, 'a'));
        assert!(hear(&mut current, mine));
        assert_eq!(choose(None, None, &id('a')).n, 1);
        assert_eq!(choose(Some(&on(None, MAX_TURN, 'b')), None, &id('a')).n, MAX_TURN);
    }

    #[test]
    fn only_well_formed_choices_are_kept() {
        assert!(on(Some('c'), 1, 'a').clean().is_some());
        assert!(on(None, MAX_TURN, 'a').clean().is_some());
        assert!(on(None, MAX_TURN + 1, 'a').clean().is_none(), "past the cap");
        assert!(CallOn { to: Some("x".into()), n: 1, by: id('a') }.clean().is_none());
        assert!(CallOn { to: None, n: 1, by: "someone".into() }.clean().is_none());
    }

    #[test]
    fn a_bounce_goes_round_the_relay_and_every_host() {
        let stops = [None, Some(id('a')), Some(id('b'))];
        assert_eq!(next_stop(&stops, None), Some(id('a')));
        assert_eq!(next_stop(&stops, Some(&id('a'))), Some(id('b')));
        assert_eq!(next_stop(&stops, Some(&id('b'))), None);
        assert_eq!(next_stop(&stops, Some(&id('e'))), None, "somewhere we do not know starts over");
        assert_eq!(next_stop(&[None], None), None);
    }

    #[test]
    fn a_bounce_is_never_faster_than_a_move_can_finish() {
        assert_eq!(bounce_every(Some(3)), BOUNCE_MIN);
        assert_eq!(bounce_every(Some(20)), Duration::from_secs(20));
        assert_eq!(bounce_every(None), BOUNCE_DEFAULT);
    }

    #[test]
    fn move_call_names_the_relay_or_a_host() {
        assert_eq!(parse_target("relay"), Ok(None));
        assert_eq!(parse_target(&id('a')), Ok(Some(id('a'))));
        assert!(parse_target("abc").is_err());
    }
}
