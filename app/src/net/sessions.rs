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
//! **The estimate follows the session carrying our media**, as do the
//! connection figures in the Network tab and in the report we send: that link
//! is what our encoding has to fit. A host with no outgoing session left falls
//! back to its guests' links, which are then what carries its media.

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
pub(super) fn pick(states: &[State], current: Option<usize>, now: Instant, started: Instant) -> Option<usize> {
    if states.first() == Some(&State::Dialing) && now.duration_since(started) < HOLD {
        return None;
    }
    if let Some(current) = current.filter(|&at| states.get(at).is_some_and(|state| state.live())) {
        let held = states[..current]
            .iter()
            .position(|state| matches!(state, State::Live(since) if now.duration_since(*since) >= HOLD));
        return Some(held.unwrap_or(current));
    }
    states.iter().position(|state| state.live())
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

    /// Offers everything we publish under `me/` through this gate, as it is
    /// announced, until the returned task is dropped: our camera, our control
    /// broadcast, and a shared screen when there is one.
    pub(super) fn offer(&self, outbound: &moq_net::origin::Producer, me: &str) -> Abort {
        let gate = self.origin.clone();
        let route = moq_net::origin::Route::default().with_hops(self.publisher.clone());
        let source = outbound.consume();
        let ours = format!("{me}/");
        Abort(tokio::spawn(async move {
            let mut announced = source.announced();
            // Each path's offer, which ends (and withdraws) when dropped.
            let mut offered: std::collections::HashMap<String, Abort> = std::collections::HashMap::new();
            while let Some(update) = announced.next().await {
                let path = update.prefix.as_str().to_string();
                if !path.starts_with(&ours) {
                    continue;
                }
                match update.kind {
                    moq_net::announce::Kind::Announced => {
                        let dynamic = match gate.dynamic(path.as_str(), route.clone()) {
                            Ok(dynamic) => dynamic,
                            Err(error) => {
                                tracing::warn!("could not offer {path}: {error:#}");
                                continue;
                            }
                        };
                        let source = source.clone();
                        let served = path.clone();
                        offered.insert(
                            path,
                            Abort(tokio::spawn(async move {
                                // The handler owns the route: ending it
                                // withdraws the offer.
                                while let Ok(request) = dynamic.requested_broadcast().await {
                                    match source.request_broadcast(served.as_str()).await {
                                        Ok(broadcast) => request.accept(broadcast),
                                        Err(error) => request.reject(error),
                                    }
                                }
                            })),
                        );
                    }
                    moq_net::announce::Kind::Retracted => {
                        offered.remove(&path);
                    }
                    moq_net::announce::Kind::Updated => {}
                }
            }
        }))
    }
}

/// What one session reports to the set.
#[derive(Debug)]
enum Event {
    Live,
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
}

/// One session of the set.
struct Member {
    target: Target,
    gate: Option<Gate>,
    state: State,
    error: Option<String>,
    /// Ever up, so a return reads as one.
    joined: bool,
    session: Option<Session>,
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
    } = args;
    let (events, mut heard) = futures_channel::mpsc::unbounded::<(usize, Event)>();
    let dial = {
        let inbound = inbound.clone();
        let outbound = outbound.clone();
        move |index: usize, member: &Member| -> Session {
            let mut client = client.clone();
            if member.target.host.is_some() {
                client = client.with_cost(HOST_COST);
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
                            say(Event::Live);
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
            session: None,
        })
        .collect();
    let hosts = members.len() - 1;
    tracing::info!("dialling the relay and {hosts} saved hosts");
    for (index, member) in members.iter_mut().enumerate() {
        member.session = Some(dial(index, member));
    }
    let path = members[0].target.url.path().trim_matches('/').to_string();
    let relay_label = redacted(&members[0].target.url);

    Peer {
        closed: Box::pin(async move {
            let started = Instant::now();
            let mut carrier: Option<usize> = None;
            let mut offer: Option<Abort> = None;
            // Held, and replaced only below: dropping it stops what it feeds.
            let mut _following: Option<Following> = None;
            let mut on_guests = false;
            let mut said: Option<Via> = None;
            let mut down_said = false;
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
                        let name = |member: &Member| match &member.target.host {
                            None => relay_label.clone(),
                            Some(id) => format!("host {id}"),
                        };
                        match event {
                            Event::Live => {
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
                                member.session = None;
                                member.error = Some(error);
                                member.state = State::Resting(Instant::now() + REST);
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
                            member.session = Some(dial(index, member));
                        }
                    }
                }

                let states: Vec<State> = members.iter().map(|member| member.state).collect();
                // Until the relay has answered or failed once, a host with
                // nobody else up is still joining, not cut off.
                let relay_tried = states.first().is_some_and(|state| *state != State::Dialing);
                let next = pick(&states, carrier, now, started);
                if next.is_some() && next != carrier {
                    let index = next.unwrap_or_default();
                    let member = &members[index];
                    // Make before break: offered there before withdrawn here.
                    // The new offer is made by a task as our paths are read
                    // back, so the old one is kept a moment longer rather than
                    // dropped in the same instant; a session that died carries
                    // nothing meanwhile.
                    if let Some(gate) = &member.gate {
                        let made = gate.offer(&outbound, &me);
                        if let Some(old) = offer.replace(made) {
                            tokio::spawn(async move {
                                tokio::time::sleep(MOVE_OVERLAP).await;
                                drop(old);
                            });
                        }
                    }
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
                    tracing::info!("still hosting; no way to the relay");
                    _following = guests.clone().map(|guests| follow_guests(guests, &estimate));
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
        let pick = |states: &[State], current, now| pick(states, current, now, start);
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
            crypto();
            let mut config = moq_tokio::server::Config::default();
            config.listen.bind = Some(moq_tokio::listen::Bind::Addr(([127, 0, 0, 1], port).into()));
            config.listen.tls.generate = vec!["localhost".to_string()];
            config.publisher = Some(origin.consume());
            config.subscriber = Some(origin.clone());
            let mut server = config.init().unwrap().bind().await.unwrap();
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
            Meeting {
                origin,
                port,
                sessions,
                _accepting: accepting,
            }
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
        _camera: moq_net::broadcast::Producer,
        _writing: Abort,
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
        let outbound = moq_tokio::origin::spawn();
        let camera = outbound.create_broadcast(camera_path(me)).unwrap();
        let mut track = camera.create_track("video", None).unwrap();
        camera.announce(moq_net::origin::Route::default()).unwrap();
        let writing = Abort(tokio::spawn(async move {
            for n in 0u64.. {
                if track.write_frame(moq_net::Timestamp::from_millis(n * 20).unwrap(), [0u8; 100].as_slice()).is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }));
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
            local: Arc::new(PeerStats::default()),
            status,
            keyframe: Arc::new(Keyframes::default()),
            room: "room team".to_string(),
        });
        let (end, ended) = tokio::sync::oneshot::channel();
        let joined = Abort(tokio::spawn(async move {
            let _ = end.send(peer.closed.await);
        }));
        Member {
            inbound,
            statuses,
            ended,
            _camera: camera,
            _writing: writing,
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

    #[tokio::test(flavor = "multi_thread")]
    async fn our_media_is_offered_on_the_relay_alone_while_it_is_up() {
        let relay = Meeting::start().await;
        let host = Meeting::start().await;
        let mut m = member("m", vec![relay_at(relay.url()), host_at('a', host.url())]);
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
        let (_accepting, _) = accept(server, Some(hosting), Some(estimates), Arc::new(Metrics::default()));
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
}
