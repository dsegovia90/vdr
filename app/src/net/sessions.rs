//! A join that can meet in more than one place: the room's relay and the
//! members who host it, dialled together, the call living on whatever answers.
//!
//! **Only a join that has somewhere else to go comes here**: the room has
//! hosts saved or invited, or we host it ourselves. Everyone else joins
//! through the single session `connect` makes, wired exactly as before.
//!
//! **Listen on all, publish on one.** Every session lands in the same inbound
//! origin, and the library already makes two routes to one publisher one
//! broadcast. When the route in use dies or another becomes best, it ends
//! that broadcast and bounces the path (withdrawn, announced again), and the
//! attendance loop takes the bounce as a change of route, not a leave
//! (`grace.rs`), asking for the broadcast afresh. What we publish
//! is offered on one session only, the first live one in [`targets`]' order,
//! through an origin of its own per session ([`Gate`]) whose offers name our
//! publishing identity first, so routes to us name one publisher whichever way
//! they come. Offered on every session, a stream would be pulled once per route a
//! viewer happened to rank first: one upload per host instead of one.
//!
//! **The order is the relay, then the hosts by endpoint id**, which every app
//! works out the same way from the same saved list without asking anyone. So
//! when the relay is down, everyone who can reach the same first host meets
//! there, and nobody's media has to cross between hosts.
//!
//! **Moving is make-before-break**: the new offer is made before the old one
//! is withdrawn, so a viewer has a route to ask through again the moment the
//! path is bounced. A withdrawal with
//! no replacement does not stop a track already flowing (the viewer has to stop
//! reading), which is why the old offer is never dropped on its own: a session
//! that died carries nothing, and one that comes back finds it still offered.
//! Moving down happens at once; moving back up waits until the better session
//! has held for [`HOLD`], so a relay that flaps does not drag everyone with it.
//!
//! **Host links are priced** ([`HOST_COST`]): a member dials a host as an
//! expensive link, so a viewer reaching someone both ways prefers the relay.
//! Without it a host's own video, one hop away directly and two through a
//! relay, would be pulled straight from the host by every member: N-1 uploads
//! on the host's line while the relay sat idle. Cost is carried only on
//! moq-lite-06 and later, which two of our apps negotiate with each other. A
//! relay limited to moq-lite-05 (the production relay today) carries none, and
//! its routes arrive free, so they win over a priced host link anyway
//! (`routes.rs` pins both). When that relay is upgraded its routes start
//! paying one per link, and the price here is what keeps them ahead: it must
//! stay well above any relay route's length.
//!
//! **A host publishes everything on every session.** Its one shared origin is
//! the bridge between its guests and the rest of the room, and a guest's
//! broadcast has to reach the relay through it with its own route intact,
//! which an offer re-made through a gate would lose. Pricing keeps the extra
//! routes unused while a cheaper one exists. A host dials only the hosts
//! whose id sorts below its own, so two hosts hold one session between them,
//! not two.
//!
//! **A host heard of during the call is dialled then** ([`Learned`],
//! [`Hosts`]), by the same rules as one known at join: its door, its price,
//! the spare. Two hosts that have not dialled each other make two rooms when
//! the relay goes, each with whoever reaches it, so the rule above has to
//! hold whichever of them joined first. A session is added at the end of the
//! list but ranked by id ([`ranked`]), so our media still goes where
//! everyone else's does. It is hung up when the host's presence says it no
//! longer hosts, or names another door; a host that left is let go only once
//! its session has ended, never on the word alone (see [`Hosts`]).
//!
//! **Known limit: a partition.** A guest who can reach only a host, while that
//! host cannot reach a relay everyone else is on, loses whoever offers on the
//! relay: their media goes one way, and nothing carries it across. Accepted
//! for now, because the common failure is the relay itself going down, which
//! this handles; telling a bridged host from a cut-off one needs more than
//! what a session can see, and belongs with promotion policy.
//!
//! **The call can be moved to a host, as an experiment** (`super::super::moves`
//! says how the room agrees where). Following a move does two things here.
//! The chosen host goes first in the order our media is offered in, so our
//! offer moves there the way it moves anywhere, and falls back to the usual
//! order when we cannot reach it, the host bridging us in from the relay. And
//! our link to that host is dialled again, free instead of [`HOST_COST`],
//! because moving offers alone would not move the media: the host bridges
//! everything to the relay, and the relay's routes would still win at every
//! viewer, so the media would go through the host and then the relay anyway.
//! A free modern link beats a moq-lite-05 relay's (`routes.rs` pins it). A
//! link's price is fixed for the life of its session, hence the redial; the
//! old session is kept a moment, as an old offer is. **Known limit: links
//! between hosts are not re-priced**, only links from the members who dial
//! the chosen host, so a second host still reaches the room through the relay
//! while it is up.
//!
//! **The estimate follows the session carrying our media**, as do the
//! connection figures in the Network tab and in the report we send: that link
//! is what our encoding has to fit. A host with no outgoing session left falls
//! back to its guests' links, which are then what carries its media.
//!
//! **A spare offer, behind its own switch** (`PeerStats::spare_route`, on by
//! default). With one offer, a session that dies leaves viewers with no route
//! to us at all: they see a retraction, and the library holds the next
//! announcement of us for two seconds when it ranks worse than the one lost,
//! so our video is gone for that long (the attendance loop's grace outlasts
//! it, so the tile stays). So we also offer on a second session, the first
//! other live one in [`pick`]'s order ([`spare`]), at a price
//! ([`SPARE_COST`]) that keeps it unused while the carrier works. Viewers
//! then hold a route to us that outlives the carrier: the path is bounced
//! through it at once, with no hold, and read again from the newest group. When the carrier
//! goes or the call moves, the spare is promoted by **re-pricing it in
//! place**, not by withdrawing one offer and making another, and the next
//! live session becomes the spare. An offer is withdrawn only when its
//! session leaves the pair, and only through its own gate, so the stale-route
//! rule above touches nothing but that offer; a re-price is no withdrawal and
//! marks nothing stale.
//!
//! How a spare is priced, worked from the ranking (cost first, then length):
//! a route's cost is what it was offered at plus each link it crosses,
//! [`HOST_COST`] into and out of a host ([`CHOSEN_COST`] into the one the call
//! was moved to), one per link through a relay that speaks moq-lite-06, and
//! nothing at all through one that speaks moq-lite-05, whose routes arrive as
//! `(0, unknown)` and win on the first figure against any priced route.
//!
//! - **Carrier the relay, spare a host.** At the host our spare costs
//!   `SPARE_COST + 100`; its route through the relay costs 2, or nothing on
//!   moq-lite-05, so the host reads us through the relay, and forwards only
//!   that best route to its guests. It also passes the spare up to the relay,
//!   where it is one hop longer and dearer than our carrier: never pulled.
//! - **Carrier a host, spare another host** (the relay down). The second host
//!   has us through the first at 200 and the spare at `SPARE_COST + 100`.
//! - **Carrier a host the call was moved to, spare the relay.** Only on a
//!   relay that carries a price: the relay then has us through the host for
//!   about 1 and the spare at `SPARE_COST + 1`. A moq-lite-05 relay cannot
//!   carry the price, so it holds no spare: everyone who follows the move
//!   would still prefer the free host link, but anyone who does not, or who
//!   reaches only the relay, would pull the spare straight from us, a second
//!   upload of our media where the host would otherwise have bridged it. The
//!   spare goes to the next host instead, if there is one.
//!
//! So [`SPARE_COST`] has to exceed the dearest real route to us at any node
//! holding a spare, a few hundred through two hosts. The socket tests below
//! measure the spare session's upload with everything healthy, on both kinds
//! of relay.
//!
//! **What a spare does not change**: a relay that stops answering but whose
//! session has not timed out still holds the best route to us at every
//! viewer, so the freeze until the timeout stays. What it changes is what
//! follows: when that session ends, every reader is bounced to the spare at
//! once instead of waiting out the library's two-second hold.
//!
//! **So a relay that goes quiet is left sooner** (`super::super::quiet` has
//! the rule and why it is relative): each pass samples what every session has
//! received, and a relay session that has heard nothing for its limit while
//! another of ours (or, when we host, a guest's session) has been heard is
//! given up ([`Member::give_up`]). That hangs it up from our side, so its
//! routes go here as a lost session's do, marking nothing stale, and every
//! read through it is bounced to whatever route is left (a socket test pins
//! it); the same pass promotes the spare. It then rests and is dialled again
//! like any session that ended, and our media waits a hold before moving back
//! to it that grows each time it is left again soon after
//! (`quiet::Hold`). Everyone acts by the same rule on their own session:
//! members, and the host a second later, so that its members' media has
//! moved to it before it lets go of the relay.
//!
//! **A relay that says it is going away** (a GOAWAY, which relay 0.17 sends
//! on SIGINT or SIGTERM) is not redialled at once (`Restart::Wait`: the
//! address names the one process that is leaving), and its session becomes
//! [`State::Draining`]: still heard through, still counted as a way in, but
//! never picked to carry our media or a spare. So our media moves make before
//! break while the old session still serves, and the library itself already
//! ranks every route through a draining session last, so reads are bounced
//! to the spare before the relay's deadline.
//!
//! **A host needs no spare.** It offers its one origin on every session, so
//! viewers already hold its own media through the relay and directly, and it
//! holds each member through the relay and through the member's spare: when
//! the relay goes, its own readers and its guests' are bounced like anyone's.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_channel::mpsc::UnboundedSender;
use futures_util::StreamExt as _;

use super::super::adapt;
use super::super::door;
use super::super::quiet::{self, Quiet};
use super::super::status::{RemoteStatus, Via};
use super::super::{Metrics, PeerStats};
use super::{Abort, Keyframes, Peer, Refusals, TURNED_AWAY, is_refusal, redacted};

/// What pulling through a host costs, against one per link through a relay.
pub(super) const HOST_COST: u64 = 100;

/// How long a better session must stay up before our media moves back to it.
pub(super) const HOLD: Duration = Duration::from_secs(2);

/// How long an ended session waits before it is dialled again, while the
/// call goes on elsewhere. Said in the log as "15s"; change both together.
pub(super) const REST: Duration = Duration::from_secs(15);

/// How long an old offer outlives the new one being made: long enough for
/// the new one to be announced, after which the viewer's subscription moves
/// across rather than ending.
const MOVE_OVERLAP: Duration = Duration::from_secs(1);

/// What pulling through the host the call has been moved to costs: nothing,
/// so its routes beat the relay's.
const CHOSEN_COST: u64 = 0;

/// What a spare offer is offered at, on top of the links it crosses: far
/// above any real route to us, a few hundred at most through two hosts, so
/// nobody pulls through it while anything else reaches us, and far below the
/// library's ceiling, which marks a route that is going away.
pub(super) const SPARE_COST: u64 = 10_000;

/// How often the set looks at its timers and at whether we still host.
const TICK: Duration = Duration::from_millis(250);

/// Where one session goes.
#[derive(Clone, PartialEq)]
pub(super) struct Target {
    /// `None` for the room's relay, else the host's endpoint id.
    pub(super) host: Option<String>,
    /// What to dial. A host's carries its door: never logged.
    pub(super) url: url::Url,
}

/// By hand: a host's URL carries its door.
impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target").field("host", &self.host).finish_non_exhaustive()
    }
}

/// The sessions a join holds, in the order our media prefers them: the
/// relay, then each host by endpoint id. Leaves out ourselves, a host that is
/// the relay we were given, and a host whose door is not one. A host (`hosting`)
/// dials only hosts below its own id: the one above dials it.
pub(super) fn targets(relay: &url::Url, hosts: &[door::Known], me: &str, hosting: bool) -> Vec<Target> {
    let mut ids: Vec<&door::Known> = hosts.iter().filter(|host| dials(&host.id, relay, me, hosting)).collect();
    ids.sort_by(|a, b| a.id.cmp(&b.id));
    ids.dedup_by(|a, b| a.id == b.id);
    let mut targets = vec![Target {
        host: None,
        url: relay.clone(),
    }];
    targets.extend(ids.into_iter().filter_map(|host| target(relay, host)).take(HOSTS_DIALLED));
    targets
}

/// Whether a join dials the host `id`: never ourselves or the relay we were
/// given, and from a host only a host below it, so two hosts hold one
/// session between them, whichever of them heard of the other first.
pub(super) fn dials(id: &str, relay: &url::Url, me: &str, hosting: bool) -> bool {
    id != me && Some(id) != relay.host_str() && (!hosting || id < me)
}

/// Where a host is dialled: its endpoint, at the relay room's path, with its
/// door. `None` for a door that is not one.
pub(super) fn target(relay: &url::Url, host: &door::Known) -> Option<Target> {
    if !door::is_door(&host.door) {
        return None;
    }
    let mut url = url::Url::parse(&format!("iroh://{}", host.id)).ok()?;
    url.set_path(relay.path());
    url.query_pairs_mut().append_pair(door::PARAM, &host.door);
    Some(Target {
        host: Some(host.id.clone()),
        url,
    })
}

/// The most hosts a join holds sessions to: as many as a room keeps.
pub(super) const HOSTS_DIALLED: usize = crate::recent::HOSTS_KEPT;

/// What the room's presence tells a join about a host as the call goes on.
#[derive(Debug)]
pub(super) enum Learned {
    /// It hosts the room, dialled at this target (which carries its door).
    Host(Target),
    /// Its presence says it no longer hosts the room.
    Stopped(String),
    /// It left the room: its presence went for longer than a bounce's grace,
    /// or it said goodbye.
    Left(String),
}

/// The hosts a join wants a session to: those known at join and those heard
/// since, less any that stopped hosting.
///
/// **The lowest ids are dialled, not the latest heard**, past
/// [`HOSTS_DIALLED`] ([`kept`]): everyone who hears the same hosts then holds
/// the same ones whatever order they heard them in, and our media goes to the
/// lowest one when the relay is down, so everyone meets there.
///
/// **A host that left is kept until its session ends.** Its leave is read
/// off its presence going, which is also what our own link failing looks
/// like when that session was the only way to it; cutting a session that
/// still works, or one the stack is still redialling, on that word could end
/// a call that would have carried on. A host whose session gave up after it
/// left is let go, and found again if its presence is heard again.
///
/// **A host's door is acted on at most once per [`DOOR_SETTLE`]**, the
/// latest heard winning: each change of door can mean a dial, ours and
/// everyone's, and a door is whatever someone's presence says.
#[derive(Debug, Default)]
pub(super) struct Hosts {
    wanted: std::collections::BTreeMap<String, Target>,
    left: std::collections::BTreeSet<String>,
    /// A door heard while the last one acted on was still settling.
    pending: std::collections::BTreeMap<String, Target>,
    /// When each host's door was last acted on.
    changed: std::collections::BTreeMap<String, Instant>,
}

/// How long a host's door is held before another change of it is acted on.
pub(super) const DOOR_SETTLE: Duration = Duration::from_secs(5);

impl Hosts {
    pub(super) fn new(known: impl IntoIterator<Item = Target>) -> Self {
        let mut hosts = Self::default();
        for target in known {
            if let Some(id) = target.host.clone() {
                hosts.wanted.insert(id, target);
            }
        }
        hosts
    }

    /// Takes in what presence said `now`. Whether it changed what is wanted;
    /// a door changed too soon after the last waits for [`Self::settle`].
    pub(super) fn learn(&mut self, learned: Learned, now: Instant) -> bool {
        match learned {
            Learned::Host(target) => {
                let Some(id) = target.host.clone() else { return false };
                let back = self.left.remove(&id);
                self.pending.remove(&id);
                match self.wanted.get(&id) {
                    Some(had) if *had == target => back,
                    Some(_) if self.changed.get(&id).is_some_and(|at| now.duration_since(*at) < DOOR_SETTLE) => {
                        self.pending.insert(id, target);
                        back
                    }
                    _ => {
                        self.changed.insert(id.clone(), now);
                        self.wanted.insert(id, target);
                        true
                    }
                }
            }
            Learned::Stopped(id) => {
                self.left.remove(&id);
                self.pending.remove(&id);
                self.changed.remove(&id);
                self.wanted.remove(&id).is_some()
            }
            Learned::Left(id) => self.wanted.contains_key(&id) && self.left.insert(id),
        }
    }

    /// Acts on each door that waited out [`DOOR_SETTLE`]. Whether any did.
    pub(super) fn settle(&mut self, now: Instant) -> bool {
        let due: Vec<String> = self
            .pending
            .keys()
            .filter(|id| self.changed.get(*id).is_none_or(|at| now.duration_since(*at) >= DOOR_SETTLE))
            .cloned()
            .collect();
        for id in &due {
            if let Some(target) = self.pending.remove(id) {
                self.changed.insert(id.clone(), now);
                self.wanted.insert(id.clone(), target);
            }
        }
        !due.is_empty()
    }

    /// Whether `id` has left the room since it was last heard hosting.
    pub(super) fn has_left(&self, id: &str) -> bool {
        self.left.contains(id)
    }

    /// Lets a host that left go, once its session has ended.
    pub(super) fn let_go(&mut self, id: &str) {
        if self.left.remove(id) {
            self.wanted.remove(id);
            self.pending.remove(id);
            self.changed.remove(id);
        }
    }

    /// Every host wanted, by id; [`kept`] says which to hold.
    pub(super) fn want(&self) -> Vec<&Target> {
        self.wanted.values().collect()
    }
}

/// Which of the `wanted` hosts, sorted by id, to hold a session to: every one
/// whose session already works (`working`), then the lowest of the rest, up
/// to [`HOSTS_DIALLED`] in all.
///
/// **A session that works is never given up for a lower id.** A door is
/// whatever someone's presence says, so eight made-up hosts with low ids
/// would otherwise push out every real one and split the room again when the
/// relay goes. The price: past eight hosts, members who heard them in
/// different orders can hold slightly different sets, and some media may
/// cross between hosts. That is the safer side: a host that bridges is
/// slower, a host nobody holds a session to is a split.
pub(super) fn kept(wanted: &[&str], working: &[&str]) -> Vec<String> {
    let mut kept: Vec<String> = wanted.iter().filter(|id| working.contains(id)).map(|id| id.to_string()).collect();
    for id in wanted {
        if kept.len() >= HOSTS_DIALLED {
            break;
        }
        if !kept.iter().any(|held| held == id) {
            kept.push(id.to_string());
        }
    }
    kept
}

/// The order of `members` our media prefers: the relay, then the hosts by
/// endpoint id, whatever order they were added in, with any retired last.
fn ranked(members: &[Member]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..members.len()).collect();
    order.sort_by(|&a, &b| {
        let key = |at: usize| (members[at].state == State::Retired, members[at].target.host.clone());
        key(a).cmp(&key(b))
    });
    order
}

/// Where one session is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum State {
    /// Dialled, not up yet.
    Dialing,
    /// Up since then.
    Live(Instant),
    /// Up, but its far side said it is going away: it still serves what it
    /// carries, and we still hear through it, but nothing new of ours goes
    /// onto it. A session that a GOAWAY reached refuses new requests, so not
    /// even a spare could be pulled through it.
    Draining,
    /// Dropped; the stack is redialling it.
    Redialling,
    /// Gave up; dialled again from then if the call is still going.
    Resting(Instant),
    /// Turned away; not dialled again this call.
    Refused,
    /// A host no longer wanted: it stopped hosting, its door changed, or it
    /// left and its session ended. Never dialled again; a host heard again
    /// gets a session of its own.
    Retired,
}

impl State {
    /// Up and taking new offers: where our media or a spare may go.
    fn live(self) -> bool {
        matches!(self, State::Live(_))
    }

    /// Up at all, draining included: what we hear through, and what keeps
    /// the call going.
    fn up(self) -> bool {
        matches!(self, State::Live(_) | State::Draining)
    }

    /// Still trying, so the call is not over while this is.
    fn trying(self) -> bool {
        matches!(self, State::Dialing | State::Redialling)
    }
}

/// Which session carries our media: `current` while it is live, unless a
/// better one has held for [`HOLD`]; otherwise the best live one at once.
/// `None` when none is live.
///
/// **The relay gets [`HOLD`] from the start of the join to answer first.**
/// Without it the first session up wins, which on a quick link to a host is
/// the host, and our media would go through it for two seconds before moving
/// back: a move at the start of every join, for nothing.
///
/// `preferred`, the host the call has been moved to, goes first in the order
/// and the rest keep theirs; when it is not live the order is the usual one.
///
/// `holds` is each session's own hold before our media moves up to it, where
/// one was left as quiet and has been made to wait longer ([`quiet::Hold`]);
/// [`HOLD`] for any it does not name. A draining session is never picked: it
/// is not live.
pub(super) fn pick(
    states: &[State],
    current: Option<usize>,
    now: Instant,
    started: Instant,
    preferred: Option<usize>,
    holds: &[Duration],
) -> Option<usize> {
    if states.first() == Some(&State::Dialing) && now.duration_since(started) < HOLD {
        return None;
    }
    let order = order(states.len(), preferred);
    if let Some(current) = current.filter(|&at| states.get(at).is_some_and(|state| state.live())) {
        let rank = order.iter().position(|&at| at == current).unwrap_or(order.len());
        let held = order[..rank]
            .iter()
            .copied()
            .find(|&at| {
                let hold = holds.get(at).copied().unwrap_or(HOLD);
                matches!(states[at], State::Live(since) if now.duration_since(since) >= hold)
            });
        return Some(held.unwrap_or(current));
    }
    order.into_iter().find(|&at| states[at].live())
}

/// The order our media prefers the sessions in: `preferred` first, when it
/// is one of them, and the rest as listed.
fn order(len: usize, preferred: Option<usize>) -> Vec<usize> {
    let preferred = preferred.filter(|&at| at < len);
    preferred.into_iter().chain((0..len).filter(|&at| Some(at) != preferred)).collect()
}

/// Which session keeps the spare offer beside `carrier`: the first other live
/// one in [`pick`]'s order, so it is where our media would go next, among
/// those that carry a route's price (`priced`). One that does not would hand
/// the spare on free, and whoever reached us only that way would pull it.
/// No hold: a spare carries nothing until it is needed. Never a draining
/// session, even with nothing else: its far side refuses new requests, so a
/// spare there could never be pulled, and it is about to close anyway.
pub(super) fn spare(states: &[State], priced: &[bool], carrier: usize, preferred: Option<usize>) -> Option<usize> {
    order(states.len(), preferred)
        .into_iter()
        .filter(|&at| at != carrier)
        .find(|&at| states[at].live() && priced.get(at).copied().unwrap_or(false))
}

/// Whether a session at `version` carries a route's price. moq-lite-06 added
/// it; older versions, and the IETF drafts we never speak to each other, do
/// not. Read from the name, so a later lite version counts as carrying it.
pub(super) fn carries_price(version: moq_net::Version) -> bool {
    let name = version.as_str();
    name.starts_with("moq-lite-") && !matches!(name, "moq-lite-01" | "moq-lite-02" | "moq-lite-03" | "moq-lite-04" | "moq-lite-05")
}

/// Where we say our media goes (`via` in presence), given the host the
/// session carrying it reaches (`None` for the relay).
///
/// **A host the call has moved to says its guests**, though it still offers
/// on the relay: everyone who follows the move reads it directly, so saying
/// "relay" drew it on the room's map as sending to a relay nobody pulled
/// from. Anyone who does not follow the move and does read it through the
/// relay says so in their own report of routes, which the map draws as it
/// is. The session to the relay stays in what we say we hold.
pub(super) fn says(carrier_host: Option<&str>, serving: bool, call_host: Option<&str>, me: &str) -> super::super::arrival::Sending {
    use super::super::arrival::Sending;
    match carrier_host {
        _ if serving && call_host == Some(me) => Sending::Guests,
        None => Sending::Relay,
        Some(id) => Sending::Host(id.to_string()),
    }
}

/// Whether a join keeps a spare offer at all: only with the switch on, and
/// never while hosting, whose one origin is offered whole on every session.
pub(super) fn keeps_spare(hosting: bool, switch: bool) -> bool {
    switch && !hosting
}

/// What the status line says about where we are.
pub(super) fn via(states: &[State], hosting: bool) -> Via {
    Via {
        relay: states.first().is_some_and(|state| state.up()),
        hosts: states.iter().skip(1).filter(|state| state.up()).count(),
        hosting,
    }
}

/// How a join over several sessions ends: `None` while it goes on. It goes
/// on while anything is up, anything is still being tried, or we host. At the
/// end the reason is the relay's, or "turned away" if every one refused us.
pub(super) fn ended(states: &[State], hosting: bool, errors: &[Option<String>]) -> Option<String> {
    if hosting || states.iter().any(|state| state.up() || state.trying()) {
        return None;
    }
    if states.iter().filter(|state| **state != State::Retired).all(|state| *state == State::Refused) {
        return Some(TURNED_AWAY.to_string());
    }
    Some(
        errors
            .iter()
            .flatten()
            .next()
            .cloned()
            .unwrap_or_else(|| "every way into the room closed".to_string()),
    )
}

/// What one session offers of ours: an origin of its own, whose offers name
/// our publishing identity first, so every route to us still names one
/// publisher.
///
/// **Each gate is a hop of its own, after ours**: a route to us reads
/// `[us, this gate, …]`, not `[us, …]`. Withdrawing an offer from a session
/// that stays up is an explicit unannounce, and the library takes one as the
/// sender giving the path up: at the far side, every route whose chain names
/// the sender goes stale until the sender announces it again. Were the sender
/// us, that would be every route to us, the relay's included, and a host we
/// moved off would lose us for good the moment our offer left it, though we
/// were on the relay it also holds. With a gate of its own as the sender, only
/// the routes through that gate go.
///
/// The price: a session's split horizon hides only what came through its own
/// gate, so our broadcasts now reach our inbound back through whoever bridges
/// them. Only their announcements: `attend` passes over our own paths before
/// asking for anything, and nothing reads our inbound into a gate.
pub(super) struct Gate {
    pub(super) origin: moq_net::origin::Producer,
    /// The first hop of every offer: our publishing identity.
    publisher: moq_net::Hops,
}

impl Gate {
    pub(super) fn new(outbound: &moq_net::origin::Producer) -> Self {
        // A chain of one hop cannot be too long or repeat itself.
        let publisher = moq_net::Hops::try_from(vec![outbound.hop()]).unwrap_or_default();
        Self {
            origin: moq_tokio::origin::spawn(),
            publisher,
        }
    }

    /// Offers everything we publish under `me/` through this gate at `price`,
    /// as it is announced, until the returned offer is dropped: our camera,
    /// our control broadcast, and a shared screen when there is one.
    pub(super) fn offer(&self, outbound: &moq_net::origin::Producer, me: &str, price: u64) -> Offer {
        let gate = self.origin.clone();
        let route = moq_net::origin::Route::default().with_hops(self.publisher.clone());
        let source = outbound.consume();
        let ours = format!("{me}/");
        let (pricing, mut priced) = tokio::sync::watch::channel(price);
        let serving = Abort(tokio::spawn(async move {
            let mut announced = source.announced();
            // Each path's offer, and what serves it. Dropping both withdraws
            // the offer; the route itself is shared so it can be re-priced.
            let mut offered: std::collections::HashMap<String, (Arc<moq_net::origin::Dynamic>, Abort)> =
                std::collections::HashMap::new();
            loop {
                tokio::select! {
                    update = announced.next() => {
                        let Some(update) = update else { return };
                        let path = update.prefix.as_str().to_string();
                        if !path.starts_with(&ours) {
                            continue;
                        }
                        match update.kind {
                            moq_net::announce::Kind::Announced => {
                                let at = route.clone().with_cost(*priced.borrow());
                                let dynamic = match gate.dynamic(path.as_str(), at) {
                                    Ok(dynamic) => Arc::new(dynamic),
                                    Err(error) => {
                                        tracing::warn!("could not offer {path}: {error:#}");
                                        continue;
                                    }
                                };
                                let handler = dynamic.clone();
                                let source = source.clone();
                                let served = path.clone();
                                let serving = Abort(tokio::spawn(async move {
                                    while let Ok(request) = handler.requested_broadcast().await {
                                        match source.request_broadcast(served.as_str()).await {
                                            Ok(broadcast) => request.accept(broadcast),
                                            Err(error) => request.reject(error),
                                        }
                                    }
                                }));
                                offered.insert(path, (dynamic, serving));
                            }
                            moq_net::announce::Kind::Retracted => {
                                offered.remove(&path);
                            }
                            moq_net::announce::Kind::Updated => {}
                        }
                    }
                    changed = priced.changed() => {
                        // The offer was dropped: so is this, and every route
                        // with it.
                        if changed.is_err() {
                            return;
                        }
                        // In place: the same routes, from the same hops, at
                        // another price. Viewers see a change of route, never
                        // a retraction, and nothing goes stale.
                        let at = route.clone().with_cost(*priced.borrow_and_update());
                        for (path, (dynamic, _)) in &offered {
                            if let Err(error) = dynamic.update(at.clone()) {
                                tracing::warn!("could not re-price {path}: {error:#}");
                            }
                        }
                    }
                }
            }
        }));
        Offer {
            _serving: serving,
            pricing,
        }
    }
}

/// Our broadcasts offered through one gate, at a price that can change in
/// place. Dropping it withdraws them.
pub(super) struct Offer {
    _serving: Abort,
    pricing: tokio::sync::watch::Sender<u64>,
}

impl Offer {
    /// Offers everything at `price` from now on, without withdrawing it.
    fn reprice(&self, price: u64) {
        self.pricing.send_replace(price);
    }
}

/// What one offer of ours is for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Role {
    /// It carries our media.
    Carrier,
    /// It waits, priced out of use, for the carrier to go.
    Spare,
}

impl Role {
    fn price(self) -> u64 {
        match self {
            Role::Carrier => 0,
            Role::Spare => SPARE_COST,
        }
    }
}

/// What one session reports to the set.
#[derive(Debug)]
enum Event {
    /// Up, and whether it carries a route's price ([`carries_price`]).
    Live { priced: bool },
    /// Its far side said it is going away, and it serves on until then.
    Draining,
    Down,
    Ended(String),
    Refused(String),
}

/// Everything [`join`] needs, named rather than positional.
pub(super) struct Join {
    /// The client every session is dialled with: over iroh in the app, a
    /// plain local socket in the tests.
    pub(super) client: moq_tokio::Client,
    pub(super) me: String,
    pub(super) targets: Vec<Target>,
    /// Where our own broadcasts are made.
    pub(super) outbound: moq_net::origin::Producer,
    /// Where everything arrives; the outbound itself when we host.
    pub(super) inbound: moq_net::origin::Producer,
    /// Whether this join started with hosting on: one shared origin, offered
    /// whole on every session. Whether we still serve is read off `local`.
    pub(super) hosting: bool,
    /// The call's estimate, which the session carrying our media feeds.
    pub(super) estimate: moq_net::bandwidth::Producer,
    /// Our guests' slowest link, for when we host and nothing else carries.
    pub(super) guests: Option<moq_net::bandwidth::Consumer>,
    /// When our guests' sessions were last heard, when we host: evidence
    /// that our own link works while a session of ours has gone quiet.
    pub(super) heard: Option<Arc<quiet::Heard>>,
    pub(super) metrics: Arc<Metrics>,
    pub(super) local: Arc<PeerStats>,
    pub(super) status: UnboundedSender<RemoteStatus>,
    pub(super) keyframe: Arc<Keyframes>,
    /// The room as the status line names it.
    pub(super) room: String,
    /// Move the call round the relay and every host this often
    /// (`--bounce`), to test moving it.
    pub(super) bounce: Option<Duration>,
    /// What presence says about the room's hosts from now on.
    pub(super) learned: futures_channel::mpsc::UnboundedReceiver<Learned>,
}

/// One session of the set.
struct Member {
    target: Target,
    gate: Option<Gate>,
    state: State,
    error: Option<String>,
    /// Ever up, so a return reads as one.
    joined: bool,
    /// Dialled again at a new price while up, so its answer is not a return.
    redialled: bool,
    session: Option<Session>,
    /// Whether its session, when last up, carried a route's price.
    priced: bool,
    /// What we offer on it, and what for. Only with a gate.
    offer: Option<(Offer, Role)>,
    /// How long it must hold before our media moves up to it again, longer
    /// each time it is left as quiet.
    hold: quiet::Hold,
    /// How long it may hear nothing before it counts as quiet.
    limit: Duration,
    /// Which dialling of this slot its session's events come from, so one
    /// from a session it no longer holds is not taken for its own.
    generation: u64,
    /// When it was retired, after which its slot may go to another host
    /// once what it kept for the overlap has gone.
    retired: Option<Instant>,
    /// Whether the log has said its host has a new door, while its session
    /// keeps the old one.
    door_noted: bool,
    /// Whether its session was dialled free, to the host the call is on.
    free: bool,
}

impl Member {
    /// A session to `target`, not dialled yet.
    fn new(target: Target, hosting: bool, outbound: &moq_net::origin::Producer) -> Self {
        Member {
            limit: quiet::limit(&target.url, hosting),
            gate: (!hosting).then(|| Gate::new(outbound)),
            state: State::Dialing,
            error: None,
            joined: false,
            redialled: false,
            session: None,
            priced: false,
            offer: None,
            hold: quiet::Hold::new(),
            generation: 0,
            retired: None,
            door_noted: false,
            free: false,
            target,
        }
    }

    /// Gives this session up: hangs it up, so its routes go here and at the
    /// far side as a lost session's do (marking nothing stale), and rests it
    /// before it is dialled again. The next pass moves our media to the spare
    /// by re-pricing it. What an ending session does, and what anything that
    /// judged a session dead before its transport did would call.
    fn give_up(&mut self, error: String) {
        // Aborted rather than only dropped, so the close goes out at once
        // with the session's own routes, whatever else still holds a handle.
        if let Some(session) = self.session.take() {
            session.connection.abort(moq_net::Error::Cancel);
        }
        self.error = Some(error);
        self.state = State::Resting(Instant::now() + REST);
    }

    /// Retires a host session for good. Its session, offer and gate are kept
    /// a moment, as an old offer is, so that if it carried our media the next
    /// pass moves it before the old one goes; then hung up as a lost session
    /// is, so its routes go without marking anything stale, and everything it
    /// ran is dropped. The slot keeps nothing live.
    fn retire(&mut self, now: Instant) {
        self.state = State::Retired;
        self.retired = Some(now);
        let session = self.session.take();
        let offer = self.offer.take();
        let gate = self.gate.take();
        tokio::spawn(async move {
            tokio::time::sleep(MOVE_OVERLAP).await;
            if let Some(session) = session {
                session.connection.abort(moq_net::Error::Cancel);
            }
            drop(offer);
            drop(gate);
        });
    }

    /// Whether this slot may be given to another host: retired, and past
    /// the overlap its old session and offer were kept for.
    fn free(&self, now: Instant) -> bool {
        self.retired.is_some_and(|at| now.duration_since(at) >= MOVE_OVERLAP)
    }
}

/// Holds a session to every host `hosts` wants and to no other: retires the
/// rest, and dials each one wanted that has none, in a retired slot when one
/// is free so the list stays as long as the most hosts held at once. Whether
/// any session was added or retired, so our gates are said again.
fn reconcile(
    members: &mut Vec<Member>,
    hosts: &mut Hosts,
    now: Instant,
    new: &dyn Fn(Target) -> Member,
    quiet: &mut Quiet,
    call_host: Option<&str>,
    dial: &mut dyn FnMut(usize, &Member, bool) -> Session,
) -> bool {
    hosts.settle(now);
    for id in members
        .iter()
        .filter(|member| matches!(member.state, State::Resting(_) | State::Refused))
        .filter_map(|member| member.target.host.clone())
        .filter(|id| hosts.has_left(id))
        .collect::<Vec<_>>()
    {
        tracing::info!("host {id} left the room and its session ended; not dialling it again");
        hosts.let_go(&id);
    }
    let wanted: Vec<Target> = hosts.want().into_iter().cloned().collect();
    let ids: Vec<&str> = wanted.iter().filter_map(|target| target.host.as_deref()).collect();
    let working: Vec<String> = members
        .iter()
        .filter(|member| member.state != State::Retired && member.state.up())
        .filter_map(|member| member.target.host.clone())
        .collect();
    let working: Vec<&str> = working.iter().map(String::as_str).collect();
    let kept = kept(&ids, &working);
    let want: Vec<Target> = wanted.iter().filter(|target| target.host.as_ref().is_some_and(|id| kept.contains(id))).cloned().collect();
    let mut changed = false;
    for (index, member) in members.iter_mut().enumerate() {
        if member.state == State::Retired {
            continue;
        }
        let Some(id) = member.target.host.clone() else { continue };
        match want.iter().find(|target| target.host.as_deref() == Some(id.as_str())) {
            None => {
                if ids.contains(&id.as_str()) {
                    tracing::info!("letting {} go: more hosts than we hold, and its session is not up", spoken(member));
                } else {
                    tracing::info!("hanging up {}: it no longer hosts the room", spoken(member));
                }
                member.retire(now);
                changed = true;
            }
            Some(target) if *target == member.target => {}
            // **A new door does not hang up a session that works**: the host
            // let us in with the old one, and that session stays good. The
            // new one is for the next dial, which is when this session is
            // next not up.
            Some(_) if member.state.up() => {
                if !std::mem::replace(&mut member.door_noted, true) {
                    tracing::info!("{} has a new door; keeping the session it let us in with until it next drops", spoken(member));
                }
            }
            Some(target) => {
                tracing::info!("dialling {} again, at its new door", spoken(member));
                if let Some(old) = member.session.take() {
                    old.connection.abort(moq_net::Error::Cancel);
                }
                member.target = target.clone();
                member.generation += 1;
                member.state = State::Dialing;
                member.error = None;
                member.redialled = false;
                member.door_noted = false;
                member.free = call_host == Some(id.as_str());
                member.session = Some(dial(index, member, member.free));
            }
        }
    }
    for target in want {
        if members
            .iter()
            .any(|member| member.state != State::Retired && member.target.host == target.host)
        {
            continue;
        }
        let mut member = new(target);
        // At its price from the start when the call is already on it, rather
        // than dialled at the usual price and again, free, a pass later.
        member.free = call_host.is_some() && member.target.host.as_deref() == call_host;
        let index = match members.iter().position(|slot| slot.free(now)) {
            Some(at) => {
                member.generation = members[at].generation + 1;
                at
            }
            None => members.len(),
        };
        tracing::info!("dialling {}, heard of during the call", spoken(&member));
        member.session = Some(dial(index, &member, member.free));
        // A slot new to this host: its history, or our guests' when it is
        // one past the end, is not this session's.
        quiet.forget(index);
        if index == members.len() {
            members.push(member);
        } else {
            members[index] = member;
        }
        changed = true;
    }
    changed
}

/// How the log names a session: the relay by its address, a host by its id.
fn named(member: &Member, relay: &str) -> String {
    match &member.target.host {
        None => relay.to_string(),
        Some(id) => format!("host {id}"),
    }
}

/// How a sentence about a session names it: "the relay", or a host by its id.
fn spoken(member: &Member) -> String {
    match &member.target.host {
        None => "the relay".to_string(),
        Some(id) => format!("host {id}"),
    }
}

/// A dialled session: its status task (which owns the connection, so
/// dropping this hangs up), and what the set follows when it carries us.
struct Session {
    _running: Abort,
    /// A handle on the connection the status task owns, to hang it up.
    connection: moq_tokio::Connection,
    bandwidth: moq_net::bandwidth::Consumer,
    monitor: moq_tokio::connection::Monitor,
}

/// Feeds the call's estimate and connection figures from one source.
struct Following {
    _estimate: Abort,
    _stats: Option<Abort>,
}

/// Dials every target, and returns the join as a [`Peer`] whose ending is
/// the end of the last way into the room.
pub(super) fn join(args: Join) -> Peer {
    let Join {
        client,
        me,
        targets,
        outbound,
        inbound,
        hosting,
        estimate,
        guests,
        heard: guests_heard,
        metrics,
        local,
        status,
        keyframe,
        room,
        bounce,
        learned,
    } = args;
    let mut learned = Some(learned);
    let relay_url = targets[0].url.clone();
    let mut hosts_wanted = Hosts::new(targets.iter().cloned());
    let (events, mut heard) = futures_channel::mpsc::unbounded::<(usize, u64, Event)>();
    let dial = {
        let inbound = inbound.clone();
        let outbound = outbound.clone();
        move |index: usize, member: &Member, chosen: bool| -> Session {
            // **Wait out a restart rather than redial into it.** A relay that
            // says it is going away without naming anywhere else is the one
            // process at that address shutting down: dialling it again at
            // once only reaches the process that is leaving. Our media moves
            // to another session meanwhile (see `Event::Draining`), and the
            // relay is dialled again as after any drop once it has closed.
            let mut client = client.clone().with_restart(moq_tokio::Restart::Wait);
            if member.target.host.is_some() {
                client = client.with_cost(if chosen { CHOSEN_COST } else { HOST_COST });
            }
            let client = match &member.gate {
                Some(gate) => client.with_publisher(&gate.origin),
                None => client.with_publisher(&outbound),
            };
            let mut connection = client.with_subscriber(inbound.clone()).connect(member.target.url.clone());
            let handle = connection.clone();
            let bandwidth = connection.send_bandwidth();
            let monitor = connection.monitor();
            let mut refusals = Refusals::for_url(&member.target.url);
            let events = events.clone();
            let generation = member.generation;
            let running = Abort(tokio::spawn(async move {
                let say = |event| {
                    let _ = events.unbounded_send((index, generation, event));
                };
                // The live session's going-away notice. Under `Restart::Wait`
                // one naming no other address changes no status, so it is
                // watched here; one that does is `Migrating`.
                let mut going: Option<moq_net::goaway::Consumer> = None;
                loop {
                    let watched = going.clone();
                    let goaway = async move {
                        match watched {
                            Some(going) => going.recv().await,
                            None => std::future::pending().await,
                        }
                    };
                    let status = tokio::select! {
                        status = connection.status() => status,
                        notice = goaway => {
                            // Said once per session; a session that closed
                            // without one has nothing more to say either.
                            going = None;
                            if notice.is_some() {
                                say(Event::Draining);
                            }
                            continue;
                        }
                    };
                    match status {
                        Ok(moq_tokio::Status::Connected) => {
                            if let Some(refusals) = refusals.as_mut() {
                                refusals.connected(Instant::now());
                            }
                            let priced = connection.version().is_some_and(carries_price);
                            going = connection.draining();
                            say(Event::Live { priced });
                        }
                        Ok(moq_tokio::Status::Migrating) => {
                            going = None;
                            say(Event::Draining);
                        }
                        Ok(moq_tokio::Status::Disconnected) => {
                            going = None;
                            if refusals.as_mut().is_some_and(|refusals| refusals.dropped(Instant::now())) {
                                say(Event::Refused(TURNED_AWAY.to_string()));
                                return;
                            }
                            say(Event::Down);
                        }
                        Ok(_) => {}
                        Err(error) => {
                            let error = format!("{error:#}");
                            say(if is_refusal(&error) { Event::Refused(error) } else { Event::Ended(error) });
                            return;
                        }
                    }
                }
            }));
            Session {
                _running: running,
                connection: handle,
                bandwidth,
                monitor,
            }
        }
    };

    let mut members: Vec<Member> = targets.into_iter().map(|target| Member::new(target, hosting, &outbound)).collect();
    let hosts = members.len() - 1;
    tracing::info!("dialling the relay and {hosts} saved hosts");
    // Said in our `routes.json`, so whoever reads it can tell our gates from
    // a relay in the chains of our media: to anyone else they look alike.
    local.set_gates(gates(&members));
    // Where the call is said to be as we join, so its host is dialled at its
    // price from the start rather than dialled twice.
    let mut chosen = chosen_member(&members, &local);
    for (index, member) in members.iter_mut().enumerate() {
        member.free = chosen == Some(index);
        member.session = Some(dial(index, member, member.free));
    }
    if let Some(every) = bounce {
        tracing::info!("bouncing every {}s between the relay and {hosts} hosts", every.as_secs());
    }
    let path = members[0].target.url.path().trim_matches('/').to_string();
    let relay_label = redacted(&members[0].target.url);

    Peer {
        closed: Box::pin(async move {
            let started = Instant::now();
            let mut carrier: Option<usize> = None;
            // Held, and replaced only below: dropping it stops what it feeds.
            let mut _following: Option<Following> = None;
            let mut on_guests = false;
            let mut said: Option<Via> = None;
            let mut down_said = false;
            let mut next_bounce = bounce.map(|every| started + every);
            // Each session's history of hearing anything, and our guests'
            // one past them, for leaving a session that went quiet.
            let mut quiet = Quiet::default();
            let mut ticker = tokio::time::interval(TICK);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    event = heard.next() => {
                        let Some((index, generation, event)) = event else { return "the sessions stopped".to_string() };
                        // A session let go, or one its slot no longer holds,
                        // may still have been heard from.
                        if members[index].state == State::Retired || members[index].generation != generation {
                            continue;
                        }
                        // Whether the call goes on elsewhere, so an ending here
                        // is said as a retry or as what it is.
                        let elsewhere = (hosting && local.offer().is_some())
                            || members.iter().enumerate().any(|(at, member)| at != index && member.state.up());
                        let member = &mut members[index];
                        let name = |member: &Member| named(member, &relay_label);
                        match event {
                            Event::Live { priced } if std::mem::take(&mut member.redialled) && member.state.live() => {
                                member.priced = priced;
                                tracing::info!("{} answered at its new price", name(member));
                            }
                            Event::Live { priced } => {
                                member.priced = priced;
                                quiet.forget(index);
                                if !priced && member.gate.is_some() && local.spare_route() {
                                    tracing::info!("{} carries no route price, so it holds no spare offer of ours", name(member));
                                }
                                match (&member.target.host, member.joined) {
                                    (None, false) => tracing::info!("joined {relay_label}"),
                                    (Some(id), false) => tracing::info!("joined {path} through host {id}"),
                                    (_, true) => {
                                        tracing::info!("reconnected to {}", name(member));
                                        keyframe.ask();
                                    }
                                }
                                member.joined = true;
                                member.state = State::Live(Instant::now());
                            }
                            // Acted on once, from the notice or from a
                            // migration, whichever the status task saw first.
                            Event::Draining => {
                                if member.state.live() {
                                    member.state = State::Draining;
                                    if carrier == Some(index) {
                                        tracing::info!("{} says it is going away; moving our media", spoken(member));
                                    } else {
                                        tracing::info!("{} says it is going away; nothing new of ours goes on it", spoken(member));
                                    }
                                }
                            }
                            Event::Down => {
                                tracing::warn!("the session dropped; redialling {}", name(member));
                                member.state = State::Redialling;
                            }
                            Event::Ended(error) => {
                                member.give_up(error);
                                match (&member.target.host, elsewhere) {
                                    (None, true) => tracing::warn!("the relay is unreachable; trying again in 15s"),
                                    (Some(id), true) => tracing::info!("host {id} is unreachable; trying again in 15s"),
                                    (None, false) => tracing::warn!("the relay is unreachable"),
                                    (Some(id), false) => tracing::info!("host {id} is unreachable"),
                                }
                            }
                            Event::Refused(error) => {
                                member.session = None;
                                member.error = Some(error);
                                member.state = State::Refused;
                                match &member.target.host {
                                    None => tracing::warn!("turned away by the relay; not trying it again this call"),
                                    Some(id) => tracing::warn!("turned away by host {id}; not trying it again this call"),
                                }
                            }
                        }
                        // A session coming, going or changing its price can
                        // move a route to us, here or at whoever reads us
                        // through it, so our catalogs are written again.
                        keyframe.rerouted();
                    }
                    news = async {
                        match learned.as_mut() {
                            Some(learned) => learned.next().await,
                            None => std::future::pending().await,
                        }
                    } => match news {
                        None => learned = None,
                        // The same rule as at join: a host dials only below.
                        Some(Learned::Host(target)) if !target.host.as_deref().is_some_and(|id| dials(id, &relay_url, &me, hosting)) => {}
                        Some(news) => {
                            hosts_wanted.learn(news, Instant::now());
                        }
                    },
                    _ = ticker.tick() => {}
                }

                // Hosts heard of, or let go, since the last pass: a session
                // to each one wanted and to no other.
                let call_host = local.call_host();
                let new = |target| Member::new(target, hosting, &outbound);
                if reconcile(&mut members, &mut hosts_wanted, Instant::now(), &new, &mut quiet, call_host.as_deref(), &mut |index, member, chosen| {
                    dial(index, member, chosen)
                }) {
                    local.set_gates(gates(&members));
                }
                let guests_at = members.len();

                let now = Instant::now();
                let serving = hosting && local.offer().is_some();
                let states: Vec<State> = members.iter().map(|member| member.state).collect();
                let errors: Vec<Option<String>> = members.iter().map(|member| member.error.clone()).collect();
                if let Some(error) = ended(&states, serving, &errors) {
                    return error;
                }

                // A session that gave up is dialled again while the call goes
                // on somewhere, so a relay that comes back is found.
                let going = serving || states.iter().any(|state| state.up());
                for (index, member) in members.iter_mut().enumerate() {
                    if let State::Resting(until) = member.state {
                        if going && now >= until {
                            member.state = State::Dialing;
                            member.free = chosen == Some(index);
                            member.session = Some(dial(index, member, member.free));
                        }
                    }
                }

                // A session that hears nothing while another of ours is
                // heard is left now rather than at its idle timeout. Its
                // routes go as a lost session's, the pass below moves our
                // media to the spare, and it rests and is dialled again as
                // one that ended does.
                for (index, member) in members.iter().enumerate() {
                    match (&member.session, member.state.up()) {
                        (Some(session), true) => {
                            let bytes = session.monitor.stats().and_then(|stats| stats.bytes_received);
                            // Only the relay is judged; a host is evidence.
                            quiet.observe(index, bytes, member.limit, member.target.host.is_none(), now);
                        }
                        _ => quiet.forget(index),
                    }
                }
                if let Some(guests_heard) = &guests_heard {
                    quiet.heard(guests_at, guests_heard.last(), quiet::QUIET_IROH);
                }
                if let Some(verdict) = quiet.quiet(now) {
                    let evidence = match members.get(verdict.heard) {
                        Some(other) => format!("{} is heard", spoken(other)),
                        None => "our guests are heard".to_string(),
                    };
                    let member = &mut members[verdict.quiet];
                    member.hold.left(now);
                    tracing::info!(
                        "{} went quiet for {:.1}s while {evidence}; leaving it, trying it again in 15s, and holding our media off it for {}s once it is back",
                        spoken(member),
                        verdict.silent.as_secs_f64(),
                        member.hold.hold().as_secs(),
                    );
                    member.give_up(format!("{} went quiet", spoken(member)));
                    quiet.forget(verdict.quiet);
                }
                for member in members.iter_mut() {
                    if let State::Live(since) = member.state {
                        member.hold.up(since, now);
                    }
                }

                let states: Vec<State> = members.iter().map(|member| member.state).collect();
                let holds: Vec<Duration> = members.iter().map(|member| member.hold.hold()).collect();
                // Which hosts we hold a session to, for our presence: the
                // announcer says it at once when this changes, so the room's
                // map follows a host joining or dropping within its poll.
                // Draining counts, since we still hear through it. The
                // session to where the room meets is said as the word, as in
                // `via`: without it, someone whose media goes through a host
                // shows no line to the relay they also hold.
                local.set_held(super::super::web::clean_held(
                    members
                        .iter()
                        .filter(|member| member.state.up())
                        .map(|member| member.target.host.clone().unwrap_or_else(|| super::super::web::HELD_RELAY.to_string())),
                    "",
                ));
                // Until the relay has answered or failed once, a host with
                // nobody else up is still joining, not cut off.
                let relay_tried = states.first().is_some_and(|state| *state != State::Dialing);
                // A bounce moves the call on, for everyone following moves;
                // what follows below applies it here like any other move.
                if next_bounce.is_some_and(|due| now >= due) {
                    if local.move_calls() {
                        let stops: Vec<Option<String>> = ranked(&members)
                            .into_iter()
                            .filter(|&at| members[at].state != State::Retired)
                            .map(|at| members[at].target.host.clone())
                            .collect();
                        let on = local.call_host();
                        local.move_call(super::super::moves::next_stop(&stops, on.as_deref()), &me);
                    }
                    next_bounce = bounce.map(|every| now + every);
                }

                // The call moved: the host it left is dialled again at the
                // usual price and the one it went to free. A session that is
                // not up keeps none to replace; its next dial reads the price.
                let now_chosen = chosen_member(&members, &local);
                if now_chosen != chosen {
                    for index in [chosen, now_chosen].into_iter().flatten() {
                        let free = now_chosen == Some(index);
                        let member = &mut members[index];
                        // Not up to redial, or dialled at this price already.
                        if member.session.is_none() || member.free == free {
                            continue;
                        }
                        let id = member.target.host.clone().unwrap_or_default();
                        if free {
                            tracing::info!("the call is on host {id}; dialling it again, free");
                        } else {
                            tracing::info!("the call left host {id}; dialling it again at the usual price");
                        }
                        member.free = free;
                        let fresh = dial(index, member, free);
                        member.redialled = member.state.live();
                        if let Some(old) = member.session.replace(fresh) {
                            // Kept a moment, as an old offer is, so what is
                            // pulled through it moves to the new session.
                            tokio::spawn(async move {
                                tokio::time::sleep(MOVE_OVERLAP).await;
                                drop(old);
                            });
                        }
                        if carrier == Some(index) {
                            if let Some(session) = &member.session {
                                _following = Some(follow(session, &estimate, &metrics));
                            }
                        }
                    }
                    chosen = now_chosen;
                }

                // Ranked by id, so a host added late takes its place in the
                // order everyone shares rather than the end.
                let rank = ranked(&members);
                let ranked_at = |index: usize| rank.iter().position(|&at| at == index);
                let ranked_states: Vec<State> = rank.iter().map(|&at| states[at]).collect();
                let ranked_holds: Vec<Duration> = rank.iter().map(|&at| holds[at]).collect();
                let ranked_chosen = chosen.and_then(ranked_at);
                let next = pick(&ranked_states, carrier.and_then(ranked_at), now, started, ranked_chosen, &ranked_holds).map(|at| rank[at]);
                // With nothing live, every offer stays as it is: a session
                // that died carries nothing, and one that comes back finds
                // it still offered.
                if let Some(next) = next {
                    let priced: Vec<bool> = rank.iter().map(|&at| members[at].priced).collect();
                    let spare_at = keeps_spare(hosting, local.spare_route())
                        .then(|| ranked_at(next).and_then(|next| spare(&ranked_states, &priced, next, ranked_chosen)))
                        .flatten()
                        .map(|at| rank[at]);
                    if arrange(&mut members, next, spare_at, &outbound, &me, &relay_label) {
                        keyframe.rerouted();
                    }
                }
                if next.is_some() && next != carrier {
                    let index = next.unwrap_or_default();
                    let member = &members[index];
                    match &member.target.host {
                        None => tracing::info!("our media goes through the relay"),
                        Some(id) => tracing::info!("our media goes through host {id}"),
                    }
                    if let Some(session) = &member.session {
                        _following = Some(follow(session, &estimate, &metrics));
                    }
                    keyframe.ask();
                    carrier = next;
                    on_guests = false;
                } else if next.is_none() && serving && !on_guests && relay_tried {
                    if states.first() == Some(&State::Draining) {
                        tracing::info!("still hosting; the relay is going away");
                    } else {
                        tracing::info!("still hosting; no way to the relay");
                    }
                    local.set_sending(Some(super::super::arrival::Sending::Guests));
                    _following = guests.clone().map(|guests| follow_guests(guests, &estimate));
                    // Every guest reading our own media through the relay
                    // has just been bounced to its direct route. A fresh
                    // read starts at our newest group, but an older relay
                    // still carries a read across from the next one: a
                    // keyframe now, rather than the rest of the encoder's
                    // interval.
                    keyframe.ask();
                    carrier = None;
                    on_guests = true;
                }

                // Said again whenever the carrier or the call's place changes;
                // set only on a change, since the announcer acts on it.
                if let Some(carrier) = carrier.filter(|_| !on_guests) {
                    let call_host = local.call_host();
                    let now = says(members[carrier].target.host.as_deref(), serving, call_host.as_deref(), &me);
                    if local.sending().as_ref() != Some(&now) {
                        if now == super::super::arrival::Sending::Guests {
                            tracing::info!("the call is on us: our media goes to our guests");
                        }
                        local.set_sending(Some(now));
                    }
                }

                let now_via = via(&states, serving);
                if now_via.relay || now_via.hosts > 0 || (now_via.hosting && relay_tried) {
                    if said != Some(now_via) {
                        let _ = status.unbounded_send(RemoteStatus::InRoom {
                            room: room.clone(),
                            via: Some(now_via),
                        });
                        said = Some(now_via);
                        down_said = false;
                    }
                } else if said.is_some() && !down_said {
                    let _ = status.unbounded_send(RemoteStatus::Reconnecting);
                    said = None;
                    down_said = true;
                }
            }
        }),
        bandwidth: None,
        stats: None,
    }
}

/// Puts each session's offer where it should be: the carrier's at no price,
/// the spare's at [`SPARE_COST`], the rest withdrawn. Whether any offer
/// changed.
///
/// **Make before break, and re-price rather than withdraw.** A session that
/// keeps an offer, changing only what it is for, is re-priced in place, so a
/// spare becomes the carrier with no withdrawal for anyone to see. A new
/// offer is made by a task as our paths are read back, so one leaving the
/// pair is kept a moment longer rather than dropped in the same instant; a
/// session that died carries nothing meanwhile.
fn arrange(
    members: &mut [Member],
    carrier: usize,
    spare: Option<usize>,
    outbound: &moq_net::origin::Producer,
    me: &str,
    relay: &str,
) -> bool {
    let mut changed = false;
    for (index, member) in members.iter_mut().enumerate() {
        let want = if index == carrier {
            Some(Role::Carrier)
        } else if Some(index) == spare {
            Some(Role::Spare)
        } else {
            None
        };
        let had = member.offer.as_ref().map(|(_, role)| *role);
        if want == had {
            continue;
        }
        let name = named(member, relay);
        match (had, want) {
            (None, Some(role)) => {
                let Some(gate) = &member.gate else { continue };
                member.offer = Some((gate.offer(outbound, me, role.price()), role));
                changed = true;
                if role == Role::Spare {
                    tracing::info!("a spare offer on {name}");
                }
            }
            (Some(_), Some(role)) => {
                if let Some((offer, held)) = member.offer.as_mut() {
                    offer.reprice(role.price());
                    *held = role;
                }
                changed = true;
                match role {
                    Role::Carrier => tracing::info!("the spare on {name} now carries our media"),
                    Role::Spare => tracing::info!("our offer on {name} is the spare now"),
                }
            }
            (Some(was), None) => {
                if let Some((old, _)) = member.offer.take() {
                    tokio::spawn(async move {
                        tokio::time::sleep(MOVE_OVERLAP).await;
                        drop(old);
                    });
                }
                changed = true;
                if was == Role::Spare {
                    tracing::info!("the spare on {name} is closed");
                }
            }
            (None, None) => {}
        }
    }
    changed
}

/// Which session goes to the host the call has been moved to, if we follow
/// moves and hold one to it.
fn chosen_member(members: &[Member], local: &PeerStats) -> Option<usize> {
    let host = local.call_host()?;
    members
        .iter()
        .position(|member| member.state != State::Retired && member.target.host.as_deref() == Some(host.as_str()))
}

/// Our gates' hops, said in our `routes.json`, so whoever reads it can tell
/// them from a relay in the chains of our media.
fn gates(members: &[Member]) -> Vec<u64> {
    members
        .iter()
        .filter(|member| member.state != State::Retired)
        .filter_map(|member| member.gate.as_ref())
        .map(|gate| gate.origin.hop().id())
        .collect()
}

/// Follows one session: its estimate into the call's, its figures into the
/// Network tab, until the returned handle is dropped.
fn follow(session: &Session, estimate: &moq_net::bandwidth::Producer, metrics: &Arc<Metrics>) -> Following {
    // Nothing measured on the old link applies to the new one; the rate
    // policy holds its target while there is no estimate.
    let _ = estimate.set(None);
    let monitor = session.monitor.clone();
    let metrics = metrics.clone();
    Following {
        _estimate: Abort(tokio::spawn(adapt::mirror(session.bandwidth.clone(), estimate.clone()))),
        _stats: Some(Abort(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(200));
            loop {
                ticker.tick().await;
                if let Some(stats) = monitor.stats() {
                    metrics.set_connection(&stats);
                }
            }
        }))),
    }
}

/// Follows our guests' slowest link, when hosting is all that is left.
fn follow_guests(guests: moq_net::bandwidth::Consumer, estimate: &moq_net::bandwidth::Producer) -> Following {
    let _ = estimate.set(None);
    Following {
        _estimate: Abort(tokio::spawn(adapt::mirror(guests, estimate.clone()))),
        _stats: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOOR: &str = "00112233445566778899aabbccddeeff";

    fn id(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    fn known(c: char) -> door::Known {
        door::Known {
            id: id(c),
            door: DOOR.to_string(),
        }
    }

    fn relay() -> url::Url {
        "http://localhost:4443/team".parse().unwrap()
    }

    fn hosts_of(targets: &[Target]) -> Vec<Option<String>> {
        targets.iter().map(|target| target.host.as_ref().map(|id| id[..1].to_string())).collect()
    }

    #[test]
    fn the_relay_comes_first_then_hosts_by_id_whatever_order_they_were_heard_in() {
        let one = targets(&relay(), &[known('c'), known('a'), known('b')], &id('f'), false);
        let other = targets(&relay(), &[known('b'), known('c'), known('a'), known('a')], &id('f'), false);
        assert_eq!(hosts_of(&one), [None, Some("a".into()), Some("b".into()), Some("c".into())]);
        assert_eq!(hosts_of(&one), hosts_of(&other));
        assert_eq!(one[1].url.as_str(), format!("iroh://{}/team?door={DOOR}", id('a')));
    }

    #[test]
    fn we_and_the_relay_itself_and_a_bad_door_are_never_dialled_as_hosts() {
        let relay: url::Url = format!("iroh://{}/team?door={DOOR}", id('b')).parse().unwrap();
        let bad = door::Known {
            id: id('d'),
            door: "not a door".to_string(),
        };
        let found = targets(&relay, &[known('a'), known('b'), known('c'), bad], &id('c'), false);
        assert_eq!(hosts_of(&found), [None, Some("a".into())]);
    }

    /// Nine hosts heard in two different orders, before any session to
    /// them is up: the same eight kept, the lowest ids, so everyone holds the
    /// same ones.
    #[test]
    fn the_same_nine_hosts_heard_in_any_order_keep_the_same_lowest_eight() {
        let nine: Vec<char> = "123456789".chars().collect();
        let heard = |order: &[char]| {
            let mut hosts = Hosts::default();
            for c in order {
                hosts.learn(Learned::Host(target(&relay(), &known(*c)).unwrap()), Instant::now());
            }
            let ids: Vec<String> = hosts.want().into_iter().map(|target| target.host.clone().unwrap()).collect();
            kept(&ids.iter().map(String::as_str).collect::<Vec<_>>(), &[])
        };
        let forwards = heard(&nine);
        let mut shuffled = nine.clone();
        shuffled.reverse();
        shuffled.swap(0, 4);
        assert_eq!(forwards, heard(&shuffled));
        assert_eq!(forwards, "12345678".chars().map(id).collect::<Vec<_>>());
    }

    /// Eight hosts held and working, then eight more claiming lower ids: none
    /// of the working ones is given up. A slot opens only when one of them
    /// stops working, and then the lowest newcomer takes it.
    #[test]
    fn hosts_with_lower_ids_never_evict_sessions_that_work() {
        let all: Vec<String> = "0123456789abcdef".chars().map(id).collect();
        let wanted: Vec<&str> = all.iter().map(String::as_str).collect();
        let held: Vec<&str> = wanted[8..].to_vec();
        let mut kept_now = kept(&wanted, &held);
        kept_now.sort();
        assert_eq!(kept_now, all[8..].to_vec(), "a lower id evicted a working session");
        let mut kept_now = kept(&wanted, &held[..7]);
        kept_now.sort();
        let mut expected = vec![id('0')];
        expected.extend(all[8..15].iter().cloned());
        assert_eq!(kept_now, expected);
    }

    /// What presence says changes what is wanted: a new host or a new door
    /// does, the same again does not, a stop takes it out, and a leave only
    /// marks it until its session ends, unless it is heard again first.
    #[test]
    fn hosts_follow_what_presence_says() {
        let now = Instant::now();
        let mut hosts = Hosts::new(targets(&relay(), &[known('a')], &id('z'), false));
        let a = || target(&relay(), &known('a')).unwrap();
        assert!(!hosts.learn(Learned::Host(a()), now), "heard again as it was known");
        let mut moved = a();
        moved.url.set_query(Some("door=ffeeddccbbaa99887766554433221100"));
        assert!(hosts.learn(Learned::Host(moved.clone()), now), "a new door");
        assert_eq!(hosts.want(), vec![&moved]);
        assert!(hosts.learn(Learned::Left(id('a')), now));
        assert!(hosts.has_left(&id('a')));
        assert_eq!(hosts.want().len(), 1, "a leave alone cuts nothing");
        assert!(hosts.learn(Learned::Host(moved.clone()), now), "back before its session ended");
        assert!(!hosts.has_left(&id('a')));
        hosts.learn(Learned::Left(id('a')), now);
        hosts.let_go(&id('a'));
        assert!(hosts.want().is_empty(), "let go once its session ended");
        assert!(!hosts.learn(Learned::Left(id('b')), now), "a leave of someone not hosting");
        hosts.learn(Learned::Host(a()), now);
        assert!(hosts.learn(Learned::Stopped(id('a')), now));
        assert!(hosts.want().is_empty());
    }

    /// A burst of doors for one host: the first change is acted on, the rest
    /// wait out the settle, and then only the latest is, once.
    #[test]
    fn a_burst_of_doors_is_acted_on_once_and_the_latest_wins() {
        let start = Instant::now();
        let mut hosts = Hosts::new(targets(&relay(), &[known('a')], &id('z'), false));
        let door = |n: u8| {
            let mut at = target(&relay(), &known('a')).unwrap();
            at.url.set_query(Some(&format!("door={}", format!("{n:02x}").repeat(16))));
            at
        };
        assert!(hosts.learn(Learned::Host(door(1)), start), "the first change");
        let mut acted = 0;
        for n in 2..=40u8 {
            let now = start + Duration::from_millis(100) * u32::from(n);
            acted += usize::from(hosts.learn(Learned::Host(door(n)), now));
            acted += usize::from(hosts.settle(now));
        }
        assert_eq!(acted, 0, "a change acted on within the settle");
        assert_eq!(hosts.want(), vec![&door(1)]);
        assert!(hosts.settle(start + DOOR_SETTLE));
        assert_eq!(hosts.want(), vec![&door(40)], "the latest wins");
        assert!(!hosts.settle(start + DOOR_SETTLE * 3), "and only once");
    }

    #[test]
    fn a_host_dials_only_the_hosts_below_it() {
        let found = targets(&relay(), &[known('a'), known('c'), known('e')], &id('d'), true);
        assert_eq!(hosts_of(&found), [None, Some("a".into()), Some("c".into())]);
    }

    #[test]
    fn a_host_target_never_shows_its_door() {
        let found = targets(&relay(), &[known('a')], &id('f'), false);
        assert!(!format!("{found:?}").contains(DOOR));
    }

    #[test]
    fn our_media_moves_down_at_once_and_back_up_only_once_the_better_one_held() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        use State::*;
        let pick = |states: &[State], current, now| pick(states, current, now, start, None, &[]);
        assert_eq!(pick(&[Dialing, Dialing], None, at(0)), None);
        assert_eq!(pick(&[Dialing, Live(at(0))], None, at(500)), None, "the relay gets a moment to answer");
        assert_eq!(pick(&[Dialing, Live(at(0))], None, at(2000)), Some(1), "then a host while the relay still dials");
        assert_eq!(pick(&[Resting(at(9)), Live(at(0))], None, at(100)), Some(1), "at once once the relay failed");
        assert_eq!(pick(&[Live(at(2100)), Live(at(0))], Some(1), at(2500)), Some(1), "the relay has not held yet");
        assert_eq!(pick(&[Live(at(2100)), Live(at(0))], Some(1), at(4200)), Some(0), "now it has");
        assert_eq!(pick(&[Redialling, Live(at(0)), Live(at(0))], Some(0), at(3000)), Some(1), "down at once");
        assert_eq!(pick(&[Live(at(0)), Live(at(0))], None, at(0)), Some(0), "the first choice needs no hold");
        assert_eq!(pick(&[Refused, Resting(at(0)), Live(at(0))], Some(1), at(0)), Some(2));
    }

    #[test]
    fn the_host_the_call_moved_to_goes_first_and_the_rest_keep_their_order() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        use State::*;
        let pick = |states: &[State], current, now, preferred| pick(states, current, now, start, preferred, &[]);
        let all = [Live(at(0)), Live(at(0)), Live(at(0))];
        assert_eq!(pick(&all, Some(0), at(5000), Some(2)), Some(2), "held, so at once");
        assert_eq!(pick(&all, None, at(5000), Some(1)), Some(1));
        assert_eq!(pick(&all, Some(2), at(5000), None), Some(0), "back to the usual order");
        assert_eq!(
            pick(&[Live(at(0)), Live(at(0)), Live(at(4000))], Some(0), at(5000), Some(2)),
            Some(0),
            "not until it has held"
        );
        assert_eq!(pick(&[Live(at(0)), Live(at(0)), Resting(at(0))], Some(2), at(5000), Some(2)), Some(0), "unreachable: the usual order");
        assert_eq!(pick(&[Redialling, Live(at(0)), Resting(at(0))], None, at(5000), Some(2)), Some(1));
        assert_eq!(pick(&[Live(at(0)), Live(at(0))], Some(1), at(5000), Some(1)), Some(1), "stays on it");
        assert_eq!(pick(&all, Some(1), at(5000), Some(9)), Some(0), "a host we do not hold is no preference");
    }

    #[test]
    fn the_spare_is_the_first_other_live_session_that_carries_a_price() {
        use State::*;
        let now = Instant::now();
        let all = [Live(now), Live(now), Live(now)];
        let priced = [true, true, true];
        assert_eq!(spare(&all, &priced, 0, None), Some(1), "the relay carries, the first host waits");
        assert_eq!(spare(&all, &priced, 1, None), Some(0), "where our media would go next: the relay");
        assert_eq!(spare(&all, &priced, 2, Some(2)), Some(0), "moved to a host: the relay is next");
        assert_eq!(spare(&all, &priced, 0, Some(2)), Some(2), "moved, not yet there: the chosen host");
        assert_eq!(spare(&all, &[false, true, true], 2, Some(2)), Some(1), "a relay that cannot carry the price is passed over");
        assert_eq!(spare(&all, &[false, true, true], 1, None), Some(2));
        assert_eq!(spare(&[Live(now), Live(now)], &[false, true], 1, Some(1)), None, "nowhere priced to put it");
        assert_eq!(spare(&[Redialling, Live(now), Resting(now), Live(now)], &[true; 4], 1, None), Some(3), "only a live one");
        assert_eq!(spare(&[Live(now)], &[true], 0, None), None, "the relay alone");
        assert_eq!(spare(&all, &[true], 0, None), None, "a price never heard is no price");
    }

    #[test]
    fn a_session_that_says_it_is_going_away_carries_nothing_new_but_still_counts_as_up() {
        use State::*;
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let pick = |states: &[State], current| pick(states, current, at(5000), start, None, &[]);
        assert_eq!(pick(&[Draining, Live(at(0))], Some(0)), Some(1), "off it at once");
        assert_eq!(pick(&[Draining, Live(at(4000))], Some(0)), Some(1), "down needs no hold");
        assert_eq!(pick(&[Draining, Redialling], Some(0)), None, "nowhere else: the offer stays where it is");
        assert_eq!(pick(&[Live(at(0)), Draining], Some(0)), Some(0));
        let priced = [true; 3];
        assert_eq!(spare(&[Live(at(0)), Draining, Live(at(0))], &priced, 0, None), Some(2));
        assert_eq!(spare(&[Live(at(0)), Draining], &priced[..2], 0, None), None, "never a spare on it, even alone");
        assert_eq!(via(&[Draining, Live(at(0))], false), Via { relay: true, hosts: 1, hosting: false });
        assert_eq!(ended(&[Draining, Refused], false, &[None, None]), None, "the call goes on through it");
    }

    #[test]
    fn a_session_left_as_quiet_waits_its_own_hold_before_our_media_moves_back() {
        use State::*;
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let holds = [Duration::from_secs(8), HOLD];
        let pick = |states: &[State], current, now| pick(states, current, now, start, None, &holds);
        let back = [Live(at(10_000)), Live(at(0))];
        assert_eq!(pick(&back, Some(1), at(12_000)), Some(1), "the usual hold is not enough");
        assert_eq!(pick(&back, Some(1), at(17_999)), Some(1));
        assert_eq!(pick(&back, Some(1), at(18_000)), Some(0), "its own hold");
    }

    #[test]
    fn a_route_price_is_carried_from_moq_lite_06_on() {
        let version = |name: &str| name.parse::<moq_net::Version>().expect("a version this moq-net knows");
        for older in ["moq-lite-01", "moq-lite-02", "moq-lite-03", "moq-lite-04", "moq-lite-05", "moq-transport-14"] {
            assert!(!carries_price(version(older)), "{older}");
        }
        for newer in ["moq-lite-06", "moq-lite-07-wip"] {
            assert!(carries_price(version(newer)), "{newer}");
        }
    }

    #[test]
    fn a_host_the_call_is_on_says_its_guests_and_everyone_else_their_carrier() {
        use super::super::super::arrival::Sending;
        let (me, other) = (id('c'), id('d'));
        assert_eq!(says(None, true, Some(&me), &me), Sending::Guests, "the call is on us");
        assert_eq!(says(None, true, None, &me), Sending::Relay, "the call is on the relay");
        assert_eq!(says(None, true, Some(&other), &me), Sending::Relay, "the call is on another host we reach through the relay");
        assert_eq!(says(Some(&other), true, Some(&other), &me), Sending::Host(other.clone()));
        assert_eq!(says(None, false, Some(&me), &me), Sending::Relay, "not serving: nobody reads us as a host");
        assert_eq!(says(Some(&other), false, None, &me), Sending::Host(other.clone()));
    }

    #[test]
    fn a_spare_is_kept_only_with_the_switch_on_and_never_while_hosting() {
        assert!(keeps_spare(false, true));
        assert!(!keeps_spare(false, false));
        assert!(!keeps_spare(true, true), "a host offers its one origin on every session already");
        assert!(!keeps_spare(true, false));
    }

    #[test]
    fn the_join_ends_only_when_every_way_in_is_gone_and_we_do_not_host() {
        use State::*;
        let now = Instant::now();
        let errors = [Some("relay gone".to_string()), None];
        assert_eq!(ended(&[Dialing, Refused], false, &errors), None);
        assert_eq!(ended(&[Resting(now), Live(now)], false, &errors), None);
        assert_eq!(ended(&[Resting(now), Refused], true, &errors), None, "a host stays");
        assert_eq!(ended(&[Resting(now), Refused], false, &errors).as_deref(), Some("relay gone"));
        assert_eq!(ended(&[Refused, Refused], false, &errors).as_deref(), Some(TURNED_AWAY));
        assert_eq!(ended(&[Refused, Retired], false, &errors).as_deref(), Some(TURNED_AWAY), "a host let go is no way in");
        assert_eq!(ended(&[Live(now), Retired], false, &errors), None);
    }

    #[test]
    fn the_status_says_where_we_are() {
        use State::*;
        let now = Instant::now();
        assert_eq!(via(&[Live(now), Dialing], false), Via { relay: true, hosts: 0, hosting: false });
        assert_eq!(via(&[Resting(now), Live(now), Live(now)], true), Via { relay: false, hosts: 2, hosting: true });
    }
}

/// The set over real sessions on local sockets: raw QUIC to 127.0.0.1, as
/// `routes.rs` and the served tests run them. No iroh, so the refusal
/// heuristic is not what ends a refused session here: raw QUIC reports the
/// refusal itself.
#[cfg(test)]
mod socket_tests {
    use super::super::{accept, camera_path};
    use super::*;

    const WITHIN: Duration = Duration::from_secs(5);

    fn crypto() {
        let _ = moq_tokio::rustls::crypto::aws_lc_rs::default_provider().install_default();
    }

    fn id(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    /// One shared origin behind a local port: a relay, or a host. Keeps the
    /// sessions it accepts so it can end them the way one that dies does.
    struct Meeting {
        origin: moq_net::origin::Producer,
        port: u16,
        sessions: Arc<std::sync::Mutex<Vec<moq_net::Session>>>,
        _accepting: Abort,
    }

    impl Meeting {
        async fn start() -> Meeting {
            Self::sharing(moq_tokio::origin::spawn()).await
        }

        async fn sharing(origin: moq_net::origin::Producer) -> Meeting {
            Self::on(0, origin).await
        }

        /// On a given port, so a relay can come up where members are already
        /// dialling it.
        async fn on(port: u16, origin: moq_net::origin::Producer) -> Meeting {
            Self::try_on(port, origin).await.expect("bound")
        }

        /// The same, for a port that may still be held a moment by a relay
        /// that just stopped there.
        async fn try_on(port: u16, origin: moq_net::origin::Producer) -> Option<Meeting> {
            Self::speaking(port, origin, Vec::new()).await
        }

        /// A relay that speaks no later than moq-lite-05, as the production
        /// one does: its routes carry no price.
        async fn older() -> Meeting {
            let versions = vec!["moq-lite-05".parse().expect("a version this moq-net knows")];
            Self::speaking(0, moq_tokio::origin::spawn(), versions).await.expect("bound")
        }

        /// `versions` empty speaks every version, as a default server does.
        /// `None` when the port cannot be bound.
        async fn speaking(port: u16, origin: moq_net::origin::Producer, versions: Vec<moq_net::Version>) -> Option<Meeting> {
            crypto();
            let mut config = moq_tokio::server::Config::default();
            config.listen.version = versions;
            config.listen.bind = Some(moq_tokio::listen::Bind::Addr(([127, 0, 0, 1], port).into()));
            config.listen.tls.generate = vec!["localhost".to_string()];
            config.publisher = Some(origin.consume());
            config.subscriber = Some(origin.clone());
            let mut server = config.init().ok()?.bind().await.ok()?;
            let port = server.local_addr().unwrap().port();
            let sessions = Arc::new(std::sync::Mutex::new(Vec::new()));
            let kept = sessions.clone();
            let accepting = Abort(tokio::spawn(async move {
                while let Some(request) = server.accept().await {
                    let kept = kept.clone();
                    tokio::spawn(async move {
                        if let Ok(session) = request.ok().await {
                            kept.lock().unwrap().push(session);
                        }
                    });
                }
            }));
            Some(Meeting {
                origin,
                port,
                sessions,
                _accepting: accepting,
            })
        }

        fn url(&self) -> url::Url {
            at(self.port)
        }

        /// Ends every session at once, as a relay that dies does, and stops
        /// accepting.
        async fn stop(self) {
            for session in self.sessions.lock().unwrap().iter() {
                session.abort(moq_net::Error::Cancel);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn at(port: u16) -> url::Url {
        format!("moqt://localhost:{port}/team").parse().unwrap()
    }

    /// A local port held for a server that is down, or not up yet, that
    /// refuses meanwhile as a port nothing listens on does.
    ///
    /// Bound and then dropped, the port is free for anything else in the
    /// process to be given, and the tests run side by side: another test's
    /// server or client could take it, and a member dialling "nothing" would
    /// reach that. So the socket keeps it, connected to an address nothing
    /// sends from: datagrams from anywhere else match no socket, and the
    /// sender is refused at once, as it is by a port that is not bound.
    struct Reserved(std::net::UdpSocket);

    impl Reserved {
        fn new() -> Reserved {
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            // The discard port: nothing on this machine sends from it.
            socket.connect("127.0.0.1:9").unwrap();
            Reserved(socket)
        }

        fn port(&self) -> u16 {
            self.0.local_addr().unwrap().port()
        }

        fn url(&self) -> url::Url {
            at(self.port())
        }

        /// Lets the port go, for a server to bind it straight after.
        fn release(self) -> u16 {
            self.port()
        }
    }

    /// How long a test client keeps trying a way in before giving up on it.
    /// Well short of the app's own budget, so a test of a dead address does
    /// not wait that out, but longer than a live one can take on a busy
    /// machine, where QUIC may have to send a lost handshake packet again:
    /// after a second, and again two seconds later, before any round trip has
    /// been measured. A join rests a session that gave up for longer than any
    /// test here waits, so a budget too short fails the test outright.
    const GIVE_UP: Duration = Duration::from_secs(5);

    /// Where every test client sends from: the loopback address, as every
    /// test server listens on it.
    ///
    /// Left to its default, a client binds a dual-stack `[::]` port, and
    /// macOS hands out the same port number to a socket on `[::]` and to one
    /// on `127.0.0.1`: one test's client and another's server. A packet to
    /// `127.0.0.1` on that port goes to the more specific socket, the server,
    /// so the client never hears its answers until that server goes. Traced:
    /// handshakes of 4.3 s or none at all, the server's replies arriving at
    /// another test's server and refused there; about one dial in a hundred
    /// with many at once. Two sockets on `127.0.0.1` are never given one
    /// port. The app's own listener binds `[::]`, so this is the tests' doing.
    ///
    /// It also spares every dial of `localhost` a quarter of a second: the
    /// `[::1]` attempt, which cannot be sent from here, fails at once rather
    /// than holding the IPv4 one back until the race lets it go.
    const LOOPBACK: std::net::SocketAddr = std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 0);

    /// A client as the app's, minus iroh, giving up on a dead address after
    /// [`GIVE_UP`].
    fn client() -> moq_tokio::Client {
        crypto();
        let mut config = moq_tokio::client::Config::default();
        config.connect.bind = Some(LOOPBACK);
        config.connect.tls.insecure = Some(true);
        config.connect.backoff.timeout = GIVE_UP;
        // As a join's client pings: what the quiet limits are set against.
        config.quic.keep_alive = quiet::KEEP_ALIVE;
        config.init().unwrap()
    }

    /// A member of the room joining through the set, publishing a camera
    /// with a frame every 20 ms.
    struct Member {
        inbound: moq_net::origin::Producer,
        statuses: futures_channel::mpsc::UnboundedReceiver<RemoteStatus>,
        ended: tokio::sync::oneshot::Receiver<String>,
        _camera: Option<moq_net::broadcast::Producer>,
        _writing: Option<Abort>,
        _joined: Abort,
        /// What its presence readers would tell it of the room's hosts.
        learn: futures_channel::mpsc::UnboundedSender<Learned>,
    }

    fn member(me: &str, targets: Vec<Target>) -> Member {
        member_with(me, targets, client())
    }

    /// A client that keeps redialling a relay that is down, and notices it
    /// back within half a second, as the app's does within its budget.
    fn patient_client() -> moq_tokio::Client {
        crypto();
        let mut config = moq_tokio::client::Config::default();
        config.connect.bind = Some(LOOPBACK);
        config.connect.tls.insecure = Some(true);
        config.connect.backoff.initial = Duration::from_millis(200);
        config.connect.backoff.max = Duration::from_millis(500);
        config.connect.backoff.timeout = Duration::from_secs(60);
        config.quic.keep_alive = quiet::KEEP_ALIVE;
        config.init().unwrap()
    }

    fn member_with(me: &str, targets: Vec<Target>, client: moq_tokio::Client) -> Member {
        member_as(me, targets, client, Arc::new(PeerStats::default()))
    }

    /// A member whose own entry is `local`, where moves of the call are
    /// switched on and made.
    fn member_as(me: &str, targets: Vec<Target>, client: moq_tokio::Client, local: Arc<PeerStats>) -> Member {
        member_from(me, targets, client, local, moq_tokio::origin::spawn())
    }

    /// The same, publishing from `outbound`, where something else of ours
    /// may already be.
    fn member_from(
        me: &str,
        targets: Vec<Target>,
        client: moq_tokio::Client,
        local: Arc<PeerStats>,
        outbound: moq_net::origin::Producer,
    ) -> Member {
        member_sending(me, targets, client, local, outbound, 100)
    }

    /// The same, with frames of `bytes` each.
    fn member_sending(
        me: &str,
        targets: Vec<Target>,
        client: moq_tokio::Client,
        local: Arc<PeerStats>,
        outbound: moq_net::origin::Producer,
        bytes: usize,
    ) -> Member {
        let camera = outbound.create_broadcast(camera_path(me)).unwrap();
        let mut track = camera.create_track("video", None).unwrap();
        camera.announce(moq_net::origin::Route::default()).unwrap();
        let writing = Abort(tokio::spawn(async move {
            for n in 0u64.. {
                if track.write_frame(moq_net::Timestamp::from_millis(n * 20).unwrap(), vec![0u8; bytes]).is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }));
        let mut member = joined(me, targets, client, local, outbound);
        member._camera = Some(camera);
        member._writing = Some(writing);
        member
    }

    /// A member joined through the set with nothing of its own published
    /// yet: what it offers is whatever `outbound` comes to hold.
    fn joined(
        me: &str,
        targets: Vec<Target>,
        client: moq_tokio::Client,
        local: Arc<PeerStats>,
        outbound: moq_net::origin::Producer,
    ) -> Member {
        joined_asking(me, targets, client, local, outbound, Arc::new(Keyframes::default()))
    }

    /// The same, asking for a keyframe through `keyframe` when our media
    /// moves, as the call does of its encode thread.
    fn joined_asking(
        me: &str,
        targets: Vec<Target>,
        client: moq_tokio::Client,
        local: Arc<PeerStats>,
        outbound: moq_net::origin::Producer,
        keyframe: Arc<Keyframes>,
    ) -> Member {
        let inbound = moq_tokio::origin::spawn();
        let (status, statuses) = futures_channel::mpsc::unbounded();
        let (learn, learned) = futures_channel::mpsc::unbounded();
        let peer = join(Join {
            client,
            me: me.to_string(),
            targets,
            outbound,
            inbound: inbound.clone(),
            hosting: false,
            estimate: moq_net::bandwidth::Producer::new(),
            guests: None,
            heard: None,
            metrics: Arc::new(Metrics::default()),
            local,
            status,
            keyframe,
            room: "room team".to_string(),
            bounce: None,
            learned,
        });
        let (end, ended) = tokio::sync::oneshot::channel();
        let joined = Abort(tokio::spawn(async move {
            let _ = end.send(peer.closed.await);
        }));
        Member {
            inbound,
            statuses,
            ended,
            _camera: None,
            _writing: None,
            _joined: joined,
            learn,
        }
    }

    fn relay_at(url: url::Url) -> Target {
        Target { host: None, url }
    }

    fn host_at(c: char, url: url::Url) -> Target {
        Target { host: Some(id(c)), url }
    }

    /// Waits for `path` to be announced (`active`) or retracted at `origin`.
    async fn becomes(origin: &moq_net::origin::Producer, path: &str, active: bool) {
        let mut announced = origin.consume().announced();
        tokio::time::timeout(WITHIN, async {
            while let Some(update) = announced.next().await {
                if update.prefix.as_str() == path && update.kind.is_active() == active {
                    return;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{path} never became {}", if active { "announced" } else { "retracted" }));
    }

    /// Whether `path` is announced at `origin` within `wait`.
    async fn heard_within(origin: &moq_net::origin::Producer, path: &str, wait: Duration) -> bool {
        let mut announced = origin.consume().announced();
        tokio::time::timeout(wait, async {
            while let Some(update) = announced.next().await {
                if update.prefix.as_str() == path && update.kind.is_active() {
                    return;
                }
            }
            std::future::pending::<()>().await;
        })
        .await
        .is_ok()
    }

    /// The best route to `path` at `origin`, if it has one within a moment:
    /// `routed` alone waits for as long as the path is not there.
    async fn route_within(origin: &moq_net::origin::Producer, path: &str) -> Option<moq_net::origin::Route> {
        tokio::time::timeout(Duration::from_millis(200), origin.consume().routed(path))
            .await
            .ok()
            .flatten()
    }

    /// The next status that names the ways in.
    async fn next_via(member: &mut Member) -> Via {
        tokio::time::timeout(WITHIN, async {
            loop {
                if let Some(RemoteStatus::InRoom { via: Some(via), .. }) = member.statuses.next().await {
                    return via;
                }
            }
        })
        .await
        .expect("a status saying where we are")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_relay_that_is_down_at_join_does_not_hold_up_a_host() {
        let host = Meeting::start().await;
        let down = Reserved::new();
        let started = Instant::now();
        let mut m = member("m", vec![relay_at(down.url()), host_at('a', host.url())]);
        becomes(&host.origin, &camera_path("m"), true).await;
        assert_eq!(next_via(&mut m).await, Via { relay: false, hosts: 1, hosting: false });
        // The relay's moment to answer first, and no more: its own retries
        // would take the whole budget.
        assert!(started.elapsed() < Duration::from_secs(4), "it waited on the relay: {:?}", started.elapsed());
        // The relay gives up, and the call goes on through the host.
        tokio::time::sleep((started + GIVE_UP + Duration::from_secs(1)).saturating_duration_since(Instant::now())).await;
        assert!(m.ended.try_recv().is_err(), "the join ended with a host still up");
        assert!(heard_within(&host.origin, &camera_path("m"), Duration::from_millis(200)).await);
    }

    /// With the spare switched off, one offer and nowhere else.
    #[tokio::test(flavor = "multi_thread")]
    async fn with_no_spare_our_media_is_offered_on_the_relay_alone_while_it_is_up() {
        let relay = Meeting::start().await;
        let host = Meeting::start().await;
        let local = Arc::new(PeerStats::default());
        local.set_spare_route(false);
        let mut m = member_as("m", vec![relay_at(relay.url()), host_at('a', host.url())], client(), local);
        becomes(&relay.origin, &camera_path("m"), true).await;
        assert!(
            !heard_within(&host.origin, &camera_path("m"), Duration::from_secs(1)).await,
            "the host was offered our media while the relay was up"
        );
        let mut via = next_via(&mut m).await;
        while via.hosts == 0 || !via.relay {
            via = next_via(&mut m).await;
        }
        assert_eq!(via, Via { relay: true, hosts: 1, hosting: false });
        // Nothing of theirs is lost by not offering there: the relay's room
        // still reaches us through the session we only listen on.
        drop(relay);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn when_the_relay_dies_our_media_moves_to_the_host_and_a_viewer_on_both_gets_it_back() {
        let relay = Meeting::start().await;
        let host = Meeting::start().await;
        let _m = member("m", vec![relay_at(relay.url()), host_at('a', host.url())]);
        let viewer = member("v", vec![relay_at(relay.url()), host_at('a', host.url())]);
        becomes(&relay.origin, &camera_path("m"), true).await;
        becomes(&viewer.inbound, &camera_path("m"), true).await;

        let died = Instant::now();
        relay.stop().await;
        becomes(&host.origin, &camera_path("m"), true).await;
        let moved = died.elapsed();
        // Through the host now, after the library's hold on a publisher that
        // was just retracted.
        let deadline = Instant::now() + WITHIN;
        loop {
            if let Some(route) = viewer.inbound.consume().routed(camera_path("m")).await {
                if route.hops.as_slice().last() == Some(&host.origin.hop()) {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "the viewer never reached m through the host");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let back = died.elapsed();
        assert!(moved < Duration::from_secs(2), "the offer took {moved:?} to move");
        assert!(back < Duration::from_secs(5), "the viewer took {back:?} to have m again");
    }

    /// With the spare on, the host hears us too, but at the spare's price,
    /// and the relay hears us free: the carrier and the spare, told apart by
    /// what they cost.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_spare_is_offered_on_the_host_at_its_price_and_the_carrier_free() {
        let relay = Meeting::start().await;
        let host = Meeting::start().await;
        let _m = member("m", vec![relay_at(relay.url()), host_at('a', host.url())]);
        becomes(&relay.origin, &camera_path("m"), true).await;
        becomes(&host.origin, &camera_path("m"), true).await;
        let carried = route_within(&relay.origin, &camera_path("m")).await.expect("m on the relay");
        let spare = route_within(&host.origin, &camera_path("m")).await.expect("m on the host");
        assert!(carried.cost.warm < HOST_COST, "{carried:?}");
        assert!(spare.cost.warm >= SPARE_COST + HOST_COST, "{spare:?}");
    }

    /// A refused session is dropped rather than redialled for the whole
    /// budget; when every way in refuses, the join ends saying so.
    #[tokio::test(flavor = "multi_thread")]
    async fn when_every_way_in_turns_us_away_the_join_ends_at_once_saying_so() {
        crypto();
        let shared = moq_tokio::origin::spawn();
        let mut config = moq_tokio::server::Config::default();
        config.listen.bind = Some(moq_tokio::listen::Bind::Addr(([127, 0, 0, 1], 0).into()));
        config.listen.tls.generate = vec!["localhost".to_string()];
        config.publisher = Some(shared.consume());
        config.subscriber = Some(shared);
        let server = config.init().unwrap().bind().await.unwrap();
        let port = server.local_addr().unwrap().port();
        let hosting = door::Hosting::open("team".to_string()).unwrap();
        let estimates = adapt::Estimates::new(moq_net::bandwidth::Producer::new());
        let (_accepting, _) = accept(server, Some(hosting), Some(estimates), Arc::new(Metrics::default()), None, None, None);
        // No door: what a member holding an old one presents, as far as the
        // host can tell.
        let started = Instant::now();
        let m = member("m", vec![relay_at(at(port)), host_at('a', at(port))]);
        let ended = tokio::time::timeout(WITHIN, m.ended).await.expect("ended in time").expect("an ending");
        assert_eq!(ended, TURNED_AWAY, "{ended}");
        assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
    }

    /// A hosting member's own camera, as a member dialling both the relay
    /// and that host sees it: through the relay, because the host link is
    /// priced. Unpriced, it would be one hop away directly and pulled from
    /// the host by every member.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_hosts_own_camera_is_reached_through_the_relay_not_straight_from_the_host() {
        let relay = Meeting::start().await;
        // The host: one origin for its camera, its session to the relay, and
        // its guests.
        let shared = moq_tokio::origin::spawn();
        let camera = shared.create_broadcast(camera_path("h")).unwrap();
        camera.announce(moq_net::origin::Route::default()).unwrap();
        let _to_relay = client()
            .with_publisher(shared.consume())
            .with_subscriber(shared.clone())
            .connect(relay.url());
        let host = Meeting::sharing(shared).await;
        becomes(&relay.origin, &camera_path("h"), true).await;

        let m = member("m", vec![relay_at(relay.url()), host_at('h', host.url())]);
        becomes(&m.inbound, &camera_path("h"), true).await;
        // Both routes have time to arrive and be ranked.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let route = m.inbound.consume().routed(camera_path("h")).await.expect("a route to h");
        assert_eq!(route.hops.as_slice().last(), Some(&relay.origin.hop()), "{route:?}");
        drop(camera);
    }

    /// The direction that went wrong: a host serving a member while the relay
    /// is down, the relay coming back, and the member moving its offer up to
    /// it. The member's offer leaves the host's session while the host is
    /// also on the relay, and the host must keep it through the relay rather
    /// than see a leave that never ends. Then the relay dies again, and the
    /// host gets the member back directly.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_keeps_a_member_who_moves_up_to_the_relay_and_back() {
        let reserved = Reserved::new();
        let port = reserved.port();
        // The host: one origin for its session to the relay and its guests.
        let shared = moq_tokio::origin::spawn();
        let host = Meeting::sharing(shared.clone()).await;
        let _to_relay = patient_client()
            .with_publisher(shared.consume())
            .with_subscriber(shared.clone())
            .connect(at(port));
        let _m = member_with("m", vec![relay_at(at(port)), host_at('h', host.url())], patient_client());
        becomes(&shared, &camera_path("m"), true).await;

        let relay = Meeting::on(reserved.release(), moq_tokio::origin::spawn()).await;
        // Moved once the relay has m from m, not bridged through the host, and
        // as m's carrier rather than its spare. m offers the spare on the relay
        // as soon as the relay answers, two seconds before moving there, and
        // until the host's own session reaches the relay the spare is the
        // relay's only route to m. Taken for the move, it had the host checked
        // below before m had moved at all.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(route) = route_within(&relay.origin, &camera_path("m")).await {
                if !route.hops.as_slice().contains(&shared.hop()) && route.cost.warm < SPARE_COST {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "m never moved up to the relay");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Past the overlap, when the old offer leaves the host's session. A
        // host that lost m then has no route to it at all, and never gets one.
        tokio::time::sleep(MOVE_OVERLAP + Duration::from_secs(1)).await;
        let route = route_within(&shared, &camera_path("m")).await.expect("the host lost m when m moved to the relay");
        assert_eq!(route.hops.as_slice().last(), Some(&relay.origin.hop()), "{route:?}");

        let relay_hop = relay.origin.hop();
        relay.stop().await;
        let deadline = Instant::now() + WITHIN;
        loop {
            if let Some(route) = route_within(&shared, &camera_path("m")).await {
                if route.hops.as_slice().last() != Some(&relay_hop) {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "the host never had m back directly");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Our own entry with moves of the call switched on.
    fn following() -> Arc<PeerStats> {
        let local = Arc::new(PeerStats::default());
        local.set_move_calls(true);
        local
    }

    /// Waits until `origin`'s best route to `path` arrives through `through`.
    async fn routed_through(origin: &moq_net::origin::Producer, path: &str, through: moq_net::Hop, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(route) = route_within(origin, path).await {
                if route.hops.as_slice().last() == Some(&through) {
                    return;
                }
            }
            assert!(Instant::now() < deadline, "{what}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A host bridging to a relay that speaks moq-lite-05, as the production
    /// one does, with a member and a viewer dialling both. Moving the call to
    /// the host moves the media too: the viewer gets the member, and the
    /// host's own camera, through the host rather than through the relay,
    /// which every route the host bridges would otherwise still prefer. And
    /// back again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_call_moved_to_a_host_goes_through_it_and_back() {
        let relay = Meeting::older().await;
        let shared = moq_tokio::origin::spawn();
        let camera = shared.create_broadcast(camera_path("h")).unwrap();
        camera.announce(moq_net::origin::Route::default()).unwrap();
        let _to_relay = client()
            .with_publisher(shared.consume())
            .with_subscriber(shared.clone())
            .connect(relay.url());
        let host = Meeting::sharing(shared).await;
        let host_hop = host.origin.hop();
        let relay_hop = relay.origin.hop();

        let (on_m, on_v) = (following(), following());
        let _m = member_as("m", vec![relay_at(relay.url()), host_at('h', host.url())], client(), on_m.clone());
        let v = member_as("v", vec![relay_at(relay.url()), host_at('h', host.url())], client(), on_v.clone());
        routed_through(&v.inbound, &camera_path("m"), relay_hop, "m never arrived through the relay").await;
        routed_through(&v.inbound, &camera_path("h"), relay_hop, "h never arrived through the relay").await;

        on_m.move_call(Some(id('h')), "m");
        on_v.move_call(Some(id('h')), "v");
        routed_through(&v.inbound, &camera_path("m"), host_hop, "m never came through the host").await;
        routed_through(&v.inbound, &camera_path("h"), host_hop, "h's own camera never came straight from it").await;

        on_m.move_call(None, "m");
        on_v.move_call(None, "v");
        routed_through(&v.inbound, &camera_path("m"), relay_hop, "m never went back to the relay").await;
        routed_through(&v.inbound, &camera_path("h"), relay_hop, "h never went back to the relay").await;
        drop(camera);
    }

    /// A member as a call wires one, for what travels in presence: a camera,
    /// a control broadcast the call's own announcer keeps saying, and the
    /// call's own reader of everyone else in the room, all on `local`.
    struct Talker {
        member: Member,
        local: Arc<PeerStats>,
        _control: moq_net::broadcast::Producer,
        _announcing: Abort,
        _attending: Abort,
    }

    fn talker(me: &str, targets: Vec<Target>, local: Arc<PeerStats>) -> Talker {
        let outbound = moq_tokio::origin::spawn();
        let (control, announcing) = saying(me, &outbound, &local);
        let member = member_from(me, targets, client(), local.clone(), outbound);
        let attending = attending(me, &member.inbound, &local);
        Talker {
            member,
            local,
            _control: control,
            _announcing: announcing,
            _attending: attending,
        }
    }

    /// Our control broadcast on `outbound`, kept said by the call's own
    /// announcer from `local`.
    fn saying(me: &str, outbound: &moq_net::origin::Producer, local: &Arc<PeerStats>) -> (moq_net::broadcast::Producer, Abort) {
        use super::super::{Presence, Said, announce, control_path, say, PRESENCE};
        let control = outbound.create_broadcast(control_path(me)).unwrap();
        let track = control.create_track(PRESENCE, None).unwrap();
        let presence = Arc::new(std::sync::Mutex::new(Said::new(track)));
        say(&presence, Presence::here(local));
        control.announce(moq_net::origin::Route::default()).unwrap();
        (control, Abort(tokio::spawn(announce(presence, local.clone()))))
    }

    /// The call's own reader of everyone announced at `inbound`.
    fn attending(me: &str, inbound: &moq_net::origin::Producer, local: &Arc<PeerStats>) -> Abort {
        attending_with(me, inbound, local, None)
    }

    /// The same, telling `news` of the hosts it hears.
    fn attending_with(me: &str, inbound: &moq_net::origin::Producer, local: &Arc<PeerStats>, news: Option<super::super::RoomNews>) -> Abort {
        use super::super::{Reports, Routing, attend};
        let reports = Reports {
            status: futures_channel::mpsc::unbounded().0,
            participants: futures_channel::mpsc::unbounded().0,
            hangup: futures_channel::mpsc::unbounded().0,
            metrics: Arc::new(Metrics::default()),
            in_room: true,
            rooms: super::super::super::audio::Rooms {
                local: local.clone(),
                controls: Arc::default(),
            },
            news,
            routing: Routing {
                guests: Arc::default(),
                ours: inbound.hop().id(),
                far: None,
            },
        };
        Abort(tokio::spawn(attend(inbound.consume(), me.to_string(), reports, None)))
    }

    /// Waits until `local` follows the call to `to` as chosen by `by`.
    async fn follows(local: &PeerStats, to: Option<char>, by: char, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let on = local.followed_move();
            if on.as_ref().is_some_and(|on| on.to == to.map(id) && on.by == id(by)) {
                return;
            }
            assert!(Instant::now() < deadline, "{what}; following {on:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Past the overlap a move keeps its old sessions for, so the next move
    /// starts from routes that have finished changing, as a person's does.
    async fn settle() {
        tokio::time::sleep(MOVE_OVERLAP + Duration::from_millis(500)).await;
    }

    /// A host bridging to a relay, as in a real room, with its own
    /// origin's hop: what a member's route through it ends in.
    struct Bridged {
        relay: Meeting,
        host: Meeting,
        _to_relay: moq_tokio::Connection,
    }

    async fn bridged() -> Bridged {
        bridged_to(Meeting::older().await).await
    }

    /// The same, to a given relay.
    async fn bridged_to(relay: Meeting) -> Bridged {
        let shared = moq_tokio::origin::spawn();
        let to_relay = client()
            .with_publisher(shared.consume())
            .with_subscriber(shared.clone())
            .connect(relay.url());
        let host = Meeting::sharing(shared).await;
        Bridged {
            relay,
            host,
            _to_relay: to_relay,
        }
    }

    impl Bridged {
        fn targets(&self) -> Vec<Target> {
            vec![relay_at(self.relay.url()), host_at('c', self.host.url())]
        }
    }

    /// The whole path of a move, as the app runs it: one member's choice
    /// goes out in its presence, through whichever way its media goes, and
    /// another member's reader takes it in. Back and forth, because every
    /// move changes the route the chooser's presence arrives by.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_said_by_one_member_is_followed_by_another_every_time() {
        let room = bridged().await;
        let b = talker(&id('b'), room.targets(), following());
        let a = talker(&id('a'), room.targets(), following());
        routed_through(&a.member.inbound, &super::super::control_path(&id('b')), room.relay.origin.hop(), "a never heard b").await;
        // Past the first documents, so what follows is a change heard.
        tokio::time::sleep(Duration::from_secs(1)).await;
        for round in 0..3 {
            b.local.move_call(Some(id('c')), &id('b'));
            follows(&a.local, Some('c'), 'b', &format!("a never followed b to the host, round {round}")).await;
            settle().await;
            b.local.move_call(None, &id('b'));
            follows(&a.local, None, 'b', &format!("a never followed b back to the relay, round {round}")).await;
            settle().await;
        }
    }

    /// The host itself on the call, as a hosting member is: one origin for
    /// its own broadcasts, its session to the relay and its guests, read by
    /// the call's own reader. Every move redials a member's session to it,
    /// and what the host read through the old one has to carry on.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_hosting_member_follows_every_move_said_by_a_guest() {
        let relay = Meeting::older().await;
        let shared = moq_tokio::origin::spawn();
        let on_h = following();
        let (_control, _announcing) = saying(&id('c'), &shared, &on_h);
        let _attending = attending(&id('c'), &shared, &on_h);
        let _to_relay = client()
            .with_publisher(shared.consume())
            .with_subscriber(shared.clone())
            .connect(relay.url());
        let host = Meeting::sharing(shared.clone()).await;
        let targets = || vec![relay_at(relay.url()), host_at('c', host.url())];
        let b = talker(&id('b'), targets(), following());
        routed_through(&shared, &super::super::control_path(&id('b')), relay.origin.hop(), "the host never heard b through the relay").await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        for round in 0..3 {
            b.local.move_call(Some(id('c')), &id('b'));
            follows(&on_h, Some('c'), 'b', &format!("the host never followed b to itself, round {round}")).await;
            settle().await;
            b.local.move_call(None, &id('b'));
            follows(&on_h, None, 'b', &format!("the host never followed b back to the relay, round {round}")).await;
            settle().await;
        }
    }

    /// A choice outlives whoever made it: every follower repeats it, so
    /// someone joining after the chooser has gone still hears it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_late_joiner_hears_the_move_after_its_chooser_has_left() {
        let room = bridged().await;
        let b = talker(&id('b'), room.targets(), following());
        let a = talker(&id('a'), room.targets(), following());
        b.local.move_call(Some(id('c')), &id('b'));
        follows(&a.local, Some('c'), 'b', "a never followed b").await;
        drop(b);
        let d = talker(&id('d'), room.targets(), following());
        follows(&d.local, Some('c'), 'b', "a late joiner never heard the move from those still here").await;
    }

    /// A presence read that fails while its path is still announced is
    /// asked for again: the announcement does not change, so nothing else
    /// would ever start a new reader, and what they say next is still heard.
    /// The failure here is their track aborting under a broadcast that stays;
    /// in a call it is a route going away under the read.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_presence_read_that_fails_while_still_announced_is_asked_for_again() {
        use super::super::{Asked, Presence, control_path, listen, presence_config, PRESENCE};
        let origin = moq_tokio::origin::spawn();
        let control = origin.create_broadcast(control_path(&id('b'))).unwrap();
        let mut first = moq_json::snapshot::Producer::new(control.create_track(PRESENCE, None).unwrap(), presence_config());
        first.update(&Presence::default()).unwrap();
        control.announce(moq_net::origin::Route::default()).unwrap();

        let local = following();
        let asked = Asked {
            me: id('a'),
            rooms: super::super::super::audio::Rooms {
                local: local.clone(),
                controls: Arc::default(),
            },
            seen: Arc::default(),
        };
        let broadcast = origin.consume().request_broadcast(control_path(&id('b')).as_str()).await.unwrap();
        let (saying, _said) = futures_channel::mpsc::unbounded();
        let _reading = Abort(tokio::spawn(listen(
            origin.consume(),
            broadcast,
            id('b'),
            saying,
            Arc::new(PeerStats::default()),
            asked,
            None,
        )));
        tokio::time::sleep(Duration::from_millis(300)).await;

        first.abort(moq_net::Error::Cancel).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let chooser = following();
        chooser.move_call(Some(id('c')), &id('b'));
        let mut second = moq_json::snapshot::Producer::new(control.create_track(PRESENCE, None).unwrap(), presence_config());
        second.update(&Presence::here(&chooser)).unwrap();
        follows(&local, Some('c'), 'b', "the reader never asked for b's presence again").await;
    }

    /// The relay dying moves everyone's presence to the host, and a move
    /// said after that is still heard.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_is_heard_after_the_relay_dies() {
        let room = bridged().await;
        let b = talker(&id('b'), room.targets(), following());
        let a = talker(&id('a'), room.targets(), following());
        routed_through(&a.member.inbound, &super::super::control_path(&id('b')), room.relay.origin.hop(), "a never heard b").await;
        let host_hop = room.host.origin.hop();
        room.relay.stop().await;
        routed_through(&a.member.inbound, &super::super::control_path(&id('b')), host_hop, "b's presence never came through the host").await;
        b.local.move_call(Some(id('c')), &id('b'));
        follows(&a.local, Some('c'), 'b', "a never heard b move the call once the relay was gone").await;
    }

    /// Someone who cannot reach the host the call moved to stays in the
    /// usual order, offering on the relay, and the host bridges them to
    /// whoever reaches only the host.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_member_who_cannot_reach_the_chosen_host_is_bridged_in() {
        let relay = Meeting::older().await;
        let shared = moq_tokio::origin::spawn();
        let _to_relay = client()
            .with_publisher(shared.consume())
            .with_subscriber(shared.clone())
            .connect(relay.url());
        let host = Meeting::sharing(shared.clone()).await;

        let (on_m, on_v) = (following(), following());
        on_m.move_call(Some(id('h')), "m");
        on_v.move_call(Some(id('h')), "v");
        let (unreachable, down) = (Reserved::new(), Reserved::new());
        let dialled = Instant::now();
        let mut m = member_as("m", vec![relay_at(relay.url()), host_at('h', unreachable.url())], client(), on_m);
        let v = member_as("v", vec![relay_at(down.url()), host_at('h', host.url())], client(), on_v);
        // At the relay, v arrives bridged by the host, and m straight from m.
        routed_through(&relay.origin, &camera_path("v"), shared.hop(), "v never reached the relay through the host").await;
        becomes(&relay.origin, &camera_path("m"), true).await;
        let route = route_within(&relay.origin, &camera_path("m")).await.expect("m on the relay");
        assert!(!route.hops.as_slice().contains(&shared.hop()), "m went through the host: {route:?}");
        routed_through(&v.inbound, &camera_path("m"), host.origin.hop(), "v never had m through the host").await;
        // Past the time a lost session takes to give up: m stays, and so
        // does v's way to it.
        tokio::time::sleep((dialled + GIVE_UP + Duration::from_secs(1)).saturating_duration_since(Instant::now())).await;
        assert!(m.ended.try_recv().is_err(), "m's join ended with the relay up");
        assert!(route_within(&v.inbound, &camera_path("m")).await.is_some(), "v lost m");
    }

    /// The route mark, read the way the call reads it, follows the relay: a
    /// viewer on the relay and a host has m through the relay, through the
    /// host once the relay dies, and through the relay again once it is back.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_viewer_reads_m_through_the_host_while_the_relay_is_down_and_the_relay_after() {
        use super::super::super::arrival::{self, Arrival};
        let relay = Meeting::start().await;
        let port = relay.port;
        let host = Meeting::start().await;
        let hosts = std::collections::BTreeMap::from([(host.origin.hop().id(), id('a'))]);
        let targets = || vec![relay_at(at(port)), host_at('a', host.url())];
        let _m = member_with("m", targets(), patient_client());
        let v = member_with("v", targets(), patient_client());

        let arrives = |wanted: Arrival| {
            let inbound = v.inbound.clone();
            let hosts = hosts.clone();
            async move {
                let guests = std::collections::HashSet::new();
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut last = None;
                loop {
                    if let Some(route) = route_within(&inbound, &camera_path("m")).await {
                        let hops: Vec<u64> = route.hops.as_slice().iter().map(|hop| hop.id()).collect();
                        let known = arrival::Known { theirs: None, hosts: &hosts, guests: &guests, ours: 0, far: None, who: "m", venue: &guests };
                        last = arrival::read(&hops, &known);
                        if last.as_ref() == Some(&wanted) {
                            return;
                        }
                    }
                    assert!(Instant::now() < deadline, "m never arrived {wanted:?}; last {last:?}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        };

        arrives(Arrival::Relay { from: None }).await;
        relay.stop().await;
        arrives(Arrival::Host { id: id('a'), from: None }).await;
        let deadline = Instant::now() + WITHIN;
        let _relay = loop {
            if let Some(relay) = Meeting::try_on(port, moq_tokio::origin::spawn()).await {
                break relay;
            }
            assert!(Instant::now() < deadline, "the relay's port never came free");
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        arrives(Arrival::Relay { from: None }).await;
    }

    /// A camera as the call publishes one: a catalog from the start, and the
    /// video track added to it when the camera is turned on, encoded from the
    /// test pattern on a thread of its own, as the encode thread does, which
    /// cuts a keyframe when asked.
    struct Camera {
        broadcast: moq_net::broadcast::Producer,
        catalog: super::super::super::fresh::Catalog,
        encoding: Option<Encoding>,
        keyframe: Arc<Keyframes>,
    }

    /// The encode thread, stopped and waited for when dropped.
    struct Encoding {
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for Encoding {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Small, so a debug build encodes it in time.
    const PATTERN: (u32, u32) = (320, 180);

    /// Our camera on `outbound`, switched off.
    fn camera(me: &str, outbound: &moq_net::origin::Producer) -> Camera {
        let mut broadcast = outbound.create_broadcast(camera_path(me)).unwrap();
        let catalog = super::super::super::fresh::catalog(&mut broadcast).unwrap();
        broadcast.announce(moq_net::origin::Route::default()).unwrap();
        Camera {
            broadcast,
            catalog,
            encoding: None,
            keyframe: Arc::new(Keyframes::default()),
        }
    }

    impl Camera {
        fn turn_on(&mut self) {
            use super::super::{VIDEO_PRIORITY, add_video, framerate};
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let runtime = tokio::runtime::Handle::current();
            let (broadcast, catalog, stopping) = (self.broadcast.clone(), self.catalog.clone(), stop.clone());
            let keyframe = self.keyframe.clone();
            let thread = std::thread::spawn(move || {
                let (width, height) = PATTERN;
                let mut config = moq_video::encode::Config::new(width, height, framerate());
                config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(crate::camera::bitrate_for(width, height, 30)));
                let mut producer = runtime.block_on(add_video(&broadcast, &catalog, &mut config, VIDEO_PRIORITY)).unwrap();
                let mut encoder = moq_video::encode::Encoder::new(&config).unwrap();
                let start = Instant::now();
                let every = Duration::from_secs(1) / 30;
                for n in 0u32.. {
                    if stopping.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    if let Some(wait) = (start + every * n).checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    }
                    if keyframe.camera.swap(false, std::sync::atomic::Ordering::Relaxed) {
                        encoder.cut().unwrap();
                    }
                    let frame = crate::camera::pattern::frame(width, height, n as u64, start.elapsed());
                    let surface = frame.to_i420_surface(1).unwrap();
                    let timestamp = moq_net::Timestamp::from_micros(frame.timestamp.as_micros() as u64).unwrap();
                    let encoded = encoder.encode(&moq_video::Frame::new(surface, timestamp)).unwrap();
                    if !encoded.is_empty() && producer.publish(&encoded).is_err() {
                        break;
                    }
                }
                let _ = producer.finish();
            });
            self.encoding = Some(Encoding {
                stop,
                thread: Some(thread),
            });
        }
    }

    /// When each of someone's camera frames reached its tile at a viewer,
    /// and how many times a camera tile there was taken down.
    #[derive(Clone, Default)]
    struct Arrivals(Arc<std::sync::Mutex<Vec<Instant>>>, Arc<std::sync::atomic::AtomicUsize>);

    impl Arrivals {
        fn count(&self) -> usize {
            self.0.lock().unwrap().len()
        }

        /// The camera tiles taken down: what a person sees as a leave.
        fn left(&self) -> usize {
            self.1.load(std::sync::atomic::Ordering::Relaxed)
        }

        /// Waits for `more` frames beyond those seen so far.
        async fn more(&self, more: usize, what: &str) {
            let from = self.count();
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.count() < from + more {
                assert!(Instant::now() < deadline, "{what}: {} of {more} frames", self.count() - from);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        /// The longest wait between frames from `from` on, counting the wait
        /// from `from` to the first.
        fn longest_gap(&self, from: Instant) -> Duration {
            let times = self.0.lock().unwrap();
            let mut last = from;
            let mut longest = Duration::ZERO;
            for at in times.iter().filter(|at| **at > from) {
                longest = longest.max(*at - last);
                last = *at;
            }
            longest
        }
    }

    /// The call's own reader of everyone at `inbound`, as [`attending`],
    /// counting the frames each camera tile is given.
    fn viewing(me: &str, inbound: &moq_net::origin::Producer) -> (Abort, Abort, Arrivals) {
        viewing_of(me, inbound, None)
    }

    /// The same, counting only `who`'s camera when given.
    fn viewing_of(me: &str, inbound: &moq_net::origin::Producer, who: Option<&str>) -> (Abort, Abort, Arrivals) {
        use super::super::super::{Feed, ParticipantEvent};
        use super::super::{Reports, Routing, attend};
        let local = Arc::new(PeerStats::default());
        let (participants, mut events) = futures_channel::mpsc::unbounded();
        let reports = Reports {
            status: futures_channel::mpsc::unbounded().0,
            participants,
            hangup: futures_channel::mpsc::unbounded().0,
            metrics: Arc::new(Metrics::default()),
            in_room: true,
            rooms: super::super::super::audio::Rooms {
                local,
                controls: Arc::default(),
            },
            news: None,
            routing: Routing {
                guests: Arc::default(),
                ours: inbound.hop().id(),
                far: None,
            },
        };
        let arrivals = Arrivals::default();
        let counting = arrivals.clone();
        let who = who.map(str::to_string);
        let tiles = Abort(tokio::spawn(async move {
            while let Some(event) = events.next().await {
                if let ParticipantEvent::Left { id, feed: Feed::Camera } = &event {
                    if who.as_ref().is_none_or(|who| who == id) {
                        counting.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    continue;
                }
                if let ParticipantEvent::Joined { id, slot, feed: Feed::Camera, .. } = event {
                    if who.as_ref().is_some_and(|who| *who != id) {
                        continue;
                    }
                    let counting = counting.clone();
                    let seen = Arc::new(std::sync::atomic::AtomicU64::new(slot.published()));
                    let watched = slot.clone();
                    slot.watch(Arc::new(move || {
                        let now = watched.published();
                        if seen.swap(now, std::sync::atomic::Ordering::Relaxed) != now {
                            counting.0.lock().unwrap().push(Instant::now());
                        }
                    }));
                }
            }
        }));
        let reading = Abort(tokio::spawn(attend(inbound.consume(), me.to_string(), reports, None)));
        (reading, tiles, arrivals)
    }

    /// A relay a member reaches on a fixed port, so it can be stopped and
    /// brought back where members are already dialling it.
    async fn relay_again(port: u16) -> Meeting {
        let deadline = Instant::now() + WITHIN;
        loop {
            if let Some(relay) = Meeting::try_on(port, moq_tokio::origin::spawn()).await {
                return relay;
            }
            assert!(Instant::now() < deadline, "the relay's port never came free");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A member with a camera of its own, and a viewer reading it the way
    /// the call does, both on the relay and a host.
    struct Watched {
        camera: Camera,
        arrivals: Arrivals,
        _m: Member,
        _v: Member,
        _reading: Abort,
        _tiles: Abort,
    }

    fn watched(targets: impl Fn() -> Vec<Target>) -> Watched {
        let outbound = moq_tokio::origin::spawn();
        let camera = camera("m", &outbound);
        let m = joined("m", targets(), patient_client(), Arc::new(PeerStats::default()), outbound);
        let v = member_with("v", targets(), patient_client());
        let (reading, tiles, arrivals) = viewing("v", &v.inbound);
        Watched {
            camera,
            arrivals,
            _m: m,
            _v: v,
            _reading: reading,
            _tiles: tiles,
        }
    }

    /// Someone whose camera is off when their route changes, and who turns it
    /// on after: the viewer gets the video. The route changes because the
    /// relay goes, and the camera's broadcast moves to the host.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_camera_turned_on_after_its_route_changed_is_seen() {
        let relay = Meeting::start().await;
        let port = relay.port;
        let host = Meeting::start().await;
        let host_url = host.url();
        let mut w = watched(|| vec![relay_at(at(port)), host_at('a', host_url.clone())]);
        becomes(&w._v.inbound, &camera_path("m"), true).await;
        // The viewer's readers are up and waiting for video.
        tokio::time::sleep(Duration::from_millis(500)).await;
        relay.stop().await;
        routed_through(&w._v.inbound, &camera_path("m"), host.origin.hop(), "m never came through the host").await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        w.camera.turn_on();
        w.arrivals.more(30, "the viewer never saw the camera turned on after the route changed").await;
    }

    /// Video flowing through the relay keeps flowing when the relay stops and
    /// a host is there, and again when the relay comes back and the camera
    /// moves back up to it. Each gap is bounded by how long the change takes
    /// to be noticed and served again, not by anything waiting on a person.
    #[tokio::test(flavor = "multi_thread")]
    async fn video_keeps_arriving_when_the_relay_stops_and_when_it_comes_back() {
        let relay = Meeting::start().await;
        let port = relay.port;
        let host = Meeting::start().await;
        let host_url = host.url();
        let mut w = watched(|| vec![relay_at(at(port)), host_at('a', host_url.clone())]);
        w.camera.turn_on();
        w.arrivals.more(30, "no video through the relay").await;

        let stopped = Instant::now();
        relay.stop().await;
        routed_through(&w._v.inbound, &camera_path("m"), host.origin.hop(), "m never came through the host").await;
        w.arrivals.more(60, "video stopped when the relay did").await;
        let down = w.arrivals.longest_gap(stopped);

        let relay = relay_again(port).await;
        // Back on the relay once it has held, then past the overlap.
        routed_through(&w._v.inbound, &camera_path("m"), relay.origin.hop(), "m never went back to the relay").await;
        let back = Instant::now();
        tokio::time::sleep(MOVE_OVERLAP + Duration::from_secs(1)).await;
        w.arrivals.more(30, "video stopped when the relay came back").await;
        let up = w.arrivals.longest_gap(back);
        assert!(down < GAP, "video was gone {down:?} when the relay stopped");
        assert!(up < GAP, "video was gone {up:?} when the relay came back");
    }

    /// The longest a viewer may go without a frame across a change of route.
    /// A read asked for again starts at the newest group, so the bound is a
    /// round trip or two plus the route settling, and at worst a group (the
    /// encoder's keyframe interval, two seconds) where something still
    /// carries a read across: about 2 s was measured for a relay stopping, under
    /// 1 s for one coming back, and 0.4 to 2 s for each move of the call.
    /// Before a lost read was asked for again, the gap after a move had no
    /// bound at all.
    const GAP: Duration = Duration::from_millis(3500);

    /// A member and a viewer in a room a host bridges to the relay, both
    /// following moves of the call, the viewer reading the member's camera
    /// the way the call does.
    struct Moving {
        room: Bridged,
        camera: Camera,
        arrivals: Arrivals,
        on_m: Arc<PeerStats>,
        on_v: Arc<PeerStats>,
        v: Member,
        _m: Member,
        _reading: Abort,
        _tiles: Abort,
    }

    async fn moving() -> Moving {
        moving_on(Meeting::older().await, true).await
    }

    /// The same on a given relay, with the member's spare offer on or off.
    async fn moving_on(relay: Meeting, spare: bool) -> Moving {
        let room = bridged_to(relay).await;
        let (on_m, on_v) = (following(), following());
        on_m.set_spare_route(spare);
        let outbound = moq_tokio::origin::spawn();
        let camera = camera("m", &outbound);
        let m = joined_asking("m", room.targets(), client(), on_m.clone(), outbound, camera.keyframe.clone());
        let v = member_as("v", room.targets(), client(), on_v.clone());
        let (reading, tiles, arrivals) = viewing("v", &v.inbound);
        routed_through(&v.inbound, &camera_path("m"), room.relay.origin.hop(), "m never arrived through the relay").await;
        Moving {
            room,
            camera,
            arrivals,
            on_m,
            on_v,
            v,
            _m: m,
            _reading: reading,
            _tiles: tiles,
        }
    }

    impl Moving {
        /// Moves the call to the host (`Some`) or back to the relay, waits
        /// until the viewer has m that way, and past the overlap.
        async fn to(&self, host: bool, what: &str) {
            let (to, through) = match host {
                true => (Some(id('c')), self.room.host.origin.hop()),
                false => (None, self.room.relay.origin.hop()),
            };
            self.on_m.move_call(to.clone(), "m");
            self.on_v.move_call(to, "v");
            routed_through(&self.v.inbound, &camera_path("m"), through, what).await;
            settle().await;
        }
    }

    /// The fault as it was seen: a camera off while the call moves to a host
    /// and back. Moving back cuts the viewer's catalog read off ("dropped")
    /// with m still announced, and nothing used to read it again, so the
    /// camera turned on after stayed blank for the rest of the call.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_camera_turned_on_after_the_call_moved_is_seen() {
        let mut w = moving().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        for round in 0..2 {
            w.to(true, &format!("m never came through the host, round {round}")).await;
            w.to(false, &format!("m never went back to the relay, round {round}")).await;
        }
        w.camera.turn_on();
        w.arrivals.more(30, "the viewer never saw the camera turned on after the call moved").await;
    }

    /// Video keeps arriving across every move of the call, there and back.
    #[tokio::test(flavor = "multi_thread")]
    async fn video_keeps_arriving_across_every_move_of_the_call() {
        let mut w = moving().await;
        w.camera.turn_on();
        w.arrivals.more(30, "no video through the relay").await;
        for round in 0..2 {
            for host in [true, false] {
                let moved = Instant::now();
                w.to(host, &format!("the call never moved, round {round}")).await;
                w.arrivals.more(30, &format!("video stopped after a move, round {round}")).await;
                let gap = w.arrivals.longest_gap(moved);
                assert!(gap < GAP, "video was gone {gap:?} moving to the {}, round {round}", if host { "host" } else { "relay" });
            }
        }
    }

    /// Counts the retractions of `path` at `origin` from now on: what makes
    /// a tile leave.
    fn retractions(origin: &moq_net::origin::Producer, path: &str) -> (Abort, Arc<std::sync::atomic::AtomicUsize>) {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counting = count.clone();
        let mut announced = origin.consume().announced();
        let path = path.to_string();
        let watching = Abort(tokio::spawn(async move {
            while let Some(update) = announced.next().await {
                if update.prefix.as_str() == path && !update.kind.is_active() {
                    counting.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }));
        (watching, count)
    }

    fn count(of: &std::sync::atomic::AtomicUsize) -> usize {
        of.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Waits out the attendance loop's grace from `from`, the last moment a
    /// withdrawal could have happened, with a margin: a withdrawal that was
    /// going to be a leave has been told by then, so asserting no leave
    /// earlier would race the very failure it is there to catch.
    async fn past_the_grace(from: Instant) {
        tokio::time::sleep_until((from + super::super::super::grace::GRACE + Duration::from_millis(500)).into()).await;
    }

    /// The longest a viewer may go without a frame when a spare takes over
    /// from a carrier that stopped cleanly. The library ends the viewer's
    /// read as soon as the carrier's route goes and bounces the path through
    /// the spare; the attendance loop asks again at once, and the fresh read
    /// starts at the newest group, which the member's own move to the spare
    /// has just begun with a keyframe, as the call's does. The bound leaves
    /// room for a debug build on a busy machine. Without a spare the same
    /// stop usually takes 2 s: a retraction, the library's hold, and the
    /// media asked for again.
    ///
    /// When the library carried the read across instead, this failed now
    /// and then: the viewer's track was handed the group in flight twice,
    /// once from each copy, and moq-mux's reader then dropped the next group
    /// whole as "old", a freeze of 1 to 2 s. A fresh read has one copy.
    const SPLICE: Duration = Duration::from_millis(500);

    /// The same for media whose sender is not asked for a keyframe when the
    /// route changes: the rest of the group in flight, at most the encoder's
    /// two-second keyframe interval, and a margin.
    const GROUP: Duration = Duration::from_millis(2500);

    /// A clean relay stop with a host there: the viewer's read of the member
    /// is bounced to the member's spare on the host. The path is withdrawn
    /// and announced again at once, so no tile leaves, and there is no wait
    /// for the library's hold.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_relay_that_stops_is_a_bounce_to_the_spare_not_a_leave() {
        let mut w = moving().await;
        w.camera.turn_on();
        w.arrivals.more(30, "no video through the relay").await;
        let (_watching, retracted) = retractions(&w.v.inbound, &camera_path("m"));
        let stopped = Instant::now();
        w.room.relay.stop().await;
        routed_through(&w.v.inbound, &camera_path("m"), w.room.host.origin.hop(), "m never came through the host").await;
        let flipped = Instant::now();
        w.arrivals.more(75, "video stopped when the relay did").await;
        let gap = w.arrivals.longest_gap(stopped);
        // Past the grace on the withdrawal, so a leave would have shown.
        past_the_grace(flipped).await;
        w.arrivals.more(10, "video stopped after the relay did").await;
        assert_eq!(w.arrivals.left(), 0, "the viewer's tile for m left");
        eprintln!("relay stopped: gap {gap:?}, {} withdrawals", count(&retracted));
        assert!(gap < SPLICE, "video was gone {gap:?} when the relay stopped");
    }

    /// The control: with the spare switched off, the host holds m only
    /// through the relay, so once the relay is gone nothing there could carry
    /// m on and the test above could not pass. Cutting the host's own link to
    /// the relay, and nothing of m's, takes m from the host.
    ///
    /// Not the same stop seen as a leave, as this was first written: without a
    /// spare that stop is usually a leave, but not always. m's media moves to
    /// the host as soon as m hears the relay go, and when its new offer reaches
    /// the host before the host has heard the same news for itself, the host's
    /// route to m changes rather than ending, and the viewer is bounced to it
    /// with no gap in the host's own routes (seen once in about twenty runs of the whole suite
    /// on a loaded machine). That is the app doing well by luck, not a fault,
    /// and a control that needs it not to happen fails at random. Nor the
    /// converse with the spare on: nothing outside m shows when its spare has
    /// reached the host, so a cut can beat it there.
    #[tokio::test(flavor = "multi_thread")]
    async fn without_a_spare_the_host_holds_a_member_only_through_the_relay() {
        let w = moving_on(Meeting::older().await, false).await;
        let path = camera_path("m");
        becomes(&w.room.host.origin, &path, true).await;
        let (_watching, retracted) = retractions(&w.room.host.origin, &path);
        w.room._to_relay.abort(moq_net::Error::Cancel);
        let deadline = Instant::now() + WITHIN;
        while count(&retracted) == 0 {
            assert!(Instant::now() < deadline, "the host kept m with no spare and no relay");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Moving the call to a host and back, on a relay that speaks moq-lite-05
    /// (which holds no spare while the call is on the host) and on one that
    /// carries a price (which does): never a leave, and each gap bounded.
    async fn moves_are_never_a_leave(relay: Meeting) {
        let mut w = moving_on(relay, true).await;
        w.camera.turn_on();
        w.arrivals.more(30, "no video through the relay").await;
        let (_watching, retracted) = retractions(&w.v.inbound, &camera_path("m"));
        let mut settled = Instant::now();
        for round in 0..2 {
            for host in [true, false] {
                let moved = Instant::now();
                w.to(host, &format!("the call never moved, round {round}")).await;
                settled = Instant::now();
                w.arrivals.more(30, &format!("video stopped after a move, round {round}")).await;
                let gap = w.arrivals.longest_gap(moved);
                eprintln!("moved to the {}, round {round}: gap {gap:?}", if host { "host" } else { "relay" });
                assert!(gap < GAP, "video was gone {gap:?} moving to the {}, round {round}", if host { "host" } else { "relay" });
            }
        }
        // The last move's old offer goes a moment after it settles; earlier
        // moves' graces ran out during the moves after them.
        past_the_grace(settled).await;
        w.arrivals.more(10, "video stopped after the last move").await;
        eprintln!("moves: {} withdrawals", count(&retracted));
        assert_eq!(w.arrivals.left(), 0, "the viewer's tile for m left across a move");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_call_moved_to_a_host_and_back_is_never_a_leave() {
        moves_are_never_a_leave(Meeting::older().await).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_call_moved_to_a_host_and_back_over_a_priced_relay_is_never_a_leave() {
        moves_are_never_a_leave(Meeting::start().await).await;
    }

    /// A viewer that hangs up its own session to the relay, with the relay
    /// still up and a host bridging it, keeps reading m: what came through
    /// that session goes as a lost session's routes do, never as a
    /// withdrawal of ours, and the read is bounced to the route through the
    /// host, no tile leaving. What leaving a quiet relay from our side relies
    /// on.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_relay_session_hung_up_here_bounces_the_viewers_read_to_the_host() {
        let room = bridged().await;
        let outbound = moq_tokio::origin::spawn();
        let mut theirs = camera("m", &outbound);
        let _m = joined_asking("m", room.targets(), client(), Arc::new(PeerStats::default()), outbound, theirs.keyframe.clone());
        let inbound = moq_tokio::origin::spawn();
        let nothing = moq_tokio::origin::spawn();
        let to_relay = client()
            .with_publisher(nothing.consume())
            .with_subscriber(inbound.clone())
            .connect(room.relay.url());
        let _to_host = client()
            .with_cost(HOST_COST)
            .with_publisher(nothing.consume())
            .with_subscriber(inbound.clone())
            .connect(room.host.url());
        let (_reading, _tiles, arrivals) = viewing("v", &inbound);
        theirs.turn_on();
        arrivals.more(30, "no video through the relay").await;
        routed_through(&inbound, &camera_path("m"), room.relay.origin.hop(), "m never arrived through the relay").await;
        // The host's route has had time to arrive beside it.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (_watching, retracted) = retractions(&inbound, &camera_path("m"));
        let cut = Instant::now();
        to_relay.abort(moq_net::Error::Cancel);
        routed_through(&inbound, &camera_path("m"), room.host.origin.hop(), "m never came through the host").await;
        let flipped = Instant::now();
        arrivals.more(75, "video stopped when we hung up on the relay").await;
        let gap = arrivals.longest_gap(cut);
        past_the_grace(flipped).await;
        arrivals.more(10, "video stopped after we hung up on the relay").await;
        assert_eq!(arrivals.left(), 0, "the viewer's tile for m left");
        eprintln!("hung up: {} withdrawals", count(&retracted));
        // Nobody asks m for a keyframe here: the read picks up at m's next group.
        assert!(gap < GROUP, "video was gone {gap:?}");
        eprintln!("hung up: gap {gap:?}");
    }

    /// A network path between members and a server, over UDP, that can fail:
    /// it forwards everything, drops everything, or drops a share of the
    /// packets, both ways, as `loss` (a percentage) says from one packet to
    /// the next. Members dial its port instead of the server's.
    struct Path {
        port: u16,
        loss: Arc<std::sync::atomic::AtomicU8>,
        _forwarding: Abort,
    }

    /// Every packet through.
    const FORWARD: u8 = 0;
    /// Nothing through: a server that died without a word, as far as anyone
    /// on this path can tell.
    const DROP: u8 = 100;

    impl Path {
        async fn to(server: u16) -> Path {
            use tokio::net::UdpSocket;
            let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
            let port = socket.local_addr().unwrap().port();
            let loss = Arc::new(std::sync::atomic::AtomicU8::new(FORWARD));
            let dropping = loss.clone();
            let forwarding = Abort(tokio::spawn(async move {
                // One upstream socket per client, so the server tells them apart.
                let mut clients: std::collections::HashMap<std::net::SocketAddr, (Arc<UdpSocket>, Abort)> =
                    std::collections::HashMap::new();
                let mut buffer = vec![0u8; 65_536];
                let mut seed = 0x9e37_79b9_7f4a_7c15u64;
                loop {
                    let Ok((length, from)) = socket.recv_from(&mut buffer).await else { continue };
                    if let std::collections::hash_map::Entry::Vacant(slot) = clients.entry(from) {
                        let upstream = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
                        upstream.connect(("127.0.0.1", server)).await.unwrap();
                        let back = upstream.clone();
                        let socket = socket.clone();
                        let dropping = dropping.clone();
                        let returning = Abort(tokio::spawn(async move {
                            let mut buffer = vec![0u8; 65_536];
                            let mut seed = from.port() as u64 | 1;
                            loop {
                                let Ok(length) = back.recv(&mut buffer).await else {
                                    tokio::time::sleep(Duration::from_millis(5)).await;
                                    continue;
                                };
                                if passes(&dropping, &mut seed) {
                                    let _ = socket.send_to(&buffer[..length], from).await;
                                }
                            }
                        }));
                        slot.insert((upstream, returning));
                    }
                    if passes(&dropping, &mut seed) {
                        let (upstream, _) = &clients[&from];
                        let _ = upstream.send(&buffer[..length]).await;
                    }
                }
            }));
            Path {
                port,
                loss,
                _forwarding: forwarding,
            }
        }

        fn url(&self) -> url::Url {
            at(self.port)
        }

        fn set(&self, loss: u8) {
            self.loss.store(loss, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Whether one packet gets through, by a cheap generator of our own.
    fn passes(loss: &std::sync::atomic::AtomicU8, seed: &mut u64) -> bool {
        let loss = loss.load(std::sync::atomic::Ordering::Relaxed);
        if loss == FORWARD {
            return true;
        }
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed % 100) as u8 >= loss
    }

    /// A member hosting the room, as the app runs one: one origin for its
    /// own broadcasts, its guests and its join; a server of its own its
    /// guests reach it on, noting when their sessions hear anything; and its
    /// join on the relay, hosting.
    struct Hosted {
        origin: moq_net::origin::Producer,
        port: u16,
        local: Arc<PeerStats>,
        _accepting: Abort,
        _joined: Abort,
        learn: futures_channel::mpsc::UnboundedSender<Learned>,
    }

    async fn hosted(relay: url::Url) -> Hosted {
        hosted_as('c', vec![relay_at(relay)]).await
    }

    /// A member hosting the room as `me`, joined to `targets`, the first of
    /// which is the relay.
    async fn hosted_as(me: char, targets: Vec<Target>) -> Hosted {
        hosted_behind(me, targets, None).await
    }

    /// The same, letting in only guests who present `door`'s, when given.
    async fn hosted_behind(me: char, targets: Vec<Target>, door: Option<door::Hosting>) -> Hosted {
        crypto();
        let shared = moq_tokio::origin::spawn();
        let mut config = moq_tokio::server::Config::default();
        config.listen.bind = Some(moq_tokio::listen::Bind::Addr(([127, 0, 0, 1], 0).into()));
        config.listen.tls.generate = vec!["localhost".to_string()];
        config.publisher = Some(shared.consume());
        config.subscriber = Some(shared.clone());
        let server = config.init().unwrap().bind().await.unwrap();
        let port = server.local_addr().unwrap().port();
        let heard = Arc::new(quiet::Heard::default());
        let estimates = adapt::Estimates::new(moq_net::bandwidth::Producer::new());
        let offer = match &door {
            Some(door) => door.offer(shared.hop().id()),
            None => door::Hosting::open("team".to_string()).unwrap().offer(shared.hop().id()),
        };
        let (accepting, _) = accept(server, door, Some(estimates), Arc::new(Metrics::default()), None, None, Some(heard.clone()));
        let local = Arc::new(PeerStats::default());
        local.set_offer(Some(offer));
        let (learn, learned) = futures_channel::mpsc::unbounded();
        let host = join(Join {
            client: client(),
            me: id(me),
            targets,
            outbound: shared.clone(),
            inbound: shared.clone(),
            hosting: true,
            estimate: moq_net::bandwidth::Producer::new(),
            guests: None,
            heard: Some(heard),
            metrics: Arc::new(Metrics::default()),
            local: local.clone(),
            status: futures_channel::mpsc::unbounded().0,
            keyframe: Arc::new(Keyframes::default()),
            room: "room team".to_string(),
            bounce: None,
            learned,
        });
        let joined = Abort(tokio::spawn(async move {
            let _ = host.closed.await;
        }));
        let deadline = Instant::now() + WITHIN;
        while !sending_through_relay(&local) {
            assert!(Instant::now() < deadline, "the host never joined the relay");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Hosted {
            origin: shared,
            port,
            local,
            _accepting: accepting,
            _joined: joined,
            learn,
        }
    }

    /// A room whose relay and host the members reach only through paths
    /// that can fail: the relay behind one, the host (hosting, its own join
    /// on the relay through that same path) behind another. m publishes a
    /// camera that cuts a keyframe when its media moves, as the call's does;
    /// v reads it as the call does, through the relay, with m's spare on the
    /// host.
    struct Behind {
        relay: Meeting,
        host: Hosted,
        to_relay: Path,
        to_host: Path,
        arrivals: Arrivals,
        on_m: Arc<PeerStats>,
        m: Member,
        v: Member,
        _camera: Camera,
        _reading: Abort,
        _tiles: Abort,
    }

    async fn behind() -> Behind {
        let relay = Meeting::older().await;
        let to_relay = Path::to(relay.port).await;
        let host = hosted(to_relay.url()).await;
        let to_host = Path::to(host.port).await;
        let targets = || vec![relay_at(to_relay.url()), host_at('c', to_host.url())];
        let on_m = Arc::new(PeerStats::default());
        let outbound = moq_tokio::origin::spawn();
        let mut camera = camera("m", &outbound);
        let m = joined_asking("m", targets(), client(), on_m.clone(), outbound, camera.keyframe.clone());
        let v = joined("v", targets(), client(), Arc::new(PeerStats::default()), moq_tokio::origin::spawn());
        let (reading, tiles, arrivals) = viewing("v", &v.inbound);
        camera.turn_on();
        arrivals.more(30, "no video through the relay").await;
        routed_through(&v.inbound, &camera_path("m"), relay.origin.hop(), "m never arrived through the relay").await;
        // The spare is offered, and every route has arrived and been ranked.
        tokio::time::sleep(Duration::from_secs(1)).await;
        Behind {
            relay,
            host,
            to_relay,
            to_host,
            arrivals,
            on_m,
            m,
            v,
            _camera: camera,
            _reading: reading,
            _tiles: tiles,
        }
    }

    /// Whether `member` has said, since this was last asked, that it is no
    /// longer on the relay: what leaving it shows as.
    fn left_the_relay(member: &mut Member) -> bool {
        let mut left = false;
        while let Ok(status) = member.statuses.try_recv() {
            if let RemoteStatus::InRoom { via: Some(via), .. } = status {
                left |= !via.relay;
            }
        }
        left
    }

    fn sending_through_relay(local: &PeerStats) -> bool {
        local.sending() == Some(super::super::super::arrival::Sending::Relay)
    }

    /// The longest a viewer may go without a frame when the relay goes
    /// silent and a host is there. Everything then comes through the host,
    /// which leaves the relay at its own quiet limit, [`quiet::HOSTING_EXTRA`]
    /// after its members, within a tick of the join and a stale sample; its
    /// read of m is then bounced to m directly and read again from m's newest
    /// group.
    /// Measured 2.6 to 3.6 s over twenty runs (about half a second less with
    /// no extra second for the host); the bound leaves a margin for a debug
    /// build on a busy machine.
    const QUIET_GAP: Duration = Duration::from_millis(
        (quiet::QUIET_QUIC.as_millis() + quiet::HOSTING_EXTRA.as_millis()) as u64 + 1500,
    );

    /// The relay stops answering (killed, frozen, or its network gone), and
    /// nobody's session to it has ended. Everyone still hears the host, so
    /// each member, and the host, leaves the relay at its quiet limit rather
    /// than its idle timeout, and the viewer's read is bounced to the host: no
    /// leave, and a gap of about that limit.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_relay_that_goes_silent_is_left_and_the_video_comes_through_the_host() {
        let r = behind().await;
        let (_watching, retracted) = retractions(&r.v.inbound, &camera_path("m"));
        let cut = Instant::now();
        r.to_relay.set(DROP);
        routed_through(&r.v.inbound, &camera_path("m"), r.host.origin.hop(), "m never came through the host").await;
        let flipped = Instant::now();
        r.arrivals.more(60, "video stopped when the relay went silent").await;
        let gap = r.arrivals.longest_gap(cut);
        past_the_grace(flipped).await;
        r.arrivals.more(10, "video stopped after the relay went silent").await;
        assert_eq!(r.arrivals.left(), 0, "the viewer's tile for m left");
        eprintln!("silent relay: {} withdrawals", count(&retracted));
        assert_eq!(r.on_m.sending(), Some(super::super::super::arrival::Sending::Host(id('c'))), "m's media never left the relay");
        assert_eq!(r.host.local.sending(), Some(super::super::super::arrival::Sending::Guests), "the host never left the relay");
        assert!(gap < QUIET_GAP, "video was gone {gap:?} when the relay went silent");
        eprintln!("silent relay: gap {gap:?}");
    }

    /// Watches `local`'s carrier for `over`, and says whether it ever left
    /// the relay.
    async fn ever_left(local: &PeerStats, over: Duration) -> bool {
        let until = Instant::now() + over;
        let mut left = false;
        while Instant::now() < until {
            left |= !sending_through_relay(local);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        left
    }

    /// Our own link stalls: every session is silent at once, for longer than
    /// the quiet limit, then everything comes back. Nothing is left, and our
    /// media never moves: nothing was heard to say the relay was the fault.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_link_that_stalls_everywhere_and_comes_back_leaves_nothing() {
        let mut r = behind().await;
        left_the_relay(&mut r.m);
        left_the_relay(&mut r.v);
        let (_watching, lost) = retractions(&r.relay.origin, &camera_path("m"));
        r.to_relay.set(DROP);
        r.to_host.set(DROP);
        let stalled = ever_left(&r.on_m, Duration::from_millis(2500)).await;
        r.to_relay.set(FORWARD);
        r.to_host.set(FORWARD);
        let moved = ever_left(&r.on_m, Duration::from_secs(5)).await;
        assert!(!stalled && !moved, "m's media left the relay");
        assert!(!left_the_relay(&mut r.m), "m left the relay");
        assert!(!left_the_relay(&mut r.v), "v left the relay");
        assert_eq!(count(&lost), 0, "the relay lost m");
        r.arrivals.more(30, "video never came back").await;
        let route = route_within(&r.v.inbound, &camera_path("m")).await.expect("m is there");
        assert_eq!(route.hops.as_slice().last(), Some(&r.relay.origin.hop()), "{route:?}");
    }

    /// A bad path to the relay, losing a third of its packets for five
    /// seconds, is not a silent one while it carries a call: nothing is left.
    ///
    /// The host watches m through the relay, as a hosting member on a call
    /// does. Without that its session to the relay carries only keep-alives
    /// and their acknowledgements, a few packets a second, and QUIC sends a
    /// lost one again on a doubling timer, so three seconds hold only about
    /// five answers: at a third lost each way, all of them now and then are,
    /// and that session really does hear nothing for its limit (measured:
    /// 3.36 s, five answers in a row dropped). Leaving it then is the rule
    /// doing what it says. A session carrying video hears dozens of packets
    /// a second, and the same loss never silences it for a second.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_lossy_relay_path_is_not_left() {
        let mut r = behind().await;
        let (_host_reading, _host_tiles, host_sees) = viewing_of(&id('c'), &r.host.origin, Some("m"));
        host_sees.more(30, "the host saw no video from m").await;
        routed_through(&r.host.origin, &camera_path("m"), r.relay.origin.hop(), "the host never had m through the relay").await;
        left_the_relay(&mut r.m);
        left_the_relay(&mut r.v);
        let (_watching, lost) = retractions(&r.relay.origin, &camera_path("m"));
        r.to_relay.set(30);
        let lossy = ever_left(&r.on_m, Duration::from_secs(5)).await;
        r.to_relay.set(FORWARD);
        let after = ever_left(&r.on_m, Duration::from_secs(1)).await;
        assert!(!lossy && !after, "m's media left the relay");
        assert!(!left_the_relay(&mut r.m), "m left the relay");
        assert!(!left_the_relay(&mut r.v), "v left the relay");
        assert!(sending_through_relay(&r.host.local), "the host left the relay");
        assert_eq!(count(&lost), 0, "the relay lost m");
        r.arrivals.more(30, "video never came back").await;
    }

    /// How long the relay below gives its sessions once it says it is going.
    const DRAIN: Duration = Duration::from_secs(3);

    /// A relay that says it is going away, as relay 0.17 does when it is
    /// stopped: our media moves to the host while the relay still serves,
    /// so the viewer sees no gap across the relay's deadline.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_relay_going_away_moves_our_media_before_it_closes() {
        let relay = Meeting::start().await;
        let host = hosted(relay.url()).await;
        let targets = || vec![relay_at(relay.url()), host_at('c', at(host.port))];
        let on_m = Arc::new(PeerStats::default());
        let outbound = moq_tokio::origin::spawn();
        let mut camera = camera("m", &outbound);
        let _m = joined_asking("m", targets(), client(), on_m.clone(), outbound, camera.keyframe.clone());
        let v = joined("v", targets(), client(), Arc::new(PeerStats::default()), moq_tokio::origin::spawn());
        let (_reading, _tiles, arrivals) = viewing("v", &v.inbound);
        camera.turn_on();
        arrivals.more(30, "no video through the relay").await;
        routed_through(&v.inbound, &camera_path("m"), relay.origin.hop(), "m never arrived through the relay").await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (_watching, retracted) = retractions(&v.inbound, &camera_path("m"));
        let said = Instant::now();
        for session in relay.sessions.lock().unwrap().iter() {
            let _ = session.drain().send(moq_net::goaway::Goaway::new().with_timeout(DRAIN));
        }
        while sending_through_relay(&on_m) {
            assert!(said.elapsed() < Duration::from_millis(500), "m's media was still on the relay {:?} after it said it was going", said.elapsed());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let moved = said.elapsed();
        tokio::time::sleep_until((said + DRAIN + Duration::from_secs(2)).into()).await;
        let gap = arrivals.longest_gap(said);
        // The relay closes at its deadline, the last withdrawal there is.
        past_the_grace(said + DRAIN).await;
        arrivals.more(10, "video stopped after the relay went").await;
        assert_eq!(arrivals.left(), 0, "the viewer's tile for m left");
        eprintln!("going away: {} withdrawals", count(&retracted));
        assert!(gap < Duration::from_secs(1), "video was gone {gap:?} across the relay going away");
        eprintln!("going away: moved in {moved:?}, gap {gap:?}");
    }

    /// The host itself: its relay goes silent while its guests are heard,
    /// and it leaves the relay too, a second after its members do, and
    /// carries on hosting.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_leaves_a_silent_relay_while_its_guests_are_heard() {
        let relay = Meeting::older().await;
        let to_relay = Path::to(relay.port).await;
        let host = hosted(to_relay.url()).await;
        let _m = member("m", vec![relay_at(to_relay.url()), host_at('c', at(host.port))]);
        becomes(&host.origin, &camera_path("m"), true).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let cut = Instant::now();
        to_relay.set(DROP);
        let deadline = cut + Duration::from_secs(8);
        while host.local.sending() != Some(super::super::super::arrival::Sending::Guests) {
            assert!(Instant::now() < deadline, "the host never left the silent relay");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Measured from the cut; the host's last byte from an idle relay
        // came up to a keep-alive before it.
        let took = cut.elapsed();
        let limit = quiet::QUIET_QUIC + quiet::HOSTING_EXTRA;
        assert!(took >= limit - quiet::KEEP_ALIVE - Duration::from_millis(250), "the host left after only {took:?}");
        assert!(took < limit + Duration::from_millis(1500), "the host took {took:?} to leave");
        eprintln!("host left the relay after {took:?}");
    }

    /// The longest a frame may be late while nothing that carries it changes:
    /// a frame is due every 33 ms, and a debug build on a busy machine is
    /// late now and then.
    const STEADY: Duration = Duration::from_millis(500);

    /// The host goes while the relay carries everyone: the member's spare on
    /// it closes, and viewers, who never used it, see nothing change.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_that_stops_while_the_relay_carries_changes_nothing_for_viewers() {
        let mut w = moving().await;
        w.camera.turn_on();
        w.arrivals.more(30, "no video through the relay").await;
        let relay_hop = w.room.relay.origin.hop();
        let (_watching, retracted) = retractions(&w.v.inbound, &camera_path("m"));
        let stopped = Instant::now();
        drop(w.room._to_relay);
        w.room.host.stop().await;
        w.arrivals.more(60, "video stopped when the host did").await;
        let gap = w.arrivals.longest_gap(stopped);
        assert_eq!(count(&retracted), 0, "the viewer lost m");
        assert!(gap < STEADY, "video was gone {gap:?} when an unused host stopped");
        let route = route_within(&w.v.inbound, &camera_path("m")).await.expect("m is still there");
        assert_eq!(route.hops.as_slice().last(), Some(&relay_hop), "{route:?}");
    }

    /// Reads `path`'s video at `origin` for as long as it is there, asking
    /// again when a read ends, as a viewer does.
    fn reading(origin: &moq_net::origin::Producer, path: String) -> Abort {
        let consumer = origin.consume();
        Abort(tokio::spawn(async move {
            loop {
                if let Ok(broadcast) = consumer.request_broadcast(path.as_str()).await {
                    if let Ok(track) = broadcast.track("video") {
                        if let Ok(mut subscriber) = track.subscribe(None).await {
                            while let Ok(Some(mut group)) = subscriber.recv_group().await {
                                while let Ok(Some(_)) = group.read_frame().await {}
                            }
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }))
    }

    /// Bytes a second a host receives over every session it accepted,
    /// measured over `over`: what its guests upload to it.
    async fn received(host: &Meeting, over: Duration) -> u64 {
        rate(over, || host.sessions.lock().unwrap().iter().map(|session| session.stats().bytes_received.unwrap_or(0)).sum()).await
    }

    /// Bytes a second a connection sends, measured over `over`.
    async fn sent(connection: &moq_tokio::Connection, over: Duration) -> u64 {
        let monitor = connection.monitor();
        rate(over, || monitor.stats().and_then(|stats| stats.bytes_sent).unwrap_or(0)).await
    }

    /// How often a session's counters are sampled while someone reads them
    /// (moq-net's `POLL_INTERVAL`, read in its source), and a margin.
    const SAMPLED: Duration = Duration::from_millis(150);

    /// The rate of a byte counter over `over`.
    ///
    /// **A session's counters are a sample, taken when they are asked for.**
    /// A read returns the last sample and asks for the next, and sampling
    /// pauses once nobody has asked for a while. So the first read after a
    /// pause is as old as the last read before it: read once at each end, a
    /// window measured what the counters did since the previous measurement,
    /// seconds earlier, rather than over `over`. After a relay stop that
    /// counted the half-second before the spare carried, and the rate came out
    /// at half the stream. Asked for throughout instead, both ends are at most
    /// one sample old.
    async fn rate(over: Duration, total: impl Fn() -> u64) -> u64 {
        let ask = |until: Instant| {
            let total = &total;
            async move {
                while Instant::now() < until {
                    total();
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                total()
            }
        };
        let start = ask(Instant::now() + SAMPLED).await;
        let end = ask(Instant::now() + over).await;
        end.saturating_sub(start) * 1000 / over.as_millis() as u64
    }

    /// What the member sends: 2000 bytes every 20 ms, 100 kB/s.
    const FRAME: usize = 2000;

    /// Well below the member's stream: keep-alives, announcements and the
    /// acknowledgements of what the host itself reads.
    const IDLE: u64 = 10_000;

    /// With everything healthy nobody pulls through a spare: the member, the
    /// host and a viewer all read the member, all through the relay. The
    /// spare session's upload (what the host receives from its guests) and
    /// the bridging host's upload to the relay stay at keep-alive level. Then
    /// the relay stops and the same measure carries the stream, which shows
    /// the spare was there all along.
    async fn nobody_pulls_the_spare_while_everything_is_healthy(relay: Meeting) {
        let room = bridged_to(relay).await;
        let relay_hop = room.relay.origin.hop();
        let _m = member_sending("m", room.targets(), client(), Arc::new(PeerStats::default()), moq_tokio::origin::spawn(), FRAME);
        let v = joined("v", room.targets(), client(), Arc::new(PeerStats::default()), moq_tokio::origin::spawn());
        let _v_reads = reading(&v.inbound, camera_path("m"));
        let _host_reads = reading(&room.host.origin, camera_path("m"));
        routed_through(&v.inbound, &camera_path("m"), relay_hop, "v never had m through the relay").await;
        routed_through(&room.host.origin, &camera_path("m"), relay_hop, "the host never had m through the relay").await;
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let spare = received(&room.host, Duration::from_secs(1)).await;
        let bridging = sent(&room._to_relay, Duration::from_secs(1)).await;
        assert!(spare < IDLE, "the spare session uploads {spare} B/s with the relay up");
        assert!(bridging < IDLE, "the host uploads {bridging} B/s to the relay with the relay up");

        room.relay.stop().await;
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if route_within(&room.host.origin, &camera_path("m")).await.is_some_and(|route| !route.hops.as_slice().contains(&relay_hop)) {
                break;
            }
            assert!(Instant::now() < deadline, "the host had no spare of m to fall back on");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let carrying = received(&room.host, Duration::from_secs(1)).await;
        assert!(carrying > IDLE * 5, "the spare carried only {carrying} B/s once the relay was gone");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nobody_pulls_the_spare_through_an_older_relay() {
        nobody_pulls_the_spare_while_everything_is_healthy(Meeting::older().await).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nobody_pulls_the_spare_through_a_priced_relay() {
        nobody_pulls_the_spare_while_everything_is_healthy(Meeting::start().await).await;
    }

    /// The host itself, when the relay stops: a guest reading the host's own
    /// video, and the host reading a member, both carry on with no leave. The
    /// host needs no spare of its own (it offers on every session already);
    /// what it reads of the member is bounced to the member's spare on it.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_hosts_own_video_and_its_view_of_a_member_carry_on_when_the_relay_stops() {
        let room = bridged().await;
        let relay_hop = room.relay.origin.hop();
        let shared = room.host.origin.clone();
        let mut own = camera("h", &shared);
        let outbound = moq_tokio::origin::spawn();
        let mut theirs = camera("m", &outbound);
        let _m = joined_asking("m", room.targets(), client(), Arc::new(PeerStats::default()), outbound, theirs.keyframe.clone());
        let v = joined("v", room.targets(), client(), Arc::new(PeerStats::default()), moq_tokio::origin::spawn());
        let (_v_reading, _v_tiles, of_host) = viewing_of("v", &v.inbound, Some("h"));
        let (_h_reading, _h_tiles, of_member) = viewing_of("h", &shared, Some("m"));
        own.turn_on();
        theirs.turn_on();
        of_host.more(30, "no video from the host").await;
        of_member.more(30, "the host saw no video from m").await;
        routed_through(&v.inbound, &camera_path("h"), relay_hop, "v never had the host through the relay").await;
        routed_through(&shared, &camera_path("m"), relay_hop, "the host never had m through the relay").await;
        let (_watching_host, host_left) = retractions(&v.inbound, &camera_path("h"));
        let (_watching_member, member_left) = retractions(&shared, &camera_path("m"));

        let stopped = Instant::now();
        room.relay.stop().await;
        of_host.more(75, "the host's video stopped when the relay did").await;
        of_member.more(75, "the host's view of m stopped when the relay did").await;
        let (host_gap, member_gap) = (of_host.longest_gap(stopped), of_member.longest_gap(stopped));
        past_the_grace(stopped + Duration::from_millis(500)).await;
        of_host.more(10, "the host's video stopped after the relay did").await;
        of_member.more(10, "the host's view of m stopped after the relay did").await;
        assert_eq!(of_host.left(), 0, "the viewer's tile for the host left");
        assert_eq!(of_member.left(), 0, "the host's tile for m left");
        eprintln!(
            "relay stopped under the host: host's video gap {host_gap:?}, its view of m {member_gap:?}, {} and {} withdrawals",
            count(&host_left),
            count(&member_left)
        );
        // This stand-in host is not a join, so nothing asks its encoder for a
        // keyframe: its own video waits out the group in flight.
        assert!(host_gap < GROUP, "the host's video was gone {host_gap:?}");
        assert!(member_gap < SPLICE, "the host's view of m was gone {member_gap:?}");
    }

    /// Hosting switched off and on again in the same call opens the room's
    /// door again: a guest holding it gets in, one without it is turned away,
    /// at most one accept loop is ever open, and a guest turned away by the
    /// switch is no longer counted as one.
    #[tokio::test(flavor = "multi_thread")]
    async fn hosting_switched_off_and_on_again_lets_guests_in_again() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use super::super::{Served, keep_hosting};

        /// One open accept loop, counted until it is dropped.
        struct Open(Arc<AtomicUsize>);
        impl Drop for Open {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }

        crypto();
        let shared = moq_tokio::origin::spawn();
        let local = Arc::new(PeerStats::default());
        local.set_host_rooms(true);
        let hosting = door::Hosting::open("team".to_string()).unwrap();
        let offer = hosting.offer(shared.hop().id());
        let guests = Arc::new(super::super::arrival::Guests::default());
        let open_now = Arc::new(AtomicUsize::new(0));
        let opened = Arc::new(AtomicUsize::new(0));
        let port = Arc::new(std::sync::Mutex::new(0u16));
        let open = {
            let (shared, guests, open_now, opened, port) = (shared.clone(), guests.clone(), open_now.clone(), opened.clone(), port.clone());
            move || {
                let (shared, hosting, guests, open_now, opened, port) =
                    (shared.clone(), hosting.clone(), guests.clone(), open_now.clone(), opened.clone(), port.clone());
                async move {
                    let mut config = moq_tokio::server::Config::default();
                    config.listen.bind = Some(moq_tokio::listen::Bind::Addr(([127, 0, 0, 1], 0).into()));
                    config.listen.tls.generate = vec!["localhost".to_string()];
                    config.publisher = Some(shared.consume());
                    config.subscriber = Some(shared.clone());
                    let server = config.init().ok()?.bind().await.ok()?;
                    *port.lock().unwrap() = server.local_addr().ok()?.port();
                    let offer = hosting.offer(shared.hop().id());
                    let (accepting, events) = accept(server, Some(hosting), None, Arc::new(Metrics::default()), None, Some(guests), None);
                    opened.fetch_add(1, Ordering::SeqCst);
                    open_now.fetch_add(1, Ordering::SeqCst);
                    let counted = Open(open_now);
                    let accepting = Abort(tokio::spawn(async move {
                        let _held = (accepting, counted);
                        std::future::pending::<()>().await
                    }));
                    Some(Served { accepting, events, offer })
                }
            }
        };
        let open_first = open.clone();
        let first = open_first().await.expect("the first door opens");
        let _hosting = Abort(tokio::spawn(keep_hosting(local.clone(), None, first, open)));

        // Until what the switch asks for is what is served, and said.
        let settles = |on: bool, times: usize| {
            let (local, open_now, opened) = (local.clone(), open_now.clone(), opened.clone());
            async move {
                tokio::time::timeout(WITHIN, async {
                    while local.offer().is_some() != on
                        || open_now.load(Ordering::SeqCst) != usize::from(on)
                        || opened.load(Ordering::SeqCst) != times
                    {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap_or_else(|_| panic!("never settled {}", if on { "on" } else { "off" }));
            }
        };
        settles(true, 1).await;
        assert_eq!(local.offer(), Some(offer.clone()));
        for times in [2, 3] {
            local.set_host_rooms(false);
            settles(false, times - 1).await;
            assert!(!local.hosting(), "our own entry stops saying so");
            local.set_host_rooms(true);
            settles(true, times).await;
            assert!(local.hosting(), "and says so again");
        }
        // The same room's door, every time, so its members' saved invites work.
        assert_eq!(local.offer(), Some(offer.clone()));

        let at_door = |door: Option<&str>| {
            let mut url = at(*port.lock().unwrap());
            if let Some(door) = door {
                url.set_query(Some(&format!("{}={door}", door::PARAM)));
            }
            url
        };
        let wrong = door::fresh().unwrap();
        let w = member("w", vec![relay_at(at_door(Some(&wrong)))]);
        let ended = tokio::time::timeout(WITHIN, w.ended).await.expect("ended in time").expect("an ending");
        assert_eq!(ended, TURNED_AWAY, "{ended}");
        let _g = member("g", vec![relay_at(at_door(Some(&offer.door)))]);
        becomes(&shared, &camera_path("g"), true).await;
        assert_eq!(guests.hops().len(), 1, "the guest's hop is kept while it is in");

        local.set_host_rooms(false);
        settles(false, 3).await;
        // Polled, not awaited as an update: the session can be gone before a
        // watcher would be listening for its retraction.
        let gone = Instant::now();
        while heard_within(&shared, &camera_path("g"), Duration::from_millis(200)).await {
            assert!(gone.elapsed() < WITHIN, "the guest was never turned away");
        }
        assert!(guests.hops().is_empty(), "a guest turned away is still counted as one");
    }

    /// A camera of `me`'s on `origin`, a small frame every 20 ms.
    fn publishing(me: &str, origin: &moq_net::origin::Producer) -> (moq_net::broadcast::Producer, Abort) {
        let camera = origin.create_broadcast(camera_path(me)).unwrap();
        let mut track = camera.create_track("video", None).unwrap();
        camera.announce(moq_net::origin::Route::default()).unwrap();
        let writing = Abort(tokio::spawn(async move {
            for n in 0u64.. {
                if track.write_frame(moq_net::Timestamp::from_millis(n * 20).unwrap(), vec![0u8; 100]).is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }));
        (camera, writing)
    }

    /// How long each of some paths has gone unannounced at one origin, at
    /// most, from when they were all first there: what the attendance loop
    /// takes for a leave once it outlasts the grace.
    struct Absences {
        gaps: Arc<std::sync::Mutex<Gaps>>,
        _watching: Abort,
    }

    /// Each path's withdrawal in progress, if any, and its longest yet.
    type Gaps = std::collections::HashMap<String, (Option<Instant>, Duration)>;

    impl Absences {
        /// Waits until every one of `paths` is announced at `origin`, then
        /// watches them.
        async fn of(origin: &moq_net::origin::Producer, paths: &[String], what: &str) -> Absences {
            for path in paths {
                assert!(heard_within(origin, path, Duration::from_secs(10)).await, "{what}: {path} never arrived");
            }
            let gaps: Gaps = paths.iter().map(|path| (path.clone(), (None, Duration::ZERO))).collect();
            let gaps = Arc::new(std::sync::Mutex::new(gaps));
            let writing = gaps.clone();
            let mut announced = origin.consume().announced();
            let watching = Abort(tokio::spawn(async move {
                while let Some(update) = announced.next().await {
                    let mut gaps = writing.lock().unwrap();
                    let Some((since, longest)) = gaps.get_mut(update.prefix.as_str()) else { continue };
                    match (update.kind.is_active(), *since) {
                        (true, Some(at)) => {
                            *longest = (*longest).max(at.elapsed());
                            *since = None;
                        }
                        (false, None) => *since = Some(Instant::now()),
                        _ => {}
                    }
                }
            }));
            Absences { gaps, _watching: watching }
        }

        /// Each path that has been gone for `limit` or longer, and for how long.
        fn over(&self, limit: Duration) -> Vec<(String, Duration)> {
            let gaps = self.gaps.lock().unwrap();
            let mut over: Vec<(String, Duration)> = gaps
                .iter()
                .map(|(path, (since, longest))| (path.clone(), (*longest).max(since.map(|at| at.elapsed()).unwrap_or_default())))
                .filter(|(_, gap)| *gap >= limit)
                .collect();
            over.sort();
            over
        }
    }

    /// Everyone's camera but `me`'s, of `everyone`.
    fn others(me: &str, everyone: &[&str]) -> Vec<String> {
        everyone.iter().filter(|who| **who != me).map(|who| camera_path(who)).collect()
    }

    /// Two hosts that know each other from the start, each with a guest who
    /// knows only that host, all on the relay. The relay stops: the two hosts'
    /// session to each other carries each side's guest across, and nobody is
    /// gone from anybody's view for as long as a leave takes.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_hosts_that_know_each_other_keep_the_room_whole_when_the_relay_stops() {
        let relay = Meeting::older().await;
        let p = hosted_as('c', vec![relay_at(relay.url())]).await;
        let h = hosted_as('h', vec![relay_at(relay.url()), host_at('c', at(p.port))]).await;
        let _p_camera = publishing("p", &p.origin);
        let _h_camera = publishing("h", &h.origin);
        let a = member("a", vec![relay_at(relay.url()), host_at('c', at(p.port))]);
        let b = member("b", vec![relay_at(relay.url()), host_at('h', at(h.port))]);
        let everyone = ["a", "b", "p", "h"];
        let views = [
            Absences::of(&a.inbound, &others("a", &everyone), "a").await,
            Absences::of(&b.inbound, &others("b", &everyone), "b").await,
            Absences::of(&p.origin, &others("p", &everyone), "p").await,
            Absences::of(&h.origin, &others("h", &everyone), "h").await,
        ];
        // Every spare offered and every route ranked.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let stopped = Instant::now();
        relay.stop().await;
        past_the_grace(stopped + Duration::from_secs(1)).await;
        for (who, view) in everyone.iter().zip(&views) {
            let gone = view.over(super::super::super::grace::GRACE);
            assert!(gone.is_empty(), "{who} lost {gone:?}");
        }
    }

    /// Waits until `local` says it holds a session to every one of `hosts`
    /// (`held`), or to none of them.
    async fn holding(local: &PeerStats, hosts: &[char], held: bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let now = local.held();
            if hosts.iter().all(|c| now.contains(&id(*c)) == held) {
                return;
            }
            assert!(Instant::now() < deadline, "{what}: holding {now:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Tells a join that each of `hosts`, a letter and a port, hosts the room.
    fn tell(learn: &futures_channel::mpsc::UnboundedSender<Learned>, hosts: &[(char, u16)]) {
        for (c, port) in hosts {
            let _ = learn.unbounded_send(Learned::Host(host_at(*c, at(*port))));
        }
    }

    /// The fault as it was seen: two hosts, and two guests on the relay, who
    /// all hear of both hosts during the call rather than at join. The relay
    /// stops: the higher host has dialled the lower, each guest holds both,
    /// everyone's media meets at the lower host, and nobody is gone from
    /// anybody's view for as long as a leave takes. Before hosts were dialled
    /// as they were heard of, this split the room.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_hosts_heard_of_during_the_call_keep_the_room_whole_when_the_relay_stops() {
        let lost = two_hosts_and_two_guests_through_a_relay_stop(true).await;
        assert!(lost.is_empty(), "{lost:?}");
    }

    /// The control: the same room, nobody told of the hosts, so nobody
    /// dials them and the stop splits it. Without this the check above could
    /// pass by seeing no loss at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_hosts_nobody_is_told_of_split_the_room_when_the_relay_stops() {
        let lost = two_hosts_and_two_guests_through_a_relay_stop(false).await;
        assert!(!lost.is_empty(), "nobody lost anybody, so the check cannot see a split");
    }

    /// Two hosts and two guests, all on the relay alone at join, each told
    /// of both hosts (`told`) during the call; then the relay stops. Who lost
    /// whom for as long as a leave takes.
    async fn two_hosts_and_two_guests_through_a_relay_stop(told: bool) -> Vec<String> {
        use super::super::super::arrival::Sending;
        let relay = Meeting::older().await;
        let p = hosted_as('c', vec![relay_at(relay.url())]).await;
        let h = hosted_as('e', vec![relay_at(relay.url())]).await;
        let _p_camera = publishing("p", &p.origin);
        let _h_camera = publishing("h", &h.origin);
        let (on_a, on_b) = (Arc::new(PeerStats::default()), Arc::new(PeerStats::default()));
        let a = member_as("a", vec![relay_at(relay.url())], client(), on_a.clone());
        let b = member_as("b", vec![relay_at(relay.url())], client(), on_b.clone());
        let everyone = ["a", "b", "p", "h"];
        let views = [
            Absences::of(&a.inbound, &others("a", &everyone), "a").await,
            Absences::of(&b.inbound, &others("b", &everyone), "b").await,
            Absences::of(&p.origin, &others("p", &everyone), "p").await,
            Absences::of(&h.origin, &others("h", &everyone), "h").await,
        ];
        if told {
            let hosts = [('c', p.port), ('e', h.port)];
            for learn in [&a.learn, &b.learn, &p.learn, &h.learn] {
                tell(learn, &hosts);
            }
            holding(&on_a, &['c', 'e'], true, "a never dialled both hosts").await;
            holding(&on_b, &['c', 'e'], true, "b never dialled both hosts").await;
            holding(&h.local, &['c'], true, "the higher host never dialled the lower").await;
        }
        // Spares offered, routes ranked.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!p.local.held().contains(&id('e')), "the lower host dialled the higher too");

        let stopped = Instant::now();
        relay.stop().await;
        past_the_grace(stopped + Duration::from_secs(1)).await;
        let mut lost = Vec::new();
        for (who, view) in everyone.iter().zip(&views) {
            let gone = view.over(super::super::super::grace::GRACE);
            if !gone.is_empty() {
                lost.push(format!("{who} lost {gone:?}"));
            }
        }
        if told {
            assert_eq!(on_a.sending(), Some(Sending::Host(id('c'))), "a's media is not at the lower host");
            assert_eq!(on_b.sending(), Some(Sending::Host(id('c'))), "b's media is not at the lower host");
        }
        lost
    }

    /// A guest as a call wires one, on the relay alone at join: its presence
    /// said, everyone's read, and each host it hears of told to its join,
    /// dialled at the port `hosts` gives that host's id. Counts the leaves
    /// told to the join.
    struct Guest {
        member: Member,
        local: Arc<PeerStats>,
        lefts: Arc<std::sync::atomic::AtomicUsize>,
        _control: moq_net::broadcast::Producer,
        _announcing: Abort,
        _attending: Abort,
        _forwarding: Abort,
    }

    fn guest_hearing(me: &str, relay: url::Url, hosts: Vec<(String, u16)>) -> Guest {
        use super::super::{Dial, RoomNews};
        let local = Arc::new(PeerStats::default());
        let outbound = moq_tokio::origin::spawn();
        let (control, announcing) = saying(me, &outbound, &local);
        let member = member_from(me, vec![relay_at(relay)], client(), local.clone(), outbound);
        let (to, mut from) = futures_channel::mpsc::unbounded::<Learned>();
        let lefts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (counting, learn) = (lefts.clone(), member.learn.clone());
        let forwarding = Abort(tokio::spawn(async move {
            while let Some(learned) = from.next().await {
                if matches!(learned, Learned::Left(_)) {
                    counting.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                let _ = learn.unbounded_send(learned);
            }
        }));
        let ports: std::collections::HashMap<String, u16> = hosts.into_iter().collect();
        let locate = move |known: &door::Known| {
            let mut url = at(*ports.get(&known.id)?);
            url.query_pairs_mut().append_pair(door::PARAM, &known.door);
            Some(Target {
                host: Some(known.id.clone()),
                url,
            })
        };
        let news = RoomNews {
            room: "team".to_string(),
            to: futures_channel::mpsc::unbounded().0,
            dial: Some(Dial {
                to,
                locate: Arc::new(locate),
            }),
        };
        let attending = attending_with(me, &member.inbound, &local, Some(news));
        Guest {
            member,
            local,
            lefts,
            _control: control,
            _announcing: announcing,
            _attending: attending,
            _forwarding: forwarding,
        }
    }

    /// A host that says so in presence, with a camera of its own.
    struct Saying {
        host: Hosted,
        _control: moq_net::broadcast::Producer,
        _announcing: Abort,
        _camera: (moq_net::broadcast::Producer, Abort),
    }

    async fn saying_host(me: char, relay: url::Url) -> Saying {
        let host = hosted_as(me, vec![relay_at(relay)]).await;
        let (control, announcing) = saying(&id(me), &host.origin, &host.local);
        let camera = publishing(&id(me), &host.origin);
        Saying {
            host,
            _control: control,
            _announcing: announcing,
            _camera: camera,
        }
    }

    /// One host, and two guests who joined on the relay alone before it
    /// said it hosts: each hears it in presence and dials it then. The relay
    /// stops: the guests still see the host and each other. And the bounce
    /// of the host's presence that the stop makes is not a leave, so each
    /// guest keeps its session to the host. Before hosts were dialled as they
    /// were heard of, both guests were cut off.
    #[tokio::test(flavor = "multi_thread")]
    async fn guests_who_hear_of_a_host_during_the_call_keep_it_when_the_relay_stops() {
        let relay = Meeting::older().await;
        let h = saying_host('e', relay.url()).await;
        let hosts = || vec![(id('e'), h.host.port)];
        let g = guest_hearing("g", relay.url(), hosts());
        let m = guest_hearing("m", relay.url(), hosts());
        let host = id('e');
        let g_view = Absences::of(&g.member.inbound, &[camera_path(&host), camera_path("m")], "g").await;
        let h_view = Absences::of(&h.host.origin, &[camera_path("g"), camera_path("m")], "the host").await;
        holding(&g.local, &['e'], true, "g never dialled the host it heard of").await;
        holding(&m.local, &['e'], true, "m never dialled the host it heard of").await;
        tokio::time::sleep(Duration::from_secs(1)).await;

        let stopped = Instant::now();
        relay.stop().await;
        past_the_grace(stopped + Duration::from_secs(1)).await;
        let gone = g_view.over(super::super::super::grace::GRACE);
        assert!(gone.is_empty(), "g lost {gone:?}");
        let gone = h_view.over(super::super::super::grace::GRACE);
        assert!(gone.is_empty(), "the host lost {gone:?}");
        for guest in [&g, &m] {
            assert_eq!(count(&guest.lefts), 0, "a bounce was told as a leave");
            assert!(guest.local.held().contains(&host), "a guest let go of the host after a bounce");
        }
    }

    /// A host that stops hosting during the call: its presence says so, and
    /// each guest that dialled it hangs that session up, while the room goes
    /// on through the relay with nobody gone.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_that_stops_hosting_is_hung_up_and_nobody_leaves() {
        let relay = Meeting::older().await;
        let h = saying_host('e', relay.url()).await;
        let mut g = guest_hearing("g", relay.url(), vec![(id('e'), h.host.port)]);
        let host = id('e');
        let view = Absences::of(&g.member.inbound, &[camera_path(&host)], "g").await;
        holding(&g.local, &['e'], true, "g never dialled the host it heard of").await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        h.host.local.set_offer(None);
        holding(&g.local, &['e'], false, "g kept its session to a host that stopped hosting").await;
        // From the hang-up itself, which comes an overlap after the session
        // is let go: a loss it caused for good has outlasted the grace by then.
        past_the_grace(Instant::now() + MOVE_OVERLAP).await;
        let gone = view.over(super::super::super::grace::GRACE);
        assert!(gone.is_empty(), "g lost {gone:?}");
        assert!(g.member.ended.try_recv().is_err(), "the join ended");
    }

    /// A host heard of with a door it does not open: turned away, the call
    /// goes on through the relay, and no session to it is held. Heard again
    /// with its own door, it is dialled.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_heard_of_with_the_wrong_door_is_turned_away_and_dialled_again_with_the_right_one() {
        let relay = Meeting::older().await;
        let door = door::Hosting::open("team".to_string()).unwrap();
        let right = door.offer(1).door;
        let h = hosted_behind('e', vec![relay_at(relay.url())], Some(door)).await;
        let local = Arc::new(PeerStats::default());
        let mut g = member_as("g", vec![relay_at(relay.url())], client(), local.clone());
        becomes(&h.origin, &camera_path("g"), true).await;
        let behind = |door: &str| {
            let mut url = at(h.port);
            url.query_pairs_mut().append_pair(door::PARAM, door);
            host_at('e', url)
        };
        let _ = g.learn.unbounded_send(Learned::Host(behind(&door::fresh().unwrap())));
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until {
            assert!(!local.held().contains(&id('e')), "a session held through the wrong door");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(g.ended.try_recv().is_err(), "the join ended when a host turned it away");
        let _ = g.learn.unbounded_send(Learned::Host(behind(&right)));
        holding(&local, &['e'], true, "the host was not dialled with its own door").await;
    }

    /// Every join now goes through the set, the relay alone when no hosts
    /// are known. What the relay is offered by such a join, beside a join
    /// wired as one was before: the same broadcasts at the same price, each
    /// naming its publisher first, and one hop longer, the gate's.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_join_of_the_relay_alone_offers_it_what_a_plain_join_did_one_gate_hop_apart() {
        let relay = Meeting::older().await;
        let through_set = moq_tokio::origin::spawn();
        let _set = member_from("s", vec![relay_at(relay.url())], client(), Arc::new(PeerStats::default()), through_set.clone());
        let plain = moq_tokio::origin::spawn();
        let _camera = publishing("p", &plain);
        let _plain = client()
            .with_publisher(plain.consume())
            .with_subscriber(moq_tokio::origin::spawn())
            .connect(relay.url());
        let (s, p) = (camera_path("s"), camera_path("p"));
        becomes(&relay.origin, &s, true).await;
        becomes(&relay.origin, &p, true).await;
        // Nothing else is offered: only what each publishes.
        let mut announced = relay.origin.consume().announced();
        let mut paths = Vec::new();
        while let Ok(Some(update)) = tokio::time::timeout(Duration::from_millis(500), announced.next()).await {
            if update.kind.is_active() {
                paths.push(update.prefix.as_str().to_string());
            }
        }
        paths.sort();
        assert_eq!(paths, [p.clone(), s.clone()]);
        let by_set = route_within(&relay.origin, &s).await.expect("the set's route");
        let by_plain = route_within(&relay.origin, &p).await.expect("the plain route");
        assert_eq!(by_set.hops.as_slice().first(), Some(&through_set.hop()), "{by_set:?}");
        assert_eq!(by_plain.hops.as_slice().first(), Some(&plain.hop()), "{by_plain:?}");
        assert_eq!(by_set.hops.len(), by_plain.hops.len() + 1, "{by_set:?} beside {by_plain:?}");
        assert_eq!(by_set.cost, by_plain.cost);
    }

    /// A session to nowhere, as a dial makes one: what `reconcile` is
    /// handed in a test that counts dials rather than making them.
    fn nowhere(down: &Reserved) -> Session {
        let connection = client().connect(down.url());
        Session {
            _running: Abort(tokio::spawn(std::future::pending())),
            bandwidth: connection.send_bandwidth(),
            monitor: connection.monitor(),
            connection,
        }
    }

    /// A host whose door changes again and again while its session is not
    /// up: the list of sessions stays as long as the most held at once, no
    /// retired slot keeps a session, offer or gate once its overlap is over,
    /// and a burst of doors within the settle dials once.
    #[tokio::test(flavor = "multi_thread")]
    async fn many_doors_for_one_host_keep_the_set_bounded_and_dial_once_per_settle() {
        let down = Reserved::new();
        let outbound = moq_tokio::origin::spawn();
        let new = |target| super::Member::new(target, false, &outbound);
        let mut members = vec![new(relay_at(down.url()))];
        let mut hosts = Hosts::default();
        let mut quiet = Quiet::default();
        let dials = std::cell::Cell::new(0usize);
        let mut dial = |_: usize, _: &super::Member, _: bool| {
            dials.set(dials.get() + 1);
            nowhere(&down)
        };
        let door = |n: u32| {
            let mut url = down.url();
            url.set_query(Some(&format!("door={n:032x}")));
            host_at('a', url)
        };
        let start = Instant::now();
        for n in 0..30 {
            let now = start + (DOOR_SETTLE + MOVE_OVERLAP) * n;
            hosts.learn(Learned::Host(door(n)), now);
            reconcile(&mut members, &mut hosts, now, &new, &mut quiet, None, &mut dial);
        }
        assert_eq!(dials.get(), 30, "each settled door dialled once");
        assert!(members.len() <= 3, "{} slots for one host", members.len());
        // Past the last overlap, every retired slot has let go of all it held.
        tokio::time::sleep(MOVE_OVERLAP + Duration::from_millis(200)).await;
        for member in members.iter().filter(|member| member.state == State::Retired) {
            assert!(member.session.is_none() && member.offer.is_none() && member.gate.is_none(), "a retired slot keeps something live");
        }
        let live = members.iter().filter(|member| member.state != State::Retired && member.gate.is_some()).count();
        assert_eq!(live, 2, "the relay's gate and the host's");

        let burst = start + (DOOR_SETTLE + MOVE_OVERLAP) * 30;
        let before = dials.get();
        for n in 0..20 {
            let now = burst + Duration::from_millis(100) * n;
            hosts.learn(Learned::Host(door(100 + n)), now);
            reconcile(&mut members, &mut hosts, now, &new, &mut quiet, None, &mut dial);
        }
        assert_eq!(dials.get(), before + 1, "a burst dialled more than once");
        reconcile(&mut members, &mut hosts, burst + DOOR_SETTLE * 2, &new, &mut quiet, None, &mut dial);
        assert_eq!(dials.get(), before + 2, "the latest door of the burst was never dialled");
    }

    /// The host carrying our media, with the relay gone, says a new door.
    /// The session it let us in with stays good, so it is kept: nobody there
    /// loses our media, and the new door waits for the next dial. This host
    /// still opens only its first door, so a session redialled with the new
    /// one would be turned away: hanging up the working one loses m.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_door_from_the_host_carrying_our_media_keeps_its_session() {
        use super::super::super::arrival::Sending;
        let relay = Meeting::older().await;
        let first = door::fresh().unwrap();
        let h = hosted_behind('e', vec![relay_at(relay.url())], Some(door::Hosting::with_door("team".to_string(), first.clone()))).await;
        let on_m = Arc::new(PeerStats::default());
        let m = member_as("m", vec![relay_at(relay.url())], client(), on_m.clone());
        let door = |door: &str| {
            let mut url = at(h.port);
            url.query_pairs_mut().append_pair(door::PARAM, door);
            host_at('e', url)
        };
        let _ = m.learn.unbounded_send(Learned::Host(door(&first)));
        holding(&on_m, &['e'], true, "m never dialled the host").await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        relay.stop().await;
        let deadline = Instant::now() + WITHIN;
        while on_m.sending() != Some(Sending::Host(id('e'))) {
            assert!(Instant::now() < deadline, "m's media never moved to the host");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let view = Absences::of(&h.origin, &[camera_path("m")], "the host").await;
        let changed = Instant::now();
        let _ = m.learn.unbounded_send(Learned::Host(door(&door::fresh().unwrap())));
        past_the_grace(changed + MOVE_OVERLAP + Duration::from_secs(1)).await;
        let gone = view.over(super::super::super::grace::GRACE);
        assert!(gone.is_empty(), "the host lost {gone:?}");
        assert!(on_m.held().contains(&id('e')), "m hung up the host");
        assert_eq!(on_m.sending(), Some(Sending::Host(id('e'))));
    }

    /// A host heard of after the call was moved to it: dialled once, free,
    /// not at the usual price and then again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_heard_of_after_the_call_moved_to_it_is_dialled_once() {
        let room = bridged().await;
        let on_m = following();
        on_m.move_call(Some(id('c')), "m");
        let m = member_as("m", vec![relay_at(room.relay.url())], client(), on_m.clone());
        becomes(&room.relay.origin, &camera_path("m"), true).await;
        let _ = m.learn.unbounded_send(Learned::Host(host_at('c', room.host.url())));
        holding(&on_m, &['c'], true, "m never dialled the host the call is on").await;
        tokio::time::sleep(MOVE_OVERLAP + Duration::from_secs(1)).await;
        let dialled = room.host.sessions.lock().unwrap().len();
        assert_eq!(dialled, 1, "the host was dialled {dialled} times");
    }
}
