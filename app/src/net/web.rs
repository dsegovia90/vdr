//! The room as a web of nodes: who is in it, where each person's media goes,
//! and which sessions are held, as everyone says it in presence.
//!
//! **Two presence fields feed it, said by every build that has them**, with
//! no switch, because they cost a few dozen bytes and say nothing a session
//! does not already show to whoever is at its far end:
//!
//! - `via`: where our media goes now. `"relay"` is wherever the room meets
//!   (the relay server, or the creator of a room hosted with `--host`, whom
//!   a guest joined as `iroh://<creator>/room`); an endpoint id is a host
//!   carrying it; `"guests"` is a host that carries for its guests and sends
//!   nowhere further (the creator of a room without a relay server, or a host
//!   cut off from the relay). Those words cannot be mistaken for an id, which
//!   is 64 lowercase hex.
//! - `held`: the hosts we hold a session to, by endpoint id, **never their
//!   doors**. A host dials only the hosts whose id sorts below its own, so a
//!   session between two hosts is said by one of them; that is enough, since
//!   an edge has no direction here.
//!
//! **What people say is the source of truth, and how their media reaches us
//! (`arrival`) is not used.** An arrival names the last hop towards *us*, not
//! the sender's carrier: a member on the relay read through a host is
//! `[member, gate, relay, host]`, which reads "via that host", and the relay
//! before it cannot be told from a gate. Drawn as the sender's link, it would
//! add an edge that is not there. A peer who says nothing (an older build,
//! or one whose presence has not arrived) is drawn with an **assumed** link
//! to the meeting place: a build without these fields joins through a single
//! session to it, so that is where its media goes, but nobody said so.
//!
//! **Only what we can place is drawn.** An id that names nobody in the room as
//! we see it makes no edge and no node, so a sender cannot grow the picture
//! with made-up ids.
//!
//! Pure, apart from [`person`], which samples a [`PeerStats`] the way a tile
//! does, so the graph can be built on the interface's side from `local` and
//! every peer's entry without asking the call thread anything.

use std::collections::BTreeMap;

use super::arrival::{MeetsAt, Sending};
use super::metrics::PeerStats;
use super::quality::valid_id;

/// At most this many ids in a `held` list, ours or a peer's. A member dials
/// the room's kept hosts (`recent::HOSTS_KEPT`, eight) and at most one more
/// named by its invite, so a real list never comes near this, however many
/// of a hundred people host. Twice that leaves room for a later build to keep
/// more, and bounds what a hostile sender costs at about a kilobyte.
pub(crate) const HELD_MAX: usize = 16;

/// `via` on the wire for the place the room meets.
const VIA_RELAY: &str = "relay";
/// `via` on the wire for a host that sends only to its own guests.
const VIA_GUESTS: &str = "guests";

/// `via` as it goes out. Only called for `Some`: the field is skipped when
/// there is nothing to say.
pub(crate) fn say_via<S: serde::Serializer>(via: &Option<Sending>, serializer: S) -> Result<S::Ok, S::Error> {
    match via {
        Some(Sending::Relay) => serializer.serialize_str(VIA_RELAY),
        Some(Sending::Guests) => serializer.serialize_str(VIA_GUESTS),
        Some(Sending::Host(id)) => serializer.serialize_str(id),
        None => serializer.serialize_none(),
    }
}

/// `via` as it comes off the wire: anything unreadable is nothing said,
/// never a failed document, because one parse error ends listening to that
/// peer.
pub(crate) fn lenient_via<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Sending>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(text) => match text.as_str() {
            VIA_RELAY => Some(Sending::Relay),
            VIA_GUESTS => Some(Sending::Guests),
            _ => clean_via(Some(Sending::Host(text)), ""),
        },
        _ => None,
    })
}

/// `held` as it comes off the wire: anything but a list is an empty one, and
/// entries that are not ids are dropped one by one.
pub(crate) fn lenient_held<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    let serde_json::Value::Array(entries) = value else {
        return Ok(Vec::new());
    };
    let ids = entries.into_iter().filter_map(|entry| match entry {
        serde_json::Value::String(id) => Some(id),
        _ => None,
    });
    Ok(clean_held(ids, ""))
}

/// A `via` as we keep or send it: a host is a well-formed id other than the
/// sender's own, or nothing is said.
pub(crate) fn clean_via(via: Option<Sending>, sender: &str) -> Option<Sending> {
    match via {
        Some(Sending::Host(id)) if !valid_id(&id) || id == sender => None,
        via => via,
    }
}

/// A `held` list as we keep or send it: well-formed ids other than the
/// sender's own, sorted, once each, at most [`HELD_MAX`]. Sorted so that the
/// same sessions are always the same document, and a re-say is a real change.
pub(crate) fn clean_held(held: impl IntoIterator<Item = String>, sender: &str) -> Vec<String> {
    let mut held: Vec<String> = held.into_iter().filter(|id| valid_id(id) && id != sender).collect();
    held.sort();
    held.dedup();
    held.truncate(HELD_MAX);
    held
}

/// What a node in the web is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A relay server: a square. Never a person.
    Relay,
    /// Someone in the room: a circle.
    Participant,
}

/// One node of the web.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: Kind,
    /// A participant's endpoint id; for a relay, whatever id the caller gave
    /// it, which must not look like an endpoint id.
    pub id: String,
    /// A participant's cleaned name, or a relay's label; `None` to show the
    /// short id.
    pub name: Option<String>,
    /// Whether they could host the room: a host-coloured border.
    pub can_host: bool,
    /// Whether this is us.
    pub me: bool,
}

/// What an edge says about the link between two nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    /// A session is held: data could flow. Light grey.
    Held,
    /// Someone who says nothing, drawn to the meeting place, because that is
    /// where a build without these fields sends. A guess, so it is drawn as
    /// one, and loses to anything anyone says.
    Assumed,
    /// Someone's media is sent over it now. Whiter.
    Flowing,
}

/// One edge, undirected: `from` sorts before `to`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub state: State,
}

/// The whole picture: nodes sorted by id, edges by their two ends, so equal
/// inputs give equal webs and a redraw can be skipped when nothing changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Web {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// One person as the web needs them, from their entry or ours.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    pub id: String,
    pub name: Option<String>,
    pub can_host: bool,
    pub me: bool,
    /// Where their media goes, as they say (ours, as `sessions` set it).
    /// `None` on a peer is someone who says nothing.
    pub via: Option<Sending>,
    /// The hosts they hold a session to, cleaned.
    pub held: Vec<String>,
}

/// A relay server, as a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relay {
    pub id: String,
    pub name: Option<String>,
}

/// The id a relay server is given when the room was joined by URL: never an
/// endpoint id, and never the relay's address, which can be private.
pub const RELAY_ID: &str = "relay";

/// Where "relay" points in a room, and the relay node to draw for it when it
/// is a server rather than a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meeting {
    pub venue: String,
    pub relay: Option<Relay>,
}

/// Where the room meets, from our own `meets_at` and who is in it. For a room
/// only; a direct call meets nowhere and passes no venue to [`build`].
///
/// A room hosted with `--host` meets at its creator: that is us
/// ([`MeetsAt::Us`]), or the endpoint we joined when it turns out to be
/// someone in the room. An endpoint that is nobody here is an iroh-enabled
/// relay server; until its creator's presence arrives, a room without a
/// relay server looks the same, and is drawn that way for that moment.
pub fn meeting(meets_at: Option<&MeetsAt>, me: &str, people: &[Person]) -> Meeting {
    match meets_at {
        Some(MeetsAt::Us) => Meeting { venue: me.to_string(), relay: None },
        Some(MeetsAt::Endpoint(id)) if people.iter().any(|person| &person.id == id) => {
            Meeting { venue: id.clone(), relay: None }
        }
        Some(MeetsAt::Endpoint(id)) => Meeting {
            venue: id.clone(),
            relay: Some(Relay { id: id.clone(), name: None }),
        },
        None => Meeting {
            venue: RELAY_ID.to_string(),
            relay: Some(Relay { id: RELAY_ID.to_string(), name: None }),
        },
    }
}

/// One person, sampled from their entry: cheap locks and atomics only, so
/// the interface can poll it.
pub fn person(id: &str, me: bool, stats: &PeerStats) -> Person {
    Person {
        id: id.to_string(),
        name: stats.name(),
        // As the tiles judge it: they say they host, or, on ours, we serve.
        can_host: stats.hosting() || stats.hosts_room(),
        me,
        via: stats.sending(),
        held: stats.held(),
    }
}

/// The web of a room: `people` (us included), the relay servers to draw, and
/// `venue`, the node a `via` of "relay" lands on (a relay's id or a person's;
/// `None` in a direct call).
pub fn build<'p>(people: &'p [Person], relays: &'p [Relay], venue: Option<&'p str>) -> Web {
    let mut nodes: BTreeMap<&'p str, Node> = BTreeMap::new();
    for relay in relays {
        nodes.entry(&relay.id).or_insert_with(|| Node {
            kind: Kind::Relay,
            id: relay.id.clone(),
            name: relay.name.clone(),
            can_host: false,
            me: false,
        });
    }
    // A person wins over a relay of the same id: someone in the room is who
    // the endpoint turned out to be.
    for person in people {
        nodes.insert(
            &person.id,
            Node {
                kind: Kind::Participant,
                id: person.id.clone(),
                name: person.name.clone(),
                can_host: person.can_host,
                me: person.me,
            },
        );
    }
    let venue = venue.filter(|venue| nodes.contains_key(venue));

    let mut edges: BTreeMap<(&str, &str), State> = BTreeMap::new();
    let mut add = |a: &'p str, b: &'p str, state: State| {
        if a == b || !nodes.contains_key(a) || !nodes.contains_key(b) {
            return;
        }
        let key = if a < b { (a, b) } else { (b, a) };
        let slot = edges.entry(key).or_insert(state);
        *slot = (*slot).max(state);
    };
    for person in people {
        for host in &person.held {
            add(&person.id, host, State::Held);
        }
        match &person.via {
            Some(Sending::Relay) => {
                if let Some(venue) = venue {
                    add(&person.id, venue, State::Flowing);
                }
            }
            Some(Sending::Host(id)) => add(&person.id, id, State::Flowing),
            Some(Sending::Guests) => {}
            None if !person.me && person.held.is_empty() => {
                if let Some(venue) = venue {
                    add(&person.id, venue, State::Assumed);
                }
            }
            None => {}
        }
    }

    Web {
        nodes: nodes.into_values().collect(),
        edges: edges
            .into_iter()
            .map(|((from, to), state)| Edge { from: from.to_string(), to: to.to_string(), state })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    fn numbered(n: usize) -> String {
        format!("{n:064x}")
    }

    fn says(who: &str, via: Option<Sending>, held: &[&str]) -> Person {
        Person {
            id: who.to_string(),
            via,
            held: held.iter().map(|id| id.to_string()).collect(),
            ..Person::default()
        }
    }

    fn host(who: &str) -> Option<Sending> {
        Some(Sending::Host(who.to_string()))
    }

    fn server() -> Vec<Relay> {
        vec![Relay { id: RELAY_ID.to_string(), name: None }]
    }

    fn edge(a: &str, b: &str, state: State) -> Edge {
        let (from, to) = if a < b { (a, b) } else { (b, a) };
        Edge { from: from.to_string(), to: to.to_string(), state }
    }

    /// What the presence fields read as, from a JSON value.
    #[derive(serde::Deserialize)]
    struct Fields {
        #[serde(default, deserialize_with = "lenient_via")]
        via: Option<Sending>,
        #[serde(default, deserialize_with = "lenient_held")]
        held: Vec<String>,
    }

    fn read(json: &str) -> Fields {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn via_reads_the_two_words_and_an_id_and_nothing_else() {
        assert_eq!(read(r#"{"via":"relay"}"#).via, Some(Sending::Relay));
        assert_eq!(read(r#"{"via":"guests"}"#).via, Some(Sending::Guests));
        assert_eq!(read(&format!(r#"{{"via":"{}"}}"#, id('a'))).via, host(&id('a')));
        for garbage in [r#""Relay""#, r#""abc""#, "5", "null", "[1]", r#"{"host":"x"}"#, &format!(r#""{}""#, "F".repeat(64))] {
            assert_eq!(read(&format!(r#"{{"via":{garbage}}}"#)).via, None, "{garbage}");
        }
    }

    #[test]
    fn held_keeps_ids_only_sorted_once_each_and_capped() {
        let json = format!(r#"{{"held":["{b}",7,"nope","{a}","{b}",null,"{upper}"]}}"#, a = id('a'), b = id('b'), upper = "A".repeat(64));
        assert_eq!(read(&json).held, vec![id('a'), id('b')]);
        for garbage in ["5", r#""x""#, "{}", "null", "true"] {
            assert!(read(&format!(r#"{{"held":{garbage}}}"#)).held.is_empty(), "{garbage}");
        }
        let many: Vec<String> = (0..100).map(numbered).collect();
        let json = serde_json::json!({ "held": many }).to_string();
        assert_eq!(read(&json).held.len(), HELD_MAX);
    }

    #[test]
    fn a_sender_naming_itself_is_cleaned_away() {
        assert_eq!(clean_via(host(&id('a')), &id('a')), None);
        assert_eq!(clean_via(Some(Sending::Relay), &id('a')), Some(Sending::Relay));
        assert_eq!(clean_held([id('a'), id('b')], &id('a')), vec![id('b')]);
    }

    #[test]
    fn via_says_what_it_reads() {
        for via in [Sending::Relay, Sending::Guests, Sending::Host(id('c'))] {
            let mut json = serde_json::Serializer::new(Vec::new());
            say_via(&Some(via.clone()), &mut json).unwrap();
            let json = format!(r#"{{"via":{}}}"#, String::from_utf8(json.into_inner()).unwrap());
            assert_eq!(read(&json).via, Some(via));
        }
    }

    #[test]
    fn a_plain_relay_room_is_a_star_all_flowing() {
        let people: Vec<Person> =
            ['a', 'b', 'c'].map(|c| Person { me: c == 'a', ..says(&id(c), Some(Sending::Relay), &[]) }).into();
        let web = build(&people, &server(), Some(RELAY_ID));
        assert_eq!(web.nodes.len(), 4);
        assert_eq!(web.nodes.iter().filter(|node| node.kind == Kind::Relay).count(), 1);
        assert!(web.nodes.iter().find(|node| node.id == id('a')).unwrap().me);
        assert_eq!(
            web.edges,
            ['a', 'b', 'c'].map(|c| edge(&id(c), RELAY_ID, State::Flowing)).to_vec()
        );
    }

    #[test]
    fn a_host_carrying_takes_its_members_and_sends_on_to_the_relay() {
        let h = id('d');
        let people = vec![
            Person { can_host: true, ..says(&h, Some(Sending::Relay), &[]) },
            says(&id('a'), host(&h), &[&h]),
            says(&id('b'), host(&h), &[&h]),
        ];
        let web = build(&people, &server(), Some(RELAY_ID));
        assert_eq!(
            web.edges,
            vec![
                edge(&id('a'), &h, State::Flowing),
                edge(&id('b'), &h, State::Flowing),
                edge(&h, RELAY_ID, State::Flowing),
            ]
        );
        assert!(web.nodes.iter().find(|node| node.id == h).unwrap().can_host);
    }

    #[test]
    fn mixed_relay_and_host_with_spares_held() {
        let (h, k) = (id('8'), id('9'));
        let people = vec![
            Person { can_host: true, ..says(&h, Some(Sending::Relay), &[]) },
            // k dials h, the host below it.
            Person { can_host: true, ..says(&k, Some(Sending::Relay), &[&h]) },
            says(&id('a'), Some(Sending::Relay), &[&h, &k]),
            says(&id('b'), host(&h), &[&h, &k]),
        ];
        let web = build(&people, &server(), Some(RELAY_ID));
        let mut expected = vec![
            edge(&h, &k, State::Held),
            edge(&h, RELAY_ID, State::Flowing),
            edge(&k, RELAY_ID, State::Flowing),
            edge(&id('a'), &h, State::Held),
            edge(&id('a'), &k, State::Held),
            edge(&id('a'), RELAY_ID, State::Flowing),
            edge(&id('b'), &h, State::Flowing),
            edge(&id('b'), &k, State::Held),
        ];
        expected.sort_by(|x, y| (&x.from, &x.to).cmp(&(&y.from, &y.to)));
        assert_eq!(web.edges, expected);
    }

    #[test]
    fn a_room_without_a_relay_meets_at_its_creator() {
        let c = id('c');
        let people = vec![
            Person { me: true, ..says(&c, Some(Sending::Guests), &[]) },
            // A guest joined at `iroh://<c>/room`: its far side is "the relay".
            says(&id('a'), Some(Sending::Relay), &[]),
            says(&id('b'), Some(Sending::Relay), &[]),
        ];
        let at = meeting(Some(&MeetsAt::Us), &c, &people);
        assert_eq!(at, Meeting { venue: c.clone(), relay: None });
        let web = build(&people, &[], Some(&at.venue));
        assert!(web.nodes.iter().all(|node| node.kind == Kind::Participant));
        assert_eq!(web.edges, vec![edge(&id('a'), &c, State::Flowing), edge(&id('b'), &c, State::Flowing)]);
        // The same room from a guest's seat.
        assert_eq!(meeting(Some(&MeetsAt::Endpoint(c.clone())), &id('a'), &people).relay, None);
        // An iroh relay server is nobody in the room, and is drawn as a relay.
        let r = meeting(Some(&MeetsAt::Endpoint(id('e'))), &id('a'), &people);
        assert_eq!(r.relay.map(|relay| relay.id), Some(id('e')));
    }

    #[test]
    fn someone_who_says_nothing_is_assumed_on_the_relay() {
        let people = vec![
            Person { me: true, ..says(&id('a'), Some(Sending::Relay), &[]) },
            says(&id('b'), None, &[]),
        ];
        let web = build(&people, &server(), Some(RELAY_ID));
        assert_eq!(web.nodes.len(), 3, "still a node");
        assert_eq!(web.edges[1], edge(&id('b'), RELAY_ID, State::Assumed));
        // Saying anything at all ends the guess, and so does being us.
        let people = vec![Person { me: true, ..says(&id('a'), None, &[]) }, says(&id('b'), None, &[&id('a')])];
        assert_eq!(build(&people, &server(), Some(RELAY_ID)).edges, vec![edge(&id('a'), &id('b'), State::Held)]);
    }

    #[test]
    fn ids_that_name_nobody_draw_nothing() {
        let people = vec![says(&id('a'), host(&id('f')), &[&id('e')])];
        let web = build(&people, &server(), Some(RELAY_ID));
        assert_eq!(web.nodes.len(), 2);
        assert!(web.edges.is_empty());
        // A venue that is not drawn takes no edges either.
        assert!(build(&[says(&id('a'), Some(Sending::Relay), &[])], &[], Some(RELAY_ID)).edges.is_empty());
    }

    #[test]
    fn a_link_said_both_ways_is_one_edge_at_its_strongest() {
        let (a, h) = (id('a'), id('d'));
        let people = vec![says(&a, host(&h), &[&h]), says(&h, None, &[&a])];
        assert_eq!(build(&people, &[], None).edges, vec![edge(&a, &h, State::Flowing)]);
    }

    #[test]
    fn a_hundred_people_and_fifty_relays_build_quickly_and_the_same_every_time() {
        let relays: Vec<Relay> = (0..50).map(|n| Relay { id: format!("relay-{n:02}"), name: None }).collect();
        let hosts: Vec<String> = (0..8).map(numbered).collect();
        let people: Vec<Person> = (0..100)
            .map(|n| {
                let via = match n % 3 {
                    0 => Some(Sending::Relay),
                    1 => host(&hosts[n % hosts.len()]),
                    _ => None,
                };
                let held: Vec<&str> = hosts.iter().map(String::as_str).collect();
                Person { can_host: n < 8, me: n == 42, ..says(&numbered(n), via, &held) }
            })
            .collect();
        let started = std::time::Instant::now();
        let web = build(&people, &relays, Some("relay-00"));
        let reversed: Vec<Person> = people.iter().rev().cloned().collect();
        let again = build(&reversed, &relays.iter().rev().cloned().collect::<Vec<_>>(), Some("relay-00"));
        assert!(started.elapsed() < std::time::Duration::from_millis(200), "{:?}", started.elapsed());
        assert_eq!(web, again, "the order things arrive in changes nothing");
        assert_eq!(web.nodes.len(), 150);
        assert!(web.nodes.windows(2).all(|pair| pair[0].id < pair[1].id));
        assert!(web.edges.windows(2).all(|pair| (&pair[0].from, &pair[0].to) < (&pair[1].from, &pair[1].to)));
        assert!(web.edges.iter().all(|edge| edge.from < edge.to));
    }
}
