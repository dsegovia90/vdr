//! Through whom someone's broadcasts reach us, read off the hop chain of the
//! route announced to us, and where our own go.
//!
//! **A chain names origins, oldest first**, and each receiving side appends the
//! hop of the session it heard the route on (the sender appends its own on
//! wires before moq-lite-05; the chain comes out the same). So the last hop is
//! the far side of the session it reached us through. As this app builds them:
//!
//! ```text
//! a member through the relay            [member, gate, relay]
//! a member on a host we are on          [member, gate, host]
//! a member on a host bridged to relay   [member, gate, host, relay]
//! a member on the relay, via a host     [member, gate, relay, host]
//! a host's own, directly                [host]
//! a host's own, through the relay       [host, relay]
//! a member who is our guest, we host    [member, gate]
//! ```
//!
//! A member on a single session (no hosts known) has no gate: `[member, relay]`.
//!
//! **Only some hops can be named.** A host says its origin's hop in presence;
//! a guest of ours declares its session's hop when it connects. A relay's hop
//! is told to nobody, and nor is a member's gate, so the two look alike.
//! Hence the rule: the last hop is the person's own host hop (direct), one of
//! our guests' sessions (direct), another host's (via them), and **anything
//! else is the far side of our own session, which is the relay**. That
//! includes a host whose presence has not arrived yet, and a host on an older
//! build that states no hop: both read as the relay. In a room hosted with
//! `--host`, joined at `iroh://<host>/room`, that far side is the host
//! itself, which states no hop there, so it is named from the address.
//!
//! **The nearest intermediary is named, and the one before it only when it is
//! a known host**: `[.., host, relay]` reads "via relay, from that host". The
//! reverse, `[.., relay, host]`, reads only "via that host", because the relay
//! before it cannot be told from a gate. Telling them apart would need each
//! member to say its own hops (its publishing hop and its gates) in presence.
//!
//! **This is the route the announcement names as best, not a measurement of
//! where bytes flow.** The library serves a broadcast from the same ranking
//! the announcement follows, moving make-before-break, so the data follows
//! within a moment (read in moq-net, not measured); a route that refused a
//! request is the case where they could differ.

use std::collections::{BTreeMap, HashMap, HashSet};

/// Through whom someone's broadcasts reach us.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arrival {
    /// Straight from them, with nobody between.
    Direct,
    /// Through the relay (or whatever we joined), and, when it is known, the
    /// host the relay heard it from.
    Relay { from: Option<String> },
    /// Through a host, by endpoint id, and the host before it when known.
    Host { id: String, from: Option<String> },
}

/// Where our own broadcasts go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sending {
    /// To the room's relay.
    Relay,
    /// To a host, by endpoint id: the relay is not there, or the call was
    /// moved to that host (both reach `sessions`' pick the same way).
    Host(String),
    /// Only to our own guests: we host, and nothing else carries.
    Guests,
}

/// What we know when reading one chain.
pub(crate) struct Known<'a> {
    /// Their own origin's hop, when they host and say so.
    pub(crate) theirs: Option<u64>,
    /// Every host's origin hop, to its endpoint id.
    pub(crate) hosts: &'a BTreeMap<u64, String>,
    /// The hops our direct guests' sessions declared, while we host.
    pub(crate) guests: &'a HashSet<u64>,
    /// Our own origin's hop.
    pub(crate) ours: u64,
    /// The endpoint id on the far side of our session, when it is a person in
    /// the room rather than a relay: a room hosted with `--host` is joined at
    /// `iroh://<host>/room`, and that host states no hop.
    pub(crate) far: Option<&'a str>,
    /// Whose chain this is.
    pub(crate) who: &'a str,
}

/// Reads a route's hop chain, oldest first. `None` for a chain that names us,
/// which the library should never hand back, or names nobody.
pub(crate) fn read(hops: &[u64], known: &Known<'_>) -> Option<Arrival> {
    if hops.contains(&known.ours) && known.ours != 0 {
        return None;
    }
    let (&last, before) = hops.split_last()?;
    if known.theirs == Some(last) || known.guests.contains(&last) {
        return Some(Arrival::Direct);
    }
    // The hop before the nearest, named only when it is a host other than
    // them: anything else there is their own hop, a gate, or a relay.
    let from = before
        .last()
        .filter(|&&hop| known.theirs != Some(hop))
        .and_then(|hop| known.hosts.get(hop))
        .cloned();
    Some(match (known.hosts.get(&last), known.far) {
        (Some(id), _) => Arrival::Host { id: id.clone(), from },
        (None, Some(far)) if far == known.who => Arrival::Direct,
        (None, Some(far)) => Arrival::Host { id: far.to_string(), from },
        (None, None) => Arrival::Relay { from },
    })
}

/// The hops our direct guests' sessions declared, while each session lives.
///
/// **Display only.** A hop is the guest's own claim, unauthenticated; it
/// decides nothing but a word on a tile. Counted, because one guest can hold
/// two sessions for a moment across a redial.
#[derive(Default)]
pub(crate) struct Guests(std::sync::Mutex<HashMap<u64, usize>>);

impl Guests {
    pub(crate) fn add(&self, hop: u64) {
        if let Ok(mut hops) = self.0.lock() {
            *hops.entry(hop).or_default() += 1;
        }
    }

    pub(crate) fn remove(&self, hop: u64) {
        if let Ok(mut hops) = self.0.lock() {
            if let Some(count) = hops.get_mut(&hop) {
                *count -= 1;
                if *count == 0 {
                    hops.remove(&hop);
                }
            }
        }
    }

    pub(crate) fn hops(&self) -> HashSet<u64> {
        self.0.lock().map(|hops| hops.keys().copied().collect()).unwrap_or_default()
    }
}

/// What a person's tile says about them: the way they reach us, or where we
/// send, and whether they host. Words, with `name` turning an endpoint id
/// into what the page calls that person.
pub(crate) fn arrival_words(arrival: &Arrival, name: impl Fn(&str) -> String) -> String {
    match arrival {
        Arrival::Direct => "direct".to_string(),
        Arrival::Relay { from: None } => "via relay".to_string(),
        Arrival::Relay { from: Some(id) } => format!("via relay, from {}", name(id)),
        Arrival::Host { id, from: None } => format!("via {}", name(id)),
        Arrival::Host { id, from: Some(before) } => format!("via {}, from {}", name(id), name(before)),
    }
}

pub(crate) fn sending_words(sending: &Sending, name: impl Fn(&str) -> String) -> String {
    match sending {
        Sending::Relay => "to relay".to_string(),
        Sending::Host(id) => format!("to {}", name(id)),
        Sending::Guests => "to guests".to_string(),
    }
}

/// The same, for the log: full ids, and "host" said, so a scripted run can be
/// judged by grepping for an id.
pub(crate) fn arrival_log(arrival: &Arrival) -> String {
    match arrival {
        Arrival::Direct => "directly".to_string(),
        Arrival::Relay { from: None } => "via relay".to_string(),
        Arrival::Relay { from: Some(id) } => format!("via relay, from host {id}"),
        Arrival::Host { id, from: None } => format!("via host {id}"),
        Arrival::Host { id, from: Some(before) } => format!("via host {id}, from host {before}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMBER: u64 = 11;
    const GATE: u64 = 12;
    const RELAY: u64 = 20;
    const HOST: u64 = 30;
    const OTHER_HOST: u64 = 31;
    const US: u64 = 40;

    fn hosts() -> BTreeMap<u64, String> {
        BTreeMap::from([(HOST, "h".to_string()), (OTHER_HOST, "k".to_string())])
    }

    /// Reads `hops` as a member's (no hop of their own known), with `guests`.
    fn member(hops: &[u64], guests: &[u64]) -> Option<Arrival> {
        let hosts = hosts();
        let guests: HashSet<u64> = guests.iter().copied().collect();
        read(hops, &Known { theirs: None, hosts: &hosts, guests: &guests, ours: US, far: None, who: "m" })
    }

    /// Reads `hops` as the host `h`'s own broadcasts.
    fn of_host(hops: &[u64]) -> Option<Arrival> {
        let hosts = hosts();
        let guests = HashSet::new();
        read(hops, &Known { theirs: Some(HOST), hosts: &hosts, guests: &guests, ours: US, far: None, who: "h" })
    }

    fn host(id: &str) -> Arrival {
        Arrival::Host { id: id.to_string(), from: None }
    }

    const RELAYED: Arrival = Arrival::Relay { from: None };

    #[test]
    fn a_member_through_the_relay_reads_as_the_relay_gated_or_not() {
        assert_eq!(member(&[MEMBER, GATE, RELAY], &[]), Some(RELAYED));
        assert_eq!(member(&[MEMBER, RELAY], &[]), Some(RELAYED));
    }

    #[test]
    fn a_member_on_a_host_we_are_on_reads_as_that_host() {
        assert_eq!(member(&[MEMBER, GATE, HOST], &[]), Some(host("h")));
    }

    #[test]
    fn a_bridged_member_names_the_relay_and_the_host_behind_it() {
        assert_eq!(
            member(&[MEMBER, GATE, HOST, RELAY], &[]),
            Some(Arrival::Relay { from: Some("h".to_string()) })
        );
        assert_eq!(
            member(&[MEMBER, GATE, OTHER_HOST, HOST], &[]),
            Some(Arrival::Host { id: "h".to_string(), from: Some("k".to_string()) })
        );
    }

    #[test]
    fn a_relay_behind_a_host_cannot_be_told_from_a_gate() {
        assert_eq!(member(&[MEMBER, GATE, RELAY, HOST], &[]), Some(host("h")));
    }

    #[test]
    fn a_hosts_own_reads_direct_straight_from_it_and_relay_through_the_relay() {
        assert_eq!(of_host(&[HOST]), Some(Arrival::Direct));
        assert_eq!(of_host(&[HOST, RELAY]), Some(RELAYED), "their own hop is not a host before the relay");
    }

    #[test]
    fn when_we_host_a_guest_is_direct_and_anyone_else_on_the_relay_is_not() {
        assert_eq!(member(&[MEMBER, GATE], &[GATE]), Some(Arrival::Direct));
        assert_eq!(member(&[MEMBER, GATE, RELAY], &[GATE]), Some(RELAYED));
        assert_eq!(member(&[MEMBER, GATE, OTHER_HOST], &[GATE]), Some(host("k")));
    }

    #[test]
    fn an_unknown_last_hop_is_the_relay_and_a_chain_naming_us_or_nobody_is_nothing() {
        assert_eq!(member(&[MEMBER, 99], &[]), Some(RELAYED));
        assert_eq!(member(&[MEMBER, US, RELAY], &[]), None);
        assert_eq!(member(&[], &[]), None);
    }

    #[test]
    fn in_a_room_hosted_with_host_the_far_side_is_that_host() {
        let hosts = BTreeMap::new();
        let guests = HashSet::new();
        let at = |who| Known { theirs: None, hosts: &hosts, guests: &guests, ours: US, far: Some("x"), who };
        assert_eq!(read(&[99], &at("x")), Some(Arrival::Direct), "the host's own");
        assert_eq!(read(&[MEMBER, 99], &at("m")), Some(host("x")), "another guest's");
    }

    #[test]
    fn a_guest_stays_known_until_its_last_session_ends() {
        let guests = Guests::default();
        guests.add(GATE);
        guests.add(GATE);
        guests.remove(GATE);
        assert!(guests.hops().contains(&GATE));
        guests.remove(GATE);
        guests.remove(GATE);
        assert!(guests.hops().is_empty());
    }

    #[test]
    fn words_name_people_as_the_page_does_and_logs_say_ids() {
        let name = |id: &str| format!("<{id}>");
        assert_eq!(arrival_words(&Arrival::Direct, name), "direct");
        assert_eq!(arrival_words(&RELAYED, name), "via relay");
        assert_eq!(arrival_words(&host("h"), name), "via <h>");
        assert_eq!(arrival_words(&Arrival::Relay { from: Some("h".into()) }, name), "via relay, from <h>");
        assert_eq!(sending_words(&Sending::Relay, name), "to relay");
        assert_eq!(sending_words(&Sending::Host("h".into()), name), "to <h>");
        assert_eq!(sending_words(&Sending::Guests, name), "to guests");
        assert_eq!(arrival_log(&host("h")), "via host h");
        assert_eq!(arrival_log(&Arrival::Direct), "directly");
    }
}
