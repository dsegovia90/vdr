//! A join that can meet in more than one place: the room's relay and the
//! members who host it, dialled together, the call living on whatever answers.
//!
//! **Only a join that has somewhere else to go comes here**: the room has
//! hosts saved or invited, or we host it ourselves. Everyone else joins
//! through the single session `connect` makes, wired exactly as before.
//!
//! **Listen on all, publish on one.** Every session lands in the same inbound
//! origin, and the library already makes two routes to one publisher one
//! broadcast, splicing to the other when the one in use dies. What we publish
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
//! is withdrawn, and a viewer's subscription carries across. A withdrawal with
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
//! routes unused while a cheaper one exists. A host dials only the saved hosts
//! whose id sorts below its own, so two hosts hold one session between them,
//! not two.
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
//! so every tile leaves and comes back. So we also offer on a second session,
//! the first other live one in [`pick`]'s order ([`spare`]), at a price
//! ([`SPARE_COST`]) that keeps it unused while the carrier works. Viewers
//! then hold a route to us that outlives the carrier, and the library splices
//! a read across at the next group instead of ending it. When the carrier
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
//! follows: when that session ends, every reader splices to the spare at once
//! instead of leaving and coming back two seconds later. Leaving a dying
//! relay sooner is ending its session sooner: whatever decides it, the hook
//! is [`Member::give_up`] on the relay's session, which hangs it up (its
//! routes go, here and as a lost session at the far side, marking nothing
//! stale), and the next pass promotes the spare.
//!
//! **A host needs no spare.** It offers its one origin on every session, so
//! viewers already hold its own media through the relay and directly, and it
//! holds each member through the relay and through the member's spare: when
//! the relay goes, its own readers and its guests' splice like anyone's.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_channel::mpsc::UnboundedSender;
use futures_util::StreamExt as _;

use super::super::adapt;
use super::super::door;
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
    let mut ids: Vec<&door::Known> = hosts
        .iter()
        .filter(|host| host.id != me && Some(host.id.as_str()) != relay.host_str())
        .filter(|host| !hosting || host.id.as_str() < me)
        .filter(|host| door::is_door(&host.door))
        .collect();
    ids.sort_by(|a, b| a.id.cmp(&b.id));
    ids.dedup_by(|a, b| a.id == b.id);
    let mut targets = vec![Target {
        host: None,
        url: relay.clone(),
    }];
    for host in ids {
        let Ok(mut url) = url::Url::parse(&format!("iroh://{}", host.id)) else { continue };
        url.set_path(relay.path());
        url.query_pairs_mut().append_pair(door::PARAM, &host.door);
        targets.push(Target {
            host: Some(host.id.clone()),
            url,
        });
    }
    targets
}

/// Where one session is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum State {
    /// Dialled, not up yet.
    Dialing,
    /// Up since then.
    Live(Instant),
    /// Dropped; the stack is redialling it.
    Redialling,
    /// Gave up; dialled again from then if the call is still going.
    Resting(Instant),
    /// Turned away; not dialled again this call.
    Refused,
}

impl State {
    fn live(self) -> bool {
        matches!(self, State::Live(_))
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
pub(super) fn pick(
    states: &[State],
    current: Option<usize>,
    now: Instant,
    started: Instant,
    preferred: Option<usize>,
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
            .find(|&at| matches!(states[at], State::Live(since) if now.duration_since(since) >= HOLD));
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
/// No hold: a spare carries nothing until it is needed.
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

/// Whether a join keeps a spare offer at all: only with the switch on, and
/// never while hosting, whose one origin is offered whole on every session.
pub(super) fn keeps_spare(hosting: bool, switch: bool) -> bool {
    switch && !hosting
}

/// What the status line says about where we are.
pub(super) fn via(states: &[State], hosting: bool) -> Via {
    Via {
        relay: states.first().is_some_and(|state| state.live()),
        hosts: states.iter().skip(1).filter(|state| state.live()).count(),
        hosting,
    }
}

/// How a join over several sessions ends: `None` while it goes on. It goes
/// on while anything is up, anything is still being tried, or we host. At the
/// end the reason is the relay's, or "turned away" if every one refused us.
pub(super) fn ended(states: &[State], hosting: bool, errors: &[Option<String>]) -> Option<String> {
    if hosting || states.iter().any(|state| state.live() || state.trying()) {
        return None;
    }
    if states.iter().all(|state| *state == State::Refused) {
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
    pub(super) metrics: Arc<Metrics>,
    pub(super) local: Arc<PeerStats>,
    pub(super) status: UnboundedSender<RemoteStatus>,
    pub(super) keyframe: Arc<Keyframes>,
    /// The room as the status line names it.
    pub(super) room: String,
    /// Move the call round the relay and every host this often
    /// (`--bounce`), to test moving it.
    pub(super) bounce: Option<Duration>,
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
}

impl Member {
    /// Gives this session up: hangs it up, so its routes go here and at the
    /// far side as a lost session's do (marking nothing stale), and rests it
    /// before it is dialled again. The next pass moves our media to the spare
    /// by re-pricing it. What an ending session does, and what anything that
    /// judged a session dead before its transport did would call.
    fn give_up(&mut self, error: String) {
        self.session = None;
        self.error = Some(error);
        self.state = State::Resting(Instant::now() + REST);
    }
}

/// How the log names a session: the relay by its address, a host by its id.
fn named(member: &Member, relay: &str) -> String {
    match &member.target.host {
        None => relay.to_string(),
        Some(id) => format!("host {id}"),
    }
}

/// A dialled session: its status task (which owns the connection, so
/// dropping this hangs up), and what the set follows when it carries us.
struct Session {
    _running: Abort,
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
        metrics,
        local,
        status,
        keyframe,
        room,
        bounce,
    } = args;
    let (events, mut heard) = futures_channel::mpsc::unbounded::<(usize, Event)>();
    let dial = {
        let inbound = inbound.clone();
        let outbound = outbound.clone();
        move |index: usize, member: &Member, chosen: bool| -> Session {
            let mut client = client.clone();
            if member.target.host.is_some() {
                client = client.with_cost(if chosen { CHOSEN_COST } else { HOST_COST });
            }
            let client = match &member.gate {
                Some(gate) => client.with_publisher(&gate.origin),
                None => client.with_publisher(&outbound),
            };
            let mut connection = client.with_subscriber(inbound.clone()).connect(member.target.url.clone());
            let bandwidth = connection.send_bandwidth();
            let monitor = connection.monitor();
            let mut refusals = Refusals::for_url(&member.target.url);
            let events = events.clone();
            let running = Abort(tokio::spawn(async move {
                let say = |event| {
                    let _ = events.unbounded_send((index, event));
                };
                loop {
                    match connection.status().await {
                        Ok(moq_tokio::Status::Connected) => {
                            if let Some(refusals) = refusals.as_mut() {
                                refusals.connected(Instant::now());
                            }
                            let priced = connection.version().is_some_and(carries_price);
                            say(Event::Live { priced });
                        }
                        Ok(moq_tokio::Status::Disconnected) => {
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
                bandwidth,
                monitor,
            }
        }
    };

    let mut members: Vec<Member> = targets
        .into_iter()
        .map(|target| Member {
            target,
            gate: (!hosting).then(|| Gate::new(&outbound)),
            state: State::Dialing,
            error: None,
            joined: false,
            redialled: false,
            session: None,
            priced: false,
            offer: None,
        })
        .collect();
    let hosts = members.len() - 1;
    tracing::info!("dialling the relay and {hosts} saved hosts");
    // Where the call is said to be as we join, so its host is dialled at its
    // price from the start rather than dialled twice.
    let mut chosen = chosen_member(&members, &local);
    for (index, member) in members.iter_mut().enumerate() {
        member.session = Some(dial(index, member, chosen == Some(index)));
    }
    let stops: Vec<Option<String>> = members.iter().map(|member| member.target.host.clone()).collect();
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
            let mut ticker = tokio::time::interval(TICK);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    event = heard.next() => {
                        let Some((index, event)) = event else { return "the sessions stopped".to_string() };
                        // Whether the call goes on elsewhere, so an ending here
                        // is said as a retry or as what it is.
                        let elsewhere = (hosting && local.offer().is_some())
                            || members.iter().enumerate().any(|(at, member)| at != index && member.state.live());
                        let member = &mut members[index];
                        let name = |member: &Member| named(member, &relay_label);
                        match event {
                            Event::Live { priced } if std::mem::take(&mut member.redialled) && member.state.live() => {
                                member.priced = priced;
                                tracing::info!("{} answered at its new price", name(member));
                            }
                            Event::Live { priced } => {
                                member.priced = priced;
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
                    }
                    _ = ticker.tick() => {}
                }

                let now = Instant::now();
                let serving = hosting && local.offer().is_some();
                let states: Vec<State> = members.iter().map(|member| member.state).collect();
                let errors: Vec<Option<String>> = members.iter().map(|member| member.error.clone()).collect();
                if let Some(error) = ended(&states, serving, &errors) {
                    return error;
                }

                // A session that gave up is dialled again while the call goes
                // on somewhere, so a relay that comes back is found.
                let going = serving || states.iter().any(|state| state.live());
                for (index, member) in members.iter_mut().enumerate() {
                    if let State::Resting(until) = member.state {
                        if going && now >= until {
                            member.state = State::Dialing;
                            member.session = Some(dial(index, member, chosen == Some(index)));
                        }
                    }
                }

                let states: Vec<State> = members.iter().map(|member| member.state).collect();
                // Until the relay has answered or failed once, a host with
                // nobody else up is still joining, not cut off.
                let relay_tried = states.first().is_some_and(|state| *state != State::Dialing);
                // A bounce moves the call on, for everyone following moves;
                // what follows below applies it here like any other move.
                if next_bounce.is_some_and(|due| now >= due) {
                    if local.move_calls() {
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
                        if member.session.is_none() {
                            continue;
                        }
                        let id = member.target.host.clone().unwrap_or_default();
                        if free {
                            tracing::info!("the call is on host {id}; dialling it again, free");
                        } else {
                            tracing::info!("the call left host {id}; dialling it again at the usual price");
                        }
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

                let next = pick(&states, carrier, now, started, chosen);
                // With nothing live, every offer stays as it is: a session
                // that died carries nothing, and one that comes back finds
                // it still offered.
                if let Some(next) = next {
                    let priced: Vec<bool> = members.iter().map(|member| member.priced).collect();
                    let spare_at = keeps_spare(hosting, local.spare_route())
                        .then(|| spare(&states, &priced, next, chosen))
                        .flatten();
                    arrange(&mut members, next, spare_at, &outbound, &me, &relay_label);
                }
                if next.is_some() && next != carrier {
                    let index = next.unwrap_or_default();
                    let member = &members[index];
                    match &member.target.host {
                        None => tracing::info!("our media goes through the relay"),
                        Some(id) => tracing::info!("our media goes through host {id}"),
                    }
                    local.set_sending(Some(match &member.target.host {
                        None => super::super::arrival::Sending::Relay,
                        Some(id) => super::super::arrival::Sending::Host(id.clone()),
                    }));
                    if let Some(session) = &member.session {
                        _following = Some(follow(session, &estimate, &metrics));
                    }
                    keyframe.ask();
                    carrier = next;
                    on_guests = false;
                } else if next.is_none() && serving && !on_guests && relay_tried {
                    tracing::info!("still hosting; no way to the relay");
                    local.set_sending(Some(super::super::arrival::Sending::Guests));
                    _following = guests.clone().map(|guests| follow_guests(guests, &estimate));
                    // Every guest reading our own media through the relay
                    // has just spliced to its direct route, and the library
                    // passes on what it reads there from the next group: a
                    // keyframe now, rather than the rest of the encoder's
                    // interval.
                    keyframe.ask();
                    carrier = None;
                    on_guests = true;
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
/// the spare's at [`SPARE_COST`], the rest withdrawn.
///
/// **Make before break, and re-price rather than withdraw.** A session that
/// keeps an offer, changing only what it is for, is re-priced in place, so a
/// spare becomes the carrier with every viewer's read carried across. A new
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
) {
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
                if role == Role::Spare {
                    tracing::info!("a spare offer on {name}");
                }
            }
            (Some(_), Some(role)) => {
                if let Some((offer, held)) = member.offer.as_mut() {
                    offer.reprice(role.price());
                    *held = role;
                }
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
                if was == Role::Spare {
                    tracing::info!("the spare on {name} is closed");
                }
            }
            (None, None) => {}
        }
    }
}

/// Which session goes to the host the call has been moved to, if we follow
/// moves and hold one to it.
fn chosen_member(members: &[Member], local: &PeerStats) -> Option<usize> {
    let host = local.call_host()?;
    members.iter().position(|member| member.target.host.as_deref() == Some(host.as_str()))
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
        let pick = |states: &[State], current, now| pick(states, current, now, start, None);
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
        let pick = |states: &[State], current, now, preferred| pick(states, current, now, start, preferred);
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

    /// A port nothing answers on: a relay that is down.
    fn nowhere() -> url::Url {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        at(socket.local_addr().unwrap().port())
    }

    /// A client as the app's, minus iroh, giving up on a dead address soon
    /// so a test of one does not wait out the app's whole budget.
    fn client() -> moq_tokio::Client {
        crypto();
        let mut config = moq_tokio::client::Config::default();
        config.connect.tls.insecure = Some(true);
        config.connect.backoff.timeout = Duration::from_secs(2);
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
    }

    fn member(me: &str, targets: Vec<Target>) -> Member {
        member_with(me, targets, client())
    }

    /// A client that keeps redialling a relay that is down, and notices it
    /// back within half a second, as the app's does within its budget.
    fn patient_client() -> moq_tokio::Client {
        crypto();
        let mut config = moq_tokio::client::Config::default();
        config.connect.tls.insecure = Some(true);
        config.connect.backoff.initial = Duration::from_millis(200);
        config.connect.backoff.max = Duration::from_millis(500);
        config.connect.backoff.timeout = Duration::from_secs(60);
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
        let peer = join(Join {
            client,
            me: me.to_string(),
            targets,
            outbound,
            inbound: inbound.clone(),
            hosting: false,
            estimate: moq_net::bandwidth::Producer::new(),
            guests: None,
            metrics: Arc::new(Metrics::default()),
            local,
            status,
            keyframe,
            room: "room team".to_string(),
            bounce: None,
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
        let started = Instant::now();
        let mut m = member("m", vec![relay_at(nowhere()), host_at('a', host.url())]);
        becomes(&host.origin, &camera_path("m"), true).await;
        assert_eq!(next_via(&mut m).await, Via { relay: false, hosts: 1, hosting: false });
        // The relay's moment to answer first, and no more: its own retries
        // would take the whole budget.
        assert!(started.elapsed() < Duration::from_secs(4), "it waited on the relay: {:?}", started.elapsed());
        // The relay gives up, and the call goes on through the host.
        tokio::time::sleep(Duration::from_secs(3)).await;
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
        let (_accepting, _) = accept(server, Some(hosting), Some(estimates), Arc::new(Metrics::default()), None, None);
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
        let port = {
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            socket.local_addr().unwrap().port()
        };
        // The host: one origin for its session to the relay and its guests.
        let shared = moq_tokio::origin::spawn();
        let host = Meeting::sharing(shared.clone()).await;
        let _to_relay = patient_client()
            .with_publisher(shared.consume())
            .with_subscriber(shared.clone())
            .connect(at(port));
        let _m = member_with("m", vec![relay_at(at(port)), host_at('h', host.url())], patient_client());
        becomes(&shared, &camera_path("m"), true).await;

        let relay = Meeting::on(port, moq_tokio::origin::spawn()).await;
        // Moved once the relay has m from m, not bridged through the host.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(route) = route_within(&relay.origin, &camera_path("m")).await {
                if !route.hops.as_slice().contains(&shared.hop()) {
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
        use super::super::{Presence, announce, control_path, presence_config, say, PRESENCE};
        let control = outbound.create_broadcast(control_path(me)).unwrap();
        let track = control.create_track(PRESENCE, None).unwrap();
        let presence = Arc::new(std::sync::Mutex::new(moq_json::snapshot::Producer::new(track, presence_config())));
        say(&presence, Presence::here(local));
        control.announce(moq_net::origin::Route::default()).unwrap();
        (control, Abort(tokio::spawn(announce(presence, local.clone()))))
    }

    /// The call's own reader of everyone announced at `inbound`.
    fn attending(me: &str, inbound: &moq_net::origin::Producer, local: &Arc<PeerStats>) -> Abort {
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
            news: None,
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
        let mut m = member_as("m", vec![relay_at(relay.url()), host_at('h', nowhere())], client(), on_m);
        let v = member_as("v", vec![relay_at(nowhere()), host_at('h', host.url())], client(), on_v);
        // At the relay, v arrives bridged by the host, and m straight from m.
        routed_through(&relay.origin, &camera_path("v"), shared.hop(), "v never reached the relay through the host").await;
        becomes(&relay.origin, &camera_path("m"), true).await;
        let route = route_within(&relay.origin, &camera_path("m")).await.expect("m on the relay");
        assert!(!route.hops.as_slice().contains(&shared.hop()), "m went through the host: {route:?}");
        routed_through(&v.inbound, &camera_path("m"), host.origin.hop(), "v never had m through the host").await;
        // Past the time a lost session takes to give up: m stays, and so
        // does v's way to it.
        tokio::time::sleep(Duration::from_secs(3)).await;
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
                        let known = arrival::Known { theirs: None, hosts: &hosts, guests: &guests, ours: 0, far: None, who: "m" };
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
        catalog: moq_mux::catalog::Producer,
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
        let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
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

    /// When each of someone's camera frames reached its tile at a viewer.
    #[derive(Clone, Default)]
    struct Arrivals(Arc<std::sync::Mutex<Vec<Instant>>>);

    impl Arrivals {
        fn count(&self) -> usize {
            self.0.lock().unwrap().len()
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
    /// A read carried across picks up at the next group, and one asked for
    /// again starts at the newest, so the bound is a group (the encoder's
    /// keyframe interval, two seconds) plus the wait before asking again and
    /// the route settling: about 2 s was measured for a relay stopping, under
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

    /// The longest a viewer may go without a frame when a spare takes over
    /// from a carrier that stopped cleanly. The library splices the read to
    /// the spare's route as soon as the carrier's goes, but what it then
    /// passes on starts at the next group: on its own that is the rest of the
    /// encoder's keyframe interval, up to two seconds (measured: 1.0 s with
    /// the stop a second into a group, 0.5 s at a second and a half). The
    /// member's own move to the spare asks its encoder for a keyframe, as the
    /// call does, which starts that group at once: about 45 ms measured. The
    /// bound leaves room for a debug build on a busy machine. Without a spare
    /// the same stop takes 2 s: a retraction, the library's hold, and the
    /// media asked for again.
    const SPLICE: Duration = Duration::from_millis(500);

    /// The same for media whose sender is not asked for a keyframe when the
    /// route changes: the rest of the group in flight, at most the encoder's
    /// two-second keyframe interval, and a margin.
    const GROUP: Duration = Duration::from_millis(2500);

    /// A clean relay stop with a host there: the viewer's read of the member
    /// carries on through the member's spare on the host. No retraction, so
    /// no tile leaves, and no wait for the library's hold.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_relay_that_stops_is_a_splice_to_the_spare_not_a_leave() {
        let mut w = moving().await;
        w.camera.turn_on();
        w.arrivals.more(30, "no video through the relay").await;
        let (_watching, retracted) = retractions(&w.v.inbound, &camera_path("m"));
        let stopped = Instant::now();
        w.room.relay.stop().await;
        routed_through(&w.v.inbound, &camera_path("m"), w.room.host.origin.hop(), "m never came through the host").await;
        // Past the library's hold on a re-announcement, so a leave would show.
        w.arrivals.more(75, "video stopped when the relay did").await;
        let gap = w.arrivals.longest_gap(stopped);
        eprintln!("splice gap {gap:?}");
        assert_eq!(count(&retracted), 0, "the viewer lost m");
        assert!(gap < SPLICE, "video was gone {gap:?} when the relay stopped");
    }

    /// The control: the same stop with the spare switched off is a leave, as
    /// it always was. Without this the test above could pass for a reason
    /// that has nothing to do with the spare.
    #[tokio::test(flavor = "multi_thread")]
    async fn without_a_spare_the_same_stop_is_a_leave() {
        let mut w = moving_on(Meeting::older().await, false).await;
        w.camera.turn_on();
        w.arrivals.more(30, "no video through the relay").await;
        let (_watching, retracted) = retractions(&w.v.inbound, &camera_path("m"));
        let stopped = Instant::now();
        w.room.relay.stop().await;
        w.arrivals.more(30, "video never came back through the host").await;
        let gap = w.arrivals.longest_gap(stopped);
        eprintln!("no-spare gap {gap:?}");
        assert!(count(&retracted) > 0, "m was never retracted, so the test above proves nothing");
    }

    /// Moving the call to a host and back, on a relay that speaks moq-lite-05
    /// (which holds no spare while the call is on the host) and on one that
    /// carries a price (which does): never a leave, and each gap bounded.
    async fn moves_are_never_a_leave(relay: Meeting) {
        let mut w = moving_on(relay, true).await;
        w.camera.turn_on();
        w.arrivals.more(30, "no video through the relay").await;
        let (_watching, retracted) = retractions(&w.v.inbound, &camera_path("m"));
        for round in 0..2 {
            for host in [true, false] {
                let moved = Instant::now();
                w.to(host, &format!("the call never moved, round {round}")).await;
                w.arrivals.more(30, &format!("video stopped after a move, round {round}")).await;
                let gap = w.arrivals.longest_gap(moved);
                eprintln!("move gap {gap:?} to the {}", if host { "host" } else { "relay" });
                assert!(gap < GAP, "video was gone {gap:?} moving to the {}, round {round}", if host { "host" } else { "relay" });
            }
        }
        assert_eq!(count(&retracted), 0, "the viewer lost m across a move");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_call_moved_to_a_host_and_back_is_never_a_leave() {
        moves_are_never_a_leave(Meeting::older().await).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_call_moved_to_a_host_and_back_over_a_priced_relay_is_never_a_leave() {
        moves_are_never_a_leave(Meeting::start().await).await;
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
        let total = || -> u64 {
            host.sessions.lock().unwrap().iter().map(|session| session.stats().bytes_received.unwrap_or(0)).sum()
        };
        let start = total();
        tokio::time::sleep(over).await;
        (total() - start) * 1000 / over.as_millis() as u64
    }

    /// Bytes a second a connection sends, measured over `over`.
    async fn sent(connection: &moq_tokio::Connection, over: Duration) -> u64 {
        let monitor = connection.monitor();
        let total = || monitor.stats().and_then(|stats| stats.bytes_sent).unwrap_or(0);
        let start = total();
        tokio::time::sleep(over).await;
        (total() - start) * 1000 / over.as_millis() as u64
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
        eprintln!("healthy: spare {spare} B/s, bridging {bridging} B/s");
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
        eprintln!("relay gone: spare {carrying} B/s");
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
    /// what it reads of the member splices to the member's spare on it.
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
        eprintln!("host's own {host_gap:?}, host's view of m {member_gap:?}");
        assert_eq!(count(&host_left), 0, "the viewer lost the host");
        assert_eq!(count(&member_left), 0, "the host lost m");
        // This stand-in host is not a join, so nothing asks its encoder for a
        // keyframe: its own video waits out the group in flight.
        assert!(host_gap < GROUP, "the host's video was gone {host_gap:?}");
        assert!(member_gap < SPLICE, "the host's view of m was gone {member_gap:?}");
    }
}
