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
//! before it cannot be told from a gate. Telling them apart needs each
//! member to say its own hops (its publishing hop and its gates), which
//! `routes.json` does for the room map: [`path_of`] reads the whole chain
//! and leaves what it cannot name as numbers, for a viewer to name from
//! everyone's claims (`web`).
//!
//! **This is the route the announcement names as best, not a measurement of
//! where bytes flow.** The library serves a broadcast from the same ranking
//! the announcement follows, moving make-before-break, so the data follows
//! within a moment (read in moq-net, not measured); a route that refused a
//! request is the case where they could differ.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::web::Hop;

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

/// Where the room meets, when that is a person's own endpoint rather than a
/// relay server: a room hosted with `--host` has no relay, and its creator's
/// session is what everyone joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeetsAt {
    /// We host the room with `--host`: every guest is on our session.
    Us,
    /// We joined at `iroh://<id>/room`. A person when `id` turns up in the
    /// room; an iroh-enabled relay server otherwise, which never does.
    Endpoint(String),
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
    /// The hops we know to be the meeting place: the last hop of every chain
    /// [`read`] takes for the relay, since that is the far side of our own
    /// session to it. Only [`path_of`] uses them, to name the relay where it
    /// sits further back in a chain.
    pub(crate) venue: &'a HashSet<u64>,
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

/// Whether a chain's last hop is the far side of our session to the meeting
/// place, as [`read`] judges it: what fills [`Known::venue`].
pub(crate) fn ends_at_venue(hops: &[u64], known: &Known<'_>) -> bool {
    matches!(read(hops, known), Some(Arrival::Relay { .. }))
}

/// Every node between them and us, nearest them first: the whole chain,
/// where [`read`] names only its last two. `None` where `read` is.
///
/// The first hop is their origin and is left out, and so is their own
/// host hop. A hop further back is named when it is a host's or the
/// meeting place's, and is otherwise left as a number for a viewer to name
/// from the claims everyone makes ([`super::web::build_reported`]): a gate
/// cannot be told from a relay here. The last hop is named as `read` names
/// it. **Unread for a reader of the second hop**: when it names nobody
/// and no other chain passes through it, it is taken for their gate and
/// left out by the caller ([`own_gates`]).
pub(crate) fn path_of(hops: &[u64], known: &Known<'_>) -> Option<Vec<Hop>> {
    if hops.contains(&known.ours) && known.ours != 0 {
        return None;
    }
    let (&last, before) = hops.split_last()?;
    let mut path: Vec<Hop> = Vec::new();
    if before.is_empty() {
        // Their origin is the far side of the session: straight from them.
        // `read` says relay for an unknown lone hop; no chain this app
        // builds is one, so either would do (inferred from the table above).
        return Some(path);
    }
    let name = |hop: u64| -> Option<Hop> {
        if known.theirs == Some(hop) {
            return None;
        }
        match known.hosts.get(&hop) {
            Some(id) if id == known.who => None,
            Some(id) => Some(Hop::Person(id.clone())),
            None if known.venue.contains(&hop) => Some(Hop::Venue),
            None => Some(Hop::Unnamed(hop)),
        }
    };
    path.extend(before[1..].iter().filter_map(|&hop| name(hop)));
    if known.theirs != Some(last) && !known.guests.contains(&last) {
        match (known.hosts.get(&last), known.far) {
            (Some(id), _) if id == known.who => {}
            (Some(id), _) => path.push(Hop::Person(id.clone())),
            (None, Some(far)) if far == known.who => {}
            (None, Some(far)) => path.push(Hop::Person(far.to_string())),
            (None, None) => path.push(Hop::Venue),
        }
    }
    path.dedup();
    Some(path)
}

/// The hops at the second place of `chains` (each person's chain, by id)
/// that sit nowhere else in any chain: each is the one person's gate, and
/// so theirs rather than a hop between. A relay or a host is in the chains
/// of everyone it carries; a gate only in its owner's. **Inferred**, from
/// how `sessions` makes one gate per session: a relay carrying one person
/// only looks the same, and is lost from their path for a viewer who does
/// not hear their own report.
pub(crate) fn own_gates<'c>(chains: impl IntoIterator<Item = &'c [u64]> + Clone) -> HashSet<u64> {
    let mut seen: HashMap<u64, usize> = HashMap::new();
    for chain in chains.clone() {
        let mut hops: Vec<u64> = chain.iter().skip(1).copied().collect();
        hops.sort_unstable();
        hops.dedup();
        for hop in hops {
            *seen.entry(hop).or_default() += 1;
        }
    }
    chains
        .into_iter()
        .filter(|chain| chain.len() >= 3)
        .map(|chain| chain[1])
        .filter(|hop| seen.get(hop) == Some(&1))
        .collect()
}

/// The hops our direct guests' sessions declared, while each session lives.
///
/// **Display only.** A hop is the guest's own claim, unauthenticated; it
/// decides nothing but a log line and a tile's border. Counted, because one guest can hold
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

/// The hosts someone's broadcasts pass through on their way to us, nearest
/// first: whoever the route names, the one before the nearest included. A
/// host the route names is carrying their media for us, so a tile can say
/// so; the relay is not a person and has no tile.
pub(crate) fn through(arrival: &Arrival) -> Vec<String> {
    match arrival {
        Arrival::Direct | Arrival::Relay { from: None } => Vec::new(),
        Arrival::Relay { from: Some(id) } => vec![id.clone()],
        Arrival::Host { id, from } => std::iter::once(id.clone()).chain(from.clone()).collect(),
    }
}

/// For the log: full ids, and "host" said, so a scripted run can be
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
        read(hops, &Known { theirs: None, hosts: &hosts, guests: &guests, ours: US, far: None, who: "m", venue: &HashSet::new() })
    }

    /// Reads `hops` as the host `h`'s own broadcasts.
    fn of_host(hops: &[u64]) -> Option<Arrival> {
        let hosts = hosts();
        let guests = HashSet::new();
        read(hops, &Known { theirs: Some(HOST), hosts: &hosts, guests: &guests, ours: US, far: None, who: "h", venue: &HashSet::new() })
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
        let venue = HashSet::new();
        let at = |who| Known { theirs: None, hosts: &hosts, guests: &guests, ours: US, far: Some("x"), who, venue: &venue };
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
    fn every_host_a_route_names_is_passed_through_and_the_relay_is_nobody() {
        assert!(through(&Arrival::Direct).is_empty());
        assert!(through(&RELAYED).is_empty());
        assert_eq!(through(&Arrival::Relay { from: Some("h".into()) }), ["h"]);
        assert_eq!(through(&host("h")), ["h"]);
        assert_eq!(through(&Arrival::Host { id: "h".into(), from: Some("k".into()) }), ["h", "k"]);
    }

    #[test]
    fn logs_say_ids() {
        assert_eq!(arrival_log(&host("h")), "via host h");
        assert_eq!(arrival_log(&Arrival::Direct), "directly");
        assert_eq!(arrival_log(&Arrival::Relay { from: Some("h".into()) }), "via relay, from host h");
    }

    /// The whole path of a member's chain, knowing `venue` as the meeting
    /// place's hops.
    fn member_path(hops: &[u64], guests: &[u64], venue: &[u64]) -> Option<Vec<Hop>> {
        let hosts = hosts();
        let guests: HashSet<u64> = guests.iter().copied().collect();
        let venue: HashSet<u64> = venue.iter().copied().collect();
        path_of(hops, &Known { theirs: None, hosts: &hosts, guests: &guests, ours: US, far: None, who: "m", venue: &venue })
    }

    fn person(id: &str) -> Hop {
        Hop::Person(id.to_string())
    }

    #[test]
    fn a_path_names_every_hop_of_every_chain_shape() {
        use Hop::{Unnamed, Venue};
        // Through the relay, gated or not.
        assert_eq!(member_path(&[MEMBER, GATE, RELAY], &[], &[]), Some(vec![Unnamed(GATE), Venue]));
        assert_eq!(member_path(&[MEMBER, RELAY], &[], &[]), Some(vec![Venue]));
        // On a host we are on, and bridged on to the relay.
        assert_eq!(member_path(&[MEMBER, GATE, HOST], &[], &[]), Some(vec![Unnamed(GATE), person("h")]));
        assert_eq!(member_path(&[MEMBER, GATE, HOST, RELAY], &[], &[]), Some(vec![Unnamed(GATE), person("h"), Venue]));
        // On the relay, via a host: the relay is named only when we know its
        // hop, as the far side of our own session to it.
        assert_eq!(member_path(&[MEMBER, GATE, RELAY, HOST], &[], &[]), Some(vec![Unnamed(GATE), Unnamed(RELAY), person("h")]));
        assert_eq!(member_path(&[MEMBER, GATE, RELAY, HOST], &[], &[RELAY]), Some(vec![Unnamed(GATE), Venue, person("h")]));
        // Two hosts.
        assert_eq!(member_path(&[MEMBER, GATE, OTHER_HOST, HOST], &[], &[]), Some(vec![Unnamed(GATE), person("k"), person("h")]));
        // Our own guest: straight to us.
        assert_eq!(member_path(&[MEMBER, GATE], &[GATE], &[]), Some(vec![]));
        assert_eq!(member_path(&[MEMBER, GATE, RELAY], &[GATE], &[]), Some(vec![Unnamed(GATE), Venue]));
        // A chain naming us, or nobody, is nothing.
        assert_eq!(member_path(&[MEMBER, US, RELAY], &[], &[]), None);
        assert_eq!(member_path(&[], &[], &[]), None);
    }

    #[test]
    fn a_hosts_own_path_leaves_out_its_own_hop() {
        let hosts = hosts();
        let none = HashSet::new();
        let theirs = Known { theirs: Some(HOST), hosts: &hosts, guests: &none, ours: US, far: None, who: "h", venue: &none };
        assert_eq!(path_of(&[HOST], &theirs), Some(vec![]));
        assert_eq!(path_of(&[HOST, RELAY], &theirs), Some(vec![Hop::Venue]));
        assert_eq!(path_of(&[HOST, OTHER_HOST], &theirs), Some(vec![person("k")]));
    }

    #[test]
    fn in_a_room_hosted_with_host_a_path_runs_through_that_host() {
        let hosts = BTreeMap::new();
        let none = HashSet::new();
        let at = |who| Known { theirs: None, hosts: &hosts, guests: &none, ours: US, far: Some("x"), who, venue: &none };
        assert_eq!(path_of(&[99], &at("x")), Some(vec![]));
        assert_eq!(path_of(&[MEMBER, 99], &at("m")), Some(vec![person("x")]));
    }

    #[test]
    fn a_hop_second_in_one_chain_only_is_its_owners_gate() {
        let chains: Vec<Vec<u64>> = vec![
            vec![MEMBER, GATE, RELAY],
            vec![MEMBER + 1, GATE + 1, RELAY],
            vec![HOST, RELAY],
            // A relay carrying many is no gate.
            vec![MEMBER + 2, RELAY + 1, HOST],
            vec![MEMBER + 3, RELAY + 1, HOST],
        ];
        let gates = own_gates(chains.iter().map(Vec::as_slice));
        assert_eq!(gates, HashSet::from([GATE, GATE + 1]));
        // The ends-at-the-venue test is `read`'s.
        let hosts = hosts();
        let none = HashSet::new();
        let reading = Known { theirs: None, hosts: &hosts, guests: &none, ours: US, far: None, who: "m", venue: &none };
        assert!(ends_at_venue(&[MEMBER, GATE, RELAY], &reading));
        assert!(!ends_at_venue(&[MEMBER, GATE, HOST], &reading));
    }
}
