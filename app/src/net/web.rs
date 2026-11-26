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
//!   doors**, and `"relay"` while our session to where the room meets is up,
//!   meaning the same place as in `via`. Without the word, someone whose
//!   media goes through a host showed no line to the relay they also hold. A
//!   host dials only the hosts whose id sorts below its own, so a session
//!   between two hosts is said by one of them; that is enough, since an edge
//!   has no direction here.
//!
//! **Which way media travels comes from the receivers, in `routes.json`**, a
//! track of its own beside presence, asked for only while someone has the
//! map open (see `call.rs`): each receiver says the path every other
//! person's media takes to reach it, as its hop chains show, and which hops
//! are its own. A viewer names every hop from all of those claims together
//! ([`build_reported`]), so a gate is told from a relay, which no one
//! receiver can do alone. The document:
//!
//! ```json
//! {"v":1,"o":4120938475123,"g":[77213,9001],"r":[5521],"n":3,
//!  "d":["relay"],
//!  "x":{"c9d8e7f6":[],"5a5a5a5a":["c9d8e7f6",88812]}}
//! ```
//!
//! `o` the author's origin hop, `g` its gates, `r` hops it knows to be the
//! meeting place, `n` how many it hears, `d` the path of everyone not in `x`,
//! and `x` the exceptions by endpoint-id prefix (`null`: not received). In a
//! path a string is a person's prefix or `"relay"`, a number a hop nobody
//! named, and `[]` is direct.
//!
//! **Presence is the fallback, as it was the whole picture before.** A peer
//! on an older build publishes no report and never asks for one, and is
//! drawn from `via` and `held` alone.
//!
//! **How their media reaches us (`arrival`) is not used.** An arrival names the last hop towards *us*, not
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
//! It is the reports, which read every hop, that the map's directions come
//! from.
//!
//! Pure, apart from [`person`], which samples a [`PeerStats`] the way a tile
//! does, so the graph can be built on the interface's side from `local` and
//! every peer's entry without asking the call thread anything.

use std::collections::{BTreeMap, BTreeSet};

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
/// The same place in a `held` list: our session to it is up.
pub(crate) const HELD_RELAY: &str = VIA_RELAY;
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

/// A `held` list as we keep or send it: [`HELD_RELAY`] and well-formed ids
/// other than the sender's own, sorted, once each, at most [`HELD_MAX`].
/// Sorted so that the same sessions are always the same document, and a
/// re-say is a real change; the word first, so the cap never drops it.
pub(crate) fn clean_held(held: impl IntoIterator<Item = String>, sender: &str) -> Vec<String> {
    let mut held: Vec<String> =
        held.into_iter().filter(|id| *id == HELD_RELAY || (valid_id(id) && id != sender)).collect();
    held.sort_by(|a, b| (a != HELD_RELAY, a).cmp(&(b != HELD_RELAY, b)));
    held.dedup();
    held.truncate(HELD_MAX);
    held
}

/// One step of the path someone's media takes to a receiver, as that
/// receiver says it in `routes.json`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Hop {
    /// Someone in the room: a full endpoint id on the writer's side, the
    /// prefix as said on the reader's.
    Person(String),
    /// Wherever the room meets, the same place as `via`'s "relay".
    Venue,
    /// A hop of the route the receiver could not name: a relay's, or a
    /// gate's, which its owner may claim in a document of its own.
    Unnamed(u64),
}

/// The most a `routes.json` document may be, checked before it is parsed.
/// A hundred people each with an exception of a few hops is about 5 KB.
pub(crate) const ROUTES_BYTES: usize = 16 * 1024;
/// At most this many exceptions in a document, written or read: more than a
/// room of a hundred, so only a hostile sender comes near it.
pub(crate) const EXCEPT_MAX: usize = 128;
/// At most this many gate hops, and as many meeting-place hops: one per
/// session, and a member dials at most nine (see [`HELD_MAX`]).
pub(crate) const GATES_MAX: usize = 8;
/// At most this many steps in a path. The library caps a route's chain at 32
/// hops; a real one in this app is four at most.
pub(crate) const PATH_MAX: usize = 8;
/// At most this many relays no one has named are drawn, beside the ones we
/// know of.
pub const UNNAMED_MAX: usize = 50;
/// How long an id prefix in `routes.json` is: the first of these that no one
/// else in the writer's roster shares. Eight hex digits collide in a room of
/// a hundred about once in four hundred thousand rooms.
const PREFIXES: [usize; 3] = [8, 12, 16];
/// The document's shape. Another value reads as nothing said, so a later
/// build that changes a meaning changes this.
const ROUTES_VERSION: u64 = 1;
/// The meeting place in a path, as `via` says it.
const ROUTES_VENUE: &str = VIA_RELAY;

/// What we say in `routes.json`, with full ids: how every other person's
/// media reaches us, and which hops are ours, so a viewer can tell our own
/// hops from a relay's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Routes {
    /// Our origin's hop: first in every route of ours.
    pub(crate) origin: u64,
    /// The hops of our gates, one per session of a join over several.
    pub(crate) gates: Vec<u64>,
    /// Hops we know to be the meeting place: the far side of our session to
    /// it. Not in the document the plan first drew; without it, someone who
    /// hears the relay only through a host could not name it, and a viewer
    /// would draw the relay twice.
    pub(crate) venue: Vec<u64>,
    /// Each peer we hear: the path between, nearest them first (`[]` is
    /// direct), or `None` when nothing of theirs reaches us.
    pub(crate) paths: BTreeMap<String, Option<Vec<Hop>>>,
}

/// What a receiver says in `routes.json`, as read and cleaned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reported {
    /// The path of everyone not in `except`; `None` when not said.
    pub default: Option<Vec<Hop>>,
    /// Paths by endpoint-id prefix; `None` is "not received".
    pub except: BTreeMap<String, Option<Vec<Hop>>>,
    /// The author's origin hop.
    pub origin: Option<u64>,
    /// The author's gate hops.
    pub gates: Vec<u64>,
    /// Hops the author knows to be the meeting place.
    pub venue: Vec<u64>,
    /// How many people the author hears.
    pub heard: Option<u32>,
}

/// The shortest prefix of `id` in [`PREFIXES`] that nobody else in `roster`
/// shares, or the longest when even that is shared: a reader then cannot
/// match it to one person and drops it.
fn prefix<'a>(id: &'a str, roster: &[&str]) -> &'a str {
    for len in PREFIXES {
        let Some(mine) = id.get(..len) else { return id };
        if !roster.iter().any(|other| *other != id && other.starts_with(mine)) {
            return mine;
        }
    }
    id.get(..PREFIXES[PREFIXES.len() - 1]).unwrap_or(id)
}

/// `routes` as it goes out: every peer whose path is the commonest one
/// left to the default, the rest as exceptions, ids shortened against
/// `roster` (everyone we know of, us included). Kept under
/// [`ROUTES_BYTES`] by leaving exceptions out, last first, which a reader
/// then takes for the default: wrong for them, never a document refused.
pub(crate) fn say_routes(routes: &Routes, roster: &[&str]) -> Vec<u8> {
    use serde_json::{Map, Value};
    let path = |hops: &[Hop]| -> Value {
        Value::Array(
            hops.iter()
                .take(PATH_MAX)
                .map(|hop| match hop {
                    Hop::Person(id) => Value::from(prefix(id, roster)),
                    Hop::Venue => Value::from(ROUTES_VENUE),
                    Hop::Unnamed(hop) => Value::from(*hop),
                })
                .collect(),
        )
    };
    let mut counts: BTreeMap<&Vec<Hop>, usize> = BTreeMap::new();
    for said in routes.paths.values().flatten() {
        *counts.entry(said).or_default() += 1;
    }
    // The commonest, and of equals the one that sorts first, so the same
    // routes are always the same document.
    let default = counts.iter().max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0))).map(|(said, _)| *said);
    let mut except: Vec<(&str, Value)> = routes
        .paths
        .iter()
        .filter(|(_, said)| said.as_ref() != default)
        .take(EXCEPT_MAX)
        .map(|(id, said)| (prefix(id, roster), said.as_deref().map_or(Value::Null, path)))
        .collect();
    let hops = |hops: &[u64]| Value::Array(hops.iter().take(GATES_MAX).map(|&hop| Value::from(hop)).collect());
    loop {
        let mut doc = Map::new();
        doc.insert("v".into(), Value::from(ROUTES_VERSION));
        doc.insert("o".into(), Value::from(routes.origin));
        if !routes.gates.is_empty() {
            doc.insert("g".into(), hops(&routes.gates));
        }
        if !routes.venue.is_empty() {
            doc.insert("r".into(), hops(&routes.venue));
        }
        doc.insert("n".into(), Value::from(routes.paths.values().flatten().count()));
        if let Some(default) = default {
            doc.insert("d".into(), path(default));
        }
        if !except.is_empty() {
            doc.insert("x".into(), Value::Object(except.iter().map(|(id, said)| (id.to_string(), said.clone())).collect()));
        }
        let bytes = serde_json::to_vec(&Value::Object(doc)).unwrap_or_default();
        if bytes.len() <= ROUTES_BYTES || except.is_empty() {
            return bytes;
        }
        let drop = except.len() / 8 + 1;
        except.truncate(except.len().saturating_sub(drop));
    }
}

/// A `routes.json` document as it comes off the wire, from `sender`:
/// `None` when it is too large to look at or is not JSON at all. Anything
/// else reads, field by field (see [`clean_reported`]).
pub fn read_routes(bytes: &[u8], sender: &str) -> Option<Reported> {
    if bytes.len() > ROUTES_BYTES {
        return None;
    }
    let doc: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    Some(clean_reported(&doc, sender))
}

/// What a document says, each field read on its own so a bad one costs
/// only itself, in the manner of [`lenient_held`]: hop zero, ids that are
/// not lowercase hex, and the sender's own id are dropped, and every list is
/// capped. Another version reads as nothing said.
pub fn clean_reported(doc: &serde_json::Value, sender: &str) -> Reported {
    use serde_json::Value;
    let Some(doc) = doc.as_object() else { return Reported::default() };
    if doc.get("v").is_some_and(|version| version.as_u64() != Some(ROUTES_VERSION)) {
        return Reported::default();
    }
    let hop = |value: &Value| value.as_u64().filter(|&hop| hop != 0);
    let hops = |value: Option<&Value>| {
        let mut hops: Vec<u64> = value.and_then(Value::as_array).map(|list| list.iter().filter_map(hop).collect()).unwrap_or_default();
        hops.sort_unstable();
        hops.dedup();
        hops.truncate(GATES_MAX);
        hops
    };
    let path = |value: &Value| -> Option<Vec<Hop>> {
        let steps = value.as_array()?;
        Some(steps.iter().filter_map(|step| clean_hop(step, sender)).take(PATH_MAX).collect())
    };
    let except = doc
        .get("x")
        .and_then(Value::as_object)
        .map(|except| {
            except
                .iter()
                .filter(|(key, _)| is_prefix(key) && !sender.starts_with(key.as_str()))
                .filter_map(|(key, said)| match said {
                    Value::Null => Some((key.clone(), None)),
                    Value::Array(_) => Some((key.clone(), path(said))),
                    _ => None,
                })
                .take(EXCEPT_MAX)
                .collect()
        })
        .unwrap_or_default();
    Reported {
        default: doc.get("d").and_then(path),
        except,
        origin: doc.get("o").and_then(hop),
        gates: hops(doc.get("g")),
        venue: hops(doc.get("r")),
        heard: doc.get("n").and_then(Value::as_u64).map(|heard| heard.min(u64::from(u32::MAX)) as u32),
    }
}

/// An id or the start of one: lowercase hex, at least the shortest prefix.
fn is_prefix(text: &str) -> bool {
    (PREFIXES[0]..=64).contains(&text.len()) && text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn clean_hop(step: &serde_json::Value, sender: &str) -> Option<Hop> {
    match step {
        serde_json::Value::String(text) if text == ROUTES_VENUE => Some(Hop::Venue),
        serde_json::Value::String(text) if is_prefix(text) && !sender.starts_with(text.as_str()) => Some(Hop::Person(text.clone())),
        serde_json::Value::Number(_) => step.as_u64().filter(|&hop| hop != 0).map(Hop::Unnamed),
        _ => None,
    }
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
    /// The node a person says their media goes to (`via`), when it is one.
    pub says: Option<usize>,
}

/// What an edge says about the link between two nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    /// A session is held: data could flow. Grey.
    Held,
    /// Someone who says nothing, drawn to the meeting place, because that is
    /// where a build without these fields sends. A guess, so it is drawn as
    /// one, and loses to anything anyone says.
    Assumed,
    /// Someone's media is sent over it now, one way or both.
    Flowing,
}

/// One edge, undirected: `from` sorts before `to`, and so does its index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub state: State,
    /// How many people's media travels from `from` to `to` over it: one per
    /// sender and receiver whose path takes it that way.
    pub forth: u32,
    /// The same, from `to` to `from`.
    pub back: u32,
}

impl Edge {
    /// Whether media travels from `from` to `to`.
    pub fn carries_forth(&self) -> bool {
        self.forth > 0
    }

    /// Whether media travels from `to` to `from`.
    pub fn carries_back(&self) -> bool {
        self.back > 0
    }
}

/// One person's media to one other, by index into [`Web::nodes`]: the
/// sender first, the receiver last, whatever carries it between.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Flow {
    pub path: Vec<usize>,
}

impl Flow {
    pub fn sender(&self) -> usize {
        self.path[0]
    }

    pub fn receiver(&self) -> usize {
        self.path[self.path.len() - 1]
    }

    /// What carries it, nearest the sender first; empty when direct.
    pub fn between(&self) -> &[usize] {
        &self.path[1..self.path.len() - 1]
    }
}

/// The whole picture: nodes sorted by id, edges by their two ends, flows by
/// their paths, so equal inputs give equal webs and a redraw can be skipped
/// when nothing changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Web {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub flows: Vec<Flow>,
}

impl Web {
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.nodes.binary_search_by(|node| node.id.as_str().cmp(id)).ok()
    }
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
    /// The hosts they hold a session to, cleaned, and [`HELD_RELAY`] for the
    /// meeting place.
    pub held: Vec<String>,
    /// Their origin's hop while they host, as their presence says: what
    /// names them in a route's chain to anyone without their own report.
    pub hop: Option<u64>,
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

/// The id of a relay nobody named: its hop, which is a random number its
/// process picked, not an address.
fn unnamed_id(hop: u64) -> String {
    format!("hop:{hop:016x}")
}

/// Whether `id` is a relay nobody named, for its label.
pub fn is_unnamed(id: &str) -> bool {
    id.starts_with("hop:")
}

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
        hop: stats.host_hop().or_else(|| stats.offer().map(|offer| offer.hop)),
    }
}

/// The web of a room from presence alone: what a room of builds without
/// `routes.json` draws, and every room while the reports are on their way.
pub fn build(people: &[Person], relays: &[Relay], venue: Option<&str>) -> Web {
    build_reported(people, relays, venue, &BTreeMap::new())
}

/// Who a hop is, from everyone's claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Claim<'p> {
    Person(&'p str),
    Venue,
    /// Claimed by two: named by neither, and drawn as nobody.
    Contested,
}

/// A step of a path once named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step<'p> {
    Node(&'p str),
    Unnamed(u64),
}

/// Each receiver's report turned into paths between people, by id: every
/// hop named from the claims of every author (their own origin and gates),
/// hosts' hops from presence, and the meeting place's hops; a hop still
/// unclaimed is a relay of its own.
///
/// **A hop right after a sender who says nothing about their own hops is
/// taken for their gate** and left out (assumed, not read): a gate sits
/// there in every chain of a member joined over several sessions, and an
/// older build does not say its gates, so it would otherwise draw a relay
/// per session of everyone on one.
fn resolve<'p>(
    people: &'p [Person],
    reported: &'p BTreeMap<String, Reported>,
    venue: Option<&'p str>,
) -> Vec<(&'p str, &'p str, Vec<Step<'p>>)> {
    let mut ids: Vec<&str> = people.iter().map(|person| person.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    let matching = |prefix: &str| -> Option<&'p str> {
        let start = ids.partition_point(|id| *id < prefix);
        let mut found = ids[start..].iter().take_while(|id| id.starts_with(prefix));
        let first = found.next()?;
        found.next().is_none().then_some(*first)
    };
    let known = |id: &str| ids.binary_search(&id).is_ok();
    let mut claims: BTreeMap<u64, Claim<'p>> = BTreeMap::new();
    let mut claim = |hop: u64, who: Claim<'p>| {
        claims.entry(hop).and_modify(|seen| if *seen != who { *seen = Claim::Contested }).or_insert(who);
    };
    for person in people {
        if let Some(hop) = person.hop {
            claim(hop, Claim::Person(&person.id));
        }
    }
    for (author, report) in reported.iter().filter(|(author, _)| known(author)) {
        for &hop in report.origin.iter().chain(&report.gates) {
            claim(hop, Claim::Person(author));
        }
        for &hop in &report.venue {
            claim(hop, Claim::Venue);
        }
    }
    let mut paths = Vec::new();
    for (receiver, report) in reported.iter().filter(|(author, _)| known(author)) {
        let receiver = receiver.as_str();
        // An exception whose prefix fits several people is nobody's, and
        // those it fits are left undrawn rather than given the default the
        // writer meant them not to have.
        const UNSURE: &Option<Vec<Hop>> = &None;
        let mut except: BTreeMap<&str, &Option<Vec<Hop>>> = BTreeMap::new();
        for (key, said) in &report.except {
            match matching(key) {
                Some(id) => {
                    except.entry(id).or_insert(said);
                }
                None => {
                    let start = ids.partition_point(|id| *id < key.as_str());
                    for id in ids[start..].iter().take_while(|id| id.starts_with(key.as_str())) {
                        except.insert(id, UNSURE);
                    }
                }
            }
        }
        for sender in people.iter().map(|person| person.id.as_str()).filter(|&id| id != receiver) {
            let said = match except.get(sender) {
                Some(said) => said.as_ref(),
                None => report.default.as_ref(),
            };
            let Some(hops) = said else { continue };
            let silent = !reported.contains_key(sender);
            let steps = hops
                .iter()
                .enumerate()
                .filter_map(|(at, hop)| match hop {
                    Hop::Person(prefix) => matching(prefix).map(Step::Node),
                    Hop::Venue => venue.map(Step::Node),
                    Hop::Unnamed(hop) => match claims.get(hop) {
                        Some(Claim::Person(id)) => Some(Step::Node(id)),
                        Some(Claim::Venue) => venue.map(Step::Node),
                        Some(Claim::Contested) => None,
                        None if at == 0 && silent => None,
                        None => Some(Step::Unnamed(*hop)),
                    },
                })
                // Their own hops and ours are no hop between them.
                .filter(|step| !matches!(step, Step::Node(id) if *id == sender || *id == receiver))
                .collect();
            paths.push((sender, receiver, steps));
        }
    }
    paths
}

/// The web of a room: `people` (us included), the relay servers to draw,
/// `venue`, the node a `via` of "relay" lands on (a relay's id or a
/// person's; `None` in a direct call), and what each receiver reported,
/// by their id.
///
/// **Which way media travels is read from the receivers.** Each flow's
/// path, sender to receiver, takes every link it crosses one way, and a
/// link's two directions are counted apart. Where nobody reports, presence
/// stands in, as it always did: a `via` carries from its sayer to where it
/// points, and back as well when the sayer reports nothing of what it
/// receives (assumed: a build without reports receives where it sends).
pub fn build_reported(
    people: &[Person],
    relays: &[Relay],
    venue: Option<&str>,
    reported: &BTreeMap<String, Reported>,
) -> Web {
    let mut base: BTreeMap<&str, Node> = BTreeMap::new();
    for relay in relays {
        base.entry(&relay.id).or_insert_with(|| Node {
            kind: Kind::Relay,
            id: relay.id.clone(),
            name: relay.name.clone(),
            can_host: false,
            me: false,
            says: None,
        });
    }
    // A person wins over a relay of the same id: someone in the room is who
    // the endpoint turned out to be.
    for person in people {
        base.insert(
            &person.id,
            Node {
                kind: Kind::Participant,
                id: person.id.clone(),
                name: person.name.clone(),
                can_host: person.can_host,
                me: person.me,
                says: None,
            },
        );
    }
    let venue = venue.filter(|venue| base.contains_key(venue));

    let paths = resolve(people, reported, venue);
    let unnamed: BTreeSet<u64> = paths
        .iter()
        .flat_map(|(_, _, steps)| steps.iter())
        .filter_map(|step| match step {
            Step::Unnamed(hop) => Some(*hop),
            Step::Node(_) => None,
        })
        .collect();
    let unnamed: BTreeMap<u64, String> = unnamed.into_iter().take(UNNAMED_MAX).map(|hop| (hop, unnamed_id(hop))).collect();
    let mut nodes: Vec<Node> = base.into_values().collect();
    for id in unnamed.values() {
        nodes.push(Node { kind: Kind::Relay, id: id.clone(), name: None, can_host: false, me: false, says: None });
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let index = |id: &str| nodes.binary_search_by(|node| node.id.as_str().cmp(id)).ok();

    let mut flows: Vec<Flow> = paths
        .iter()
        .filter_map(|(sender, receiver, steps)| {
            let mut path = vec![index(sender)?];
            path.extend(steps.iter().filter_map(|step| match step {
                Step::Node(id) => index(id),
                Step::Unnamed(hop) => unnamed.get(hop).and_then(|id| index(id)),
            }));
            path.push(index(receiver)?);
            path.dedup();
            (path.len() >= 2).then_some(Flow { path })
        })
        .collect();
    flows.sort();

    /// A link as it is gathered: what presence says of it, and the streams
    /// each way, `forth` from the lower index to the higher.
    #[derive(Default)]
    struct Link {
        state: Option<State>,
        forth: u32,
        back: u32,
    }
    let mut links: BTreeMap<(usize, usize), Link> = BTreeMap::new();
    let mut said = |a: usize, b: usize, state: State| {
        if a != b {
            let slot = &mut links.entry((a.min(b), a.max(b))).or_default().state;
            *slot = Some((*slot).map_or(state, |seen| seen.max(state)));
        }
    };
    let venue_at = venue.and_then(index);
    for person in people {
        let Some(at) = index(&person.id) else { continue };
        for host in &person.held {
            let to = if host == HELD_RELAY { venue_at } else { index(host) };
            if let Some(to) = to {
                said(at, to, State::Held);
            }
        }
        if person.via.is_none() && !person.me && person.held.is_empty() {
            if let Some(to) = venue_at {
                said(at, to, State::Assumed);
            }
        }
    }
    let carry = |links: &mut BTreeMap<(usize, usize), Link>, from: usize, to: usize, streams: u32| {
        if from == to {
            return;
        }
        let link = links.entry((from.min(to), from.max(to))).or_default();
        let count = if from < to { &mut link.forth } else { &mut link.back };
        *count = count.saturating_add(streams);
    };
    for flow in &flows {
        for pair in flow.path.windows(2) {
            carry(&mut links, pair[0], pair[1], 1);
        }
    }
    let mut says = Vec::new();
    for person in people {
        let Some(at) = index(&person.id) else { continue };
        let to = match &person.via {
            Some(Sending::Relay) => venue_at,
            Some(Sending::Host(id)) => index(id),
            Some(Sending::Guests) | None => None,
        };
        let Some(to) = to.filter(|&to| to != at) else { continue };
        says.push((at, to));
        let link = links.entry((at.min(to), at.max(to))).or_default();
        let (out, back) = if at < to { (&mut link.forth, &mut link.back) } else { (&mut link.back, &mut link.forth) };
        *out = (*out).max(1);
        if !reported.contains_key(&person.id) {
            *back = (*back).max(1);
        }
    }

    for (at, to) in says {
        nodes[at].says = Some(to);
    }
    let edges = links
        .into_iter()
        .map(|((a, b), link)| Edge {
            from: nodes[a].id.clone(),
            to: nodes[b].id.clone(),
            state: if link.forth + link.back > 0 { State::Flowing } else { link.state.unwrap_or(State::Held) },
            forth: link.forth,
            back: link.back,
        })
        .collect();
    Web { nodes, edges, flows }
}

/// `web` with a line both ways between `me` and each of `straight`: a
/// direct call, where the one session between two people carries both ways
/// and nobody says a `via`, since nothing meets anywhere. Ids that are not
/// nodes draw nothing, as in [`build`], and the edges stay sorted.
pub fn direct(mut web: Web, me: &str, straight: &[&str]) -> Web {
    let Some(at) = web.index_of(me) else { return web };
    for &them in straight {
        let Some(other) = web.index_of(them).filter(|&other| other != at) else { continue };
        let (from, to) = (web.nodes[at.min(other)].id.clone(), web.nodes[at.max(other)].id.clone());
        match web.edges.iter_mut().find(|seen| seen.from == from && seen.to == to) {
            Some(seen) => {
                seen.state = State::Flowing;
                seen.forth = seen.forth.max(1);
                seen.back = seen.back.max(1);
            }
            None => web.edges.push(Edge { from, to, state: State::Flowing, forth: 1, back: 1 }),
        }
    }
    web.edges.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    web
}

/// Where what a sender says of its own media and what its receivers see
/// part: it says it sends to `says`, and `count` receivers see its media
/// arrive first through `seen` (the receiver itself when direct).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disagreement {
    pub says: usize,
    pub seen: usize,
    pub count: usize,
}

/// Every place `id`'s own `via` and its receivers' first hop part, by the
/// node seen. Nothing for someone who says no `via`, or who sends only to
/// their guests.
pub fn disagreements(web: &Web, id: &str) -> Vec<Disagreement> {
    let Some(at) = web.index_of(id) else { return Vec::new() };
    let Some(says) = web.nodes[at].says else { return Vec::new() };
    let mut seen: BTreeMap<usize, usize> = BTreeMap::new();
    for flow in web.flows.iter().filter(|flow| flow.sender() == at) {
        let first = flow.path[1];
        if first != says {
            *seen.entry(first).or_default() += 1;
        }
    }
    seen.into_iter().map(|(seen, count)| Disagreement { says, seen, count }).collect()
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

    fn edge(a: &str, b: &str, state: State) -> (String, String, State) {
        let (from, to) = if a < b { (a, b) } else { (b, a) };
        (from.to_string(), to.to_string(), state)
    }

    /// The edges as the tests before directions compared them.
    trait Lines {
        fn lines(&self) -> Vec<(String, String, State)>;
    }

    impl Lines for Web {
        fn lines(&self) -> Vec<(String, String, State)> {
            self.edges.iter().map(|edge| (edge.from.clone(), edge.to.clone(), edge.state)).collect()
        }
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
            web.lines(),
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
            web.lines(),
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
        expected.sort();
        assert_eq!(web.lines(), expected);
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
        assert_eq!(web.lines(), vec![edge(&id('a'), &c, State::Flowing), edge(&id('b'), &c, State::Flowing)]);
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
        assert_eq!(web.lines()[1], edge(&id('b'), RELAY_ID, State::Assumed));
        // Saying anything at all ends the guess, and so does being us.
        let people = vec![Person { me: true, ..says(&id('a'), None, &[]) }, says(&id('b'), None, &[&id('a')])];
        assert_eq!(build(&people, &server(), Some(RELAY_ID)).lines(), vec![edge(&id('a'), &id('b'), State::Held)]);
    }

    #[test]
    fn held_carries_the_word_for_the_meeting_place_first_and_never_capped_away() {
        let json = format!(r#"{{"held":["{b}","relay","Relay","{a}","relay"]}}"#, a = id('a'), b = id('b'));
        assert_eq!(read(&json).held, vec![HELD_RELAY.to_string(), id('a'), id('b')]);
        let mut many: Vec<String> = (0..100).map(numbered).collect();
        many.push(HELD_RELAY.to_string());
        let held = clean_held(many, "");
        assert_eq!(held.len(), HELD_MAX);
        assert_eq!(held[0], HELD_RELAY);
        // `guests` is a `via` word only.
        assert!(clean_held(["guests".to_string()], "").is_empty());
    }

    #[test]
    fn someone_through_a_host_who_holds_the_relay_has_a_grey_line_to_it() {
        let h = id('d');
        let people = vec![
            Person { can_host: true, ..says(&h, Some(Sending::Relay), &[HELD_RELAY]) },
            says(&id('a'), host(&h), &[HELD_RELAY, &h]),
        ];
        let web = build(&people, &server(), Some(RELAY_ID));
        assert_eq!(
            web.lines(),
            vec![
                edge(&id('a'), &h, State::Flowing),
                edge(&id('a'), RELAY_ID, State::Held),
                edge(&h, RELAY_ID, State::Flowing),
            ]
        );
        // The same in a room meeting at an iroh relay server, whose id is not
        // the word.
        let at = meeting(Some(&MeetsAt::Endpoint(id('e'))), &id('a'), &people);
        let web = build(&people, &[at.relay.clone().unwrap()], Some(&at.venue));
        assert!(web.lines().contains(&edge(&id('a'), &id('e'), State::Held)));
        // And in a direct call, with no meeting place, the word draws nothing.
        assert_eq!(build(&[says(&id('a'), None, &[HELD_RELAY])], &[], None).lines(), vec![]);
    }

    #[test]
    fn ids_that_name_nobody_draw_nothing() {
        let people = vec![says(&id('a'), host(&id('f')), &[&id('e')])];
        let web = build(&people, &server(), Some(RELAY_ID));
        assert_eq!(web.nodes.len(), 2);
        assert!(web.lines().is_empty());
        // A venue that is not drawn takes no edges either.
        assert!(build(&[says(&id('a'), Some(Sending::Relay), &[])], &[], Some(RELAY_ID)).lines().is_empty());
    }

    #[test]
    fn a_direct_call_draws_one_flowing_line_between_the_two() {
        let (me, them) = (id('b'), id('a'));
        let people = vec![Person { me: true, ..says(&me, None, &[]) }, says(&them, None, &[])];
        let web = build(&people, &[], None);
        assert!(web.lines().is_empty(), "nothing said, nothing meets");
        let web = direct(web, &me, &[&them, &id('f'), &me]);
        assert_eq!(web.lines(), vec![edge(&me, &them, State::Flowing)], "and nobody unknown, nor us to us");
        // Saying it again changes nothing.
        assert_eq!(direct(web.clone(), &me, &[&them]), web);
    }

    #[test]
    fn a_link_said_both_ways_is_one_edge_at_its_strongest() {
        let (a, h) = (id('a'), id('d'));
        let people = vec![says(&a, host(&h), &[&h]), says(&h, None, &[&a])];
        assert_eq!(build(&people, &[], None).lines(), vec![edge(&a, &h, State::Flowing)]);
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

    // What receivers report, and what it draws.

    fn person(who: &str, via: Option<Sending>, held: &[&str]) -> Person {
        says(who, via, held)
    }

    /// `author`'s report, through the wire and back, with `paths` by full id.
    fn report(
        author: &str,
        origin: u64,
        gates: &[u64],
        venue: &[u64],
        paths: &[(&str, Option<Vec<Hop>>)],
        roster: &[&str],
    ) -> (String, Reported) {
        let routes = Routes {
            origin,
            gates: gates.to_vec(),
            venue: venue.to_vec(),
            paths: paths.iter().map(|(id, path)| (id.to_string(), path.clone())).collect(),
        };
        let document = say_routes(&routes, roster);
        (author.to_string(), read_routes(&document, author).expect("our own document reads"))
    }

    fn named(id: &str) -> Hop {
        Hop::Person(id.to_string())
    }

    /// Whether media travels from `a` to `b` on their edge.
    fn carries(web: &Web, a: &str, b: &str) -> bool {
        let (from, to) = if a < b { (a, b) } else { (b, a) };
        let edge = web.edges.iter().find(|edge| edge.from == from && edge.to == to).unwrap_or_else(|| panic!("no edge {a} {b}"));
        if a == from { edge.carries_forth() } else { edge.carries_back() }
    }

    #[test]
    fn prefixes_lengthen_on_a_collision_and_an_ambiguous_one_is_dropped() {
        let x = format!("abcdef01{}", "1".repeat(56));
        let y = format!("abcdef01{}", "2".repeat(56));
        let (z1, z2) = (format!("0123456789abcdef{}", "a".repeat(48)), format!("0123456789abcdef{}", "b".repeat(48)));
        let plain = id('c');
        let roster = [x.as_str(), y.as_str(), z1.as_str(), z2.as_str(), plain.as_str()];
        assert_eq!(prefix(&plain, &roster), "cccccccc");
        assert_eq!(prefix(&x, &roster), "abcdef011111");
        assert_eq!(prefix(&z1, &roster), "0123456789abcdef", "even sixteen collide: the longest");
        let author = id('9');
        let (_, said) = report(&author, 1, &[], &[], &[(&x, Some(vec![])), (&y, None), (&z1, Some(vec![named(&x)])), (&plain, Some(vec![Hop::Venue]))], &roster);
        // x's path is the default (of equals, the one that sorts first);
        // y's exception is under the lengthened prefix.
        assert_eq!(said.default, Some(vec![]));
        assert!(said.except.contains_key("abcdef012222") && said.except.contains_key("0123456789abcdef"));
        // Read against the same roster: x and y each match one person, the
        // sixteen-digit prefix both z's, so it is dropped.
        let people: Vec<Person> = roster.iter().chain([&author.as_str()]).map(|who| person(who, None, &[])).collect();
        let reported = BTreeMap::from([(author.clone(), said)]);
        let web = build_reported(&people, &[], None, &reported);
        let flow = |from: &str| web.flows.iter().find(|flow| web.nodes[flow.sender()].id == from && web.nodes[flow.receiver()].id == author);
        assert_eq!(flow(&x).map(|flow| flow.path.len()), Some(2), "direct");
        assert!(flow(&y).is_none(), "not received");
        assert!(flow(&z1).is_none() && flow(&z2).is_none(), "ambiguous, so nobody's");
    }

    #[test]
    fn the_default_and_exceptions_round_trip_and_null_is_not_received() {
        let (a, b, c, d) = (id('a'), id('b'), id('c'), id('d'));
        let roster = [a.as_str(), b.as_str(), c.as_str(), d.as_str()];
        let (_, said) = report(&a, 10, &[11], &[99], &[(&b, Some(vec![Hop::Venue])), (&c, Some(vec![Hop::Venue])), (&d, None)], &roster);
        assert_eq!(said.default, Some(vec![Hop::Venue]));
        assert_eq!(said.except, BTreeMap::from([("dddddddd".to_string(), None)]));
        assert_eq!((said.origin, said.gates.as_slice(), said.venue.as_slice(), said.heard), (Some(10), &[11][..], &[99][..], Some(2)));
        // Everyone on the default says no exceptions at all.
        let document = say_routes(&Routes { origin: 1, paths: BTreeMap::from([(b.clone(), Some(vec![]))]), ..Routes::default() }, &roster);
        let text = String::from_utf8(document).unwrap();
        assert_eq!(text, r#"{"d":[],"n":1,"o":1,"v":1}"#);
    }

    #[test]
    fn every_cap_holds() {
        // Too large to look at, whatever it says.
        let padded = format!(r#"{{"v":1,"pad":"{}"}}"#, "x".repeat(ROUTES_BYTES));
        assert!(read_routes(padded.as_bytes(), "").is_none());
        // Every list capped.
        let except: serde_json::Map<String, serde_json::Value> =
            (0..200).map(|n| (format!("{n:08x}"), serde_json::json!([]))).collect();
        let doc = serde_json::json!({
            "v": 1,
            "g": (1..40).collect::<Vec<u64>>(),
            "r": (1..40).collect::<Vec<u64>>(),
            "d": (1..40).collect::<Vec<u64>>(),
            "x": except,
        });
        let said = clean_reported(&doc, "");
        assert_eq!(said.except.len(), EXCEPT_MAX);
        assert_eq!(said.gates.len(), GATES_MAX);
        assert_eq!(said.venue.len(), GATES_MAX);
        assert_eq!(said.default.map(|path| path.len()), Some(PATH_MAX));
        // The writer keeps under the size, leaving exceptions out.
        let roster: Vec<String> = (0..300).map(|n| format!("{:08x}{}", n * 7919 + 1, "0".repeat(56))).collect();
        let refs: Vec<&str> = roster.iter().map(String::as_str).collect();
        let long: Vec<Hop> = (0..PATH_MAX as u64).map(|hop| Hop::Unnamed(u64::MAX - hop)).collect();
        let routes = Routes {
            origin: 1,
            paths: roster.iter().enumerate().map(|(n, id)| (id.clone(), Some(if n % 2 == 0 { long.clone() } else { vec![Hop::Unnamed(n as u64 + 1)] }))).collect(),
            ..Routes::default()
        };
        let document = say_routes(&routes, &refs);
        assert!(document.len() <= ROUTES_BYTES, "{}", document.len());
        let said = read_routes(&document, "").unwrap();
        assert!(!said.except.is_empty() && said.except.len() <= EXCEPT_MAX);
    }

    #[test]
    fn one_bad_field_costs_only_itself() {
        let me = id('e');
        let doc = serde_json::json!({
            "v": 1,
            "o": "nope",
            "g": [1, "x", 0, 2, -3, 2],
            "n": "many",
            "d": 5,
            "x": {
                "ZZZZZZZZ": [],
                "abcdef01": "bad",
                "eeeeeeee": [],
                "aaaaaaaa": [1, "relay", {}, "bbbbbbbb", "eeeeeeee", 0, "Relay", 2.5],
                "cccccccc": null,
            },
        });
        let said = clean_reported(&doc, &me);
        assert_eq!(said.origin, None);
        assert_eq!(said.gates, vec![1, 2]);
        assert_eq!(said.heard, None);
        assert_eq!(said.default, None);
        assert_eq!(
            said.except,
            BTreeMap::from([
                ("aaaaaaaa".to_string(), Some(vec![Hop::Unnamed(1), Hop::Venue, Hop::Person("bbbbbbbb".into())])),
                ("cccccccc".to_string(), None),
            ]),
            "the author's own id, a non-hex key and a non-path value go; so do bad steps"
        );
        // Not an object, or another version: nothing said.
        assert_eq!(clean_reported(&serde_json::json!([1, 2]), ""), Reported::default());
        assert_eq!(clean_reported(&serde_json::json!({"v": 2, "o": 5}), ""), Reported::default());
        assert!(read_routes(b"{not json", "").is_none());
    }

    /// A relay room of three, each joined over several sessions: every
    /// chain names the sender's gate first, which its own report claims.
    fn relay_room() -> (Vec<Person>, BTreeMap<String, Reported>) {
        let (a, b, c) = (id('a'), id('b'), id('c'));
        let roster = [a.as_str(), b.as_str(), c.as_str()];
        let people = vec![
            Person { me: true, ..person(&a, Some(Sending::Relay), &[HELD_RELAY]) },
            person(&b, Some(Sending::Relay), &[HELD_RELAY]),
            person(&c, Some(Sending::Relay), &[HELD_RELAY]),
        ];
        let gate = |who: &str| match who.as_bytes()[0] { b'a' => 11, b'b' => 21, _ => 31 };
        let reported = roster
            .iter()
            .enumerate()
            .map(|(n, &author)| {
                let paths: Vec<(&str, Option<Vec<Hop>>)> = roster
                    .iter()
                    .filter(|&&other| other != author)
                    .map(|&other| (other, Some(vec![Hop::Unnamed(gate(other)), Hop::Unnamed(99)])))
                    .collect();
                report(author, 10 * (n as u64 + 1), &[gate(author)], &[99], &paths, &roster)
            })
            .collect();
        (people, reported)
    }

    #[test]
    fn in_a_relay_room_every_link_to_the_relay_carries_both_ways() {
        let (people, reported) = relay_room();
        let web = build_reported(&people, &server(), Some(RELAY_ID), &reported);
        assert_eq!(web.nodes.len(), 4, "gates are their owners', so no relay is made of one");
        assert_eq!(web.flows.len(), 6);
        for c in ['a', 'b', 'c'] {
            assert!(carries(&web, &id(c), RELAY_ID) && carries(&web, RELAY_ID, &id(c)), "{c}");
            let edge = web.edges.iter().find(|edge| edge.from == id(c)).unwrap();
            assert_eq!((edge.forth, edge.back), (2, 2), "{c} sends to two and hears two");
        }
        assert!(web.flows.iter().all(|flow| flow.between() == [web.index_of(RELAY_ID).unwrap()]));
        assert!(people.iter().all(|person| disagreements(&web, &person.id).is_empty()));
    }

    #[test]
    fn a_call_moved_to_a_host_carries_through_it_and_leaves_the_relay_held() {
        let (a, b, h) = (id('a'), id('b'), id('d'));
        let roster = [a.as_str(), b.as_str(), h.as_str()];
        let people = vec![
            Person { can_host: true, hop: Some(50), ..person(&h, Some(Sending::Relay), &[HELD_RELAY]) },
            Person { me: true, ..person(&a, host(&h), &[HELD_RELAY, &h]) },
            person(&b, host(&h), &[HELD_RELAY, &h]),
        ];
        let reported = BTreeMap::from([
            report(&a, 10, &[11, 12], &[99], &[(&b, Some(vec![named(&h)])), (&h, Some(vec![]))], &roster),
            report(&b, 20, &[21, 22], &[99], &[(&a, Some(vec![named(&h)])), (&h, Some(vec![]))], &roster),
            // The host hears its guests straight off their sessions.
            report(&h, 50, &[], &[99], &[(&a, Some(vec![])), (&b, Some(vec![]))], &roster),
        ]);
        let web = build_reported(&people, &server(), Some(RELAY_ID), &reported);
        for guest in [&a, &b] {
            assert!(carries(&web, guest, &h) && carries(&web, &h, guest));
            let edge = web.edges.iter().find(|edge| &edge.from == guest && edge.to == RELAY_ID).unwrap();
            assert_eq!(edge.state, State::Held, "held, nothing on it");
        }
        // The host sends on to the relay, as it says, and nobody reports
        // hearing anything from it.
        assert!(carries(&web, &h, RELAY_ID) && !carries(&web, RELAY_ID, &h));
    }

    #[test]
    fn with_the_relay_stopped_a_guest_sends_through_one_host_and_hears_the_other_straight() {
        // H1 and H2 host; G sends via H1 and holds both; K sends via H2.
        let (h1, h2, g, k) = (id('1'), id('2'), id('a'), id('b'));
        let roster = [h1.as_str(), h2.as_str(), g.as_str(), k.as_str()];
        let people = vec![
            Person { can_host: true, hop: Some(100), ..person(&h1, Some(Sending::Guests), &[]) },
            Person { can_host: true, hop: Some(200), ..person(&h2, Some(Sending::Guests), &[&h1]) },
            Person { me: true, ..person(&g, host(&h1), &[&h1, &h2]) },
            person(&k, host(&h2), &[&h1, &h2]),
        ];
        let reported = BTreeMap::from([
            report(&g, 30, &[31, 32], &[], &[(&h1, Some(vec![])), (&h2, Some(vec![])), (&k, Some(vec![named(&h2)]))], &roster),
            report(&h1, 100, &[], &[], &[(&g, Some(vec![])), (&h2, Some(vec![])), (&k, Some(vec![named(&h2)]))], &roster),
            report(&h2, 200, &[], &[], &[(&g, Some(vec![named(&h1)])), (&h1, Some(vec![])), (&k, Some(vec![]))], &roster),
            report(&k, 40, &[41, 42], &[], &[(&g, Some(vec![named(&h1)])), (&h1, Some(vec![])), (&h2, Some(vec![]))], &roster),
        ]);
        let web = build_reported(&people, &[], None, &reported);
        // G–H1: G's half white (it sends there), H1's white (H1's own).
        assert!(carries(&web, &g, &h1) && carries(&web, &h1, &g));
        // G–H2: H2's half white, G's grey: H2 hears G through H1.
        assert!(carries(&web, &h2, &g) && !carries(&web, &g, &h2));
        let edge = web.edges.iter().find(|edge| edge.from == h2 && edge.to == g).unwrap();
        assert_eq!(edge.state, State::Flowing);
        // H1 carries G's media on to H2 and K.
        assert!(carries(&web, &h1, &h2) && carries(&web, &h1, &k));
        assert!(disagreements(&web, &g).is_empty());
    }

    #[test]
    fn a_sender_seen_arriving_elsewhere_than_it_says_is_a_disagreement() {
        let (a, b, c, h) = (id('a'), id('b'), id('c'), id('d'));
        let roster = [a.as_str(), b.as_str(), c.as_str(), h.as_str()];
        let people = vec![
            Person { me: true, ..person(&a, Some(Sending::Relay), &[HELD_RELAY]) },
            person(&b, Some(Sending::Relay), &[HELD_RELAY]),
            person(&c, Some(Sending::Relay), &[HELD_RELAY]),
            Person { can_host: true, hop: Some(50), ..person(&h, Some(Sending::Relay), &[HELD_RELAY]) },
        ];
        let through_h = || Some(vec![named(&h), Hop::Venue]);
        let reported = BTreeMap::from([
            report(&b, 20, &[], &[99], &[(&a, through_h()), (&c, Some(vec![Hop::Venue])), (&h, Some(vec![Hop::Venue]))], &roster),
            report(&c, 30, &[], &[99], &[(&a, through_h()), (&b, Some(vec![Hop::Venue])), (&h, Some(vec![Hop::Venue]))], &roster),
            report(&h, 50, &[], &[99], &[(&a, Some(vec![])), (&b, Some(vec![Hop::Venue])), (&c, Some(vec![Hop::Venue]))], &roster),
        ]);
        let web = build_reported(&people, &server(), Some(RELAY_ID), &reported);
        let (relay, at_h) = (web.index_of(RELAY_ID).unwrap(), web.index_of(&h).unwrap());
        assert_eq!(disagreements(&web, &a), vec![Disagreement { says: relay, seen: at_h, count: 3 }]);
        assert!(disagreements(&web, &b).is_empty());
    }

    #[test]
    fn claims_name_hops_and_what_nobody_claims_is_a_relay_capped() {
        let (a, b, h, k) = (id('a'), id('b'), id('d'), id('e'));
        let roster = [a.as_str(), b.as_str(), h.as_str(), k.as_str()];
        let people = vec![
            Person { me: true, ..person(&a, Some(Sending::Relay), &[]) },
            person(&b, Some(Sending::Relay), &[]),
            // A host named by its presence hop only.
            Person { can_host: true, hop: Some(500), ..person(&h, Some(Sending::Relay), &[]) },
            person(&k, Some(Sending::Relay), &[]),
        ];
        // A hears B through 600 (K's gate, by K's claim), then 500 (H by
        // presence), then 777, which nobody claims.
        let reported = BTreeMap::from([
            report(&a, 10, &[], &[], &[(&b, Some(vec![Hop::Unnamed(600), Hop::Unnamed(500), Hop::Unnamed(777)]))], &roster),
            report(&k, 40, &[600], &[], &[], &roster),
        ]);
        let web = build_reported(&people, &server(), Some(RELAY_ID), &reported);
        let flow = web.flows.iter().find(|flow| web.nodes[flow.sender()].id == b).unwrap();
        let names: Vec<&str> = flow.path.iter().map(|&at| web.nodes[at].id.as_str()).collect();
        assert_eq!(names, [b.as_str(), k.as_str(), h.as_str(), &unnamed_id(777), a.as_str()]);
        assert_eq!(web.nodes.iter().find(|node| node.id == unnamed_id(777)).map(|node| node.kind), Some(Kind::Relay));
        // A hop claimed by two names neither, and is no relay either.
        let mut contested = reported.clone();
        contested.insert(b.clone(), report(&b, 600, &[], &[], &[], &roster).1);
        let web = build_reported(&people, &server(), Some(RELAY_ID), &contested);
        let flow = web.flows.iter().find(|flow| web.nodes[flow.sender()].id == b).unwrap();
        assert_eq!(flow.path.len(), 4, "{:?}", flow.path);
        // Sixty unclaimed hops make fifty relays.
        let many: Vec<Hop> = (1000..1060).map(Hop::Unnamed).collect();
        let mut wide = BTreeMap::new();
        for (n, chunk) in many.chunks(PATH_MAX).enumerate() {
            let sender = &roster[n % 2 + 1];
            let author = id(char::from(b'5' + n as u8));
            wide.insert(author.clone(), Reported { except: BTreeMap::from([(sender[..8].to_string(), Some(chunk.to_vec()))]), ..Reported::default() });
        }
        let mut crowd = people.clone();
        for author in wide.keys() {
            crowd.push(person(author, None, &[]));
        }
        let web = build_reported(&crowd, &server(), Some(RELAY_ID), &wide);
        assert_eq!(web.nodes.iter().filter(|node| is_unnamed(&node.id)).count(), UNNAMED_MAX);
    }

    #[test]
    fn a_hop_right_after_a_sender_who_reports_nothing_is_their_gate() {
        let (a, o) = (id('a'), id('b'));
        let roster = [a.as_str(), o.as_str()];
        let people = vec![Person { me: true, ..person(&a, Some(Sending::Relay), &[HELD_RELAY]) }, person(&o, Some(Sending::Relay), &[HELD_RELAY])];
        let reported = BTreeMap::from([report(&a, 10, &[], &[99], &[(&o, Some(vec![Hop::Unnamed(4242), Hop::Unnamed(99)]))], &roster)]);
        let web = build_reported(&people, &server(), Some(RELAY_ID), &reported);
        assert_eq!(web.nodes.len(), 3, "no relay made of their gate");
    }

    #[test]
    fn a_peer_on_an_older_build_is_drawn_from_presence() {
        // Nobody reports: every `via` carries both ways, as before reports.
        let people = vec![
            Person { me: true, ..says(&id('a'), Some(Sending::Relay), &[HELD_RELAY]) },
            says(&id('b'), Some(Sending::Relay), &[HELD_RELAY]),
            says(&id('c'), None, &[]),
        ];
        let web = build(&people, &server(), Some(RELAY_ID));
        assert!(carries(&web, &id('a'), RELAY_ID) && carries(&web, RELAY_ID, &id('a')));
        assert!(carries(&web, &id('b'), RELAY_ID) && carries(&web, RELAY_ID, &id('b')));
        assert_eq!(web.lines()[2], edge(&id('c'), RELAY_ID, State::Assumed));
        assert!(web.flows.is_empty());
        // We report; they do not, and never ask. Their own line is still
        // both ways (assumed back: they receive where they send), ours is as
        // we report it.
        let roster = [id('a'), id('b'), id('c')];
        let roster: Vec<&str> = roster.iter().map(String::as_str).collect();
        let reported = BTreeMap::from([report(&id('a'), 10, &[], &[99], &[(&id('b'), Some(vec![Hop::Venue])), (&id('c'), None)], &roster)]);
        let web = build_reported(&people, &server(), Some(RELAY_ID), &reported);
        assert!(carries(&web, &id('b'), RELAY_ID) && carries(&web, RELAY_ID, &id('b')));
        assert!(carries(&web, RELAY_ID, &id('a')), "we hear b through it");
        assert!(carries(&web, &id('a'), RELAY_ID), "we say we send there");
        assert_eq!(web.lines()[2].2, State::Assumed, "nobody reports c's media");
    }

    #[test]
    fn equal_reports_build_an_equal_web_whatever_the_order() {
        let (people, reported) = relay_room();
        let web = build_reported(&people, &server(), Some(RELAY_ID), &reported);
        let reversed: Vec<Person> = people.iter().rev().cloned().collect();
        assert_eq!(build_reported(&reversed, &server(), Some(RELAY_ID), &reported), web);
        assert!(web.flows.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
