//! What the moq stack does when one broadcast can arrive by more than one way.
//!
//! Test-only. A room with a backup host means several live sessions at once,
//! and everything that design rests on is library behaviour rather than ours:
//! that two routes to one publisher are one broadcast, that a viewer can tell
//! which session it is using, what two of our endpoints negotiate, and that
//! what we offer can move between sessions without a redial. Each test pins
//! one of those, so an upgrade that changes it fails here rather than in a
//! call.
//!
//! Real sessions over raw QUIC (`moql://`) on the loopback address, through
//! `moq_tokio`'s own client and server. Two iroh endpoints offer the same ALPN
//! list and land on raw QUIC too, so what is negotiated here should carry over;
//! iroh itself is not exercised. A "relay" is what a host will be: one shared
//! origin handed to every session as both publisher and subscriber.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use moq_net::{announce, broadcast, origin, Hop, Timestamp, Version};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const PATH: &str = "alice";
const TRACK: &str = "video";
/// Big enough that a session carrying it stands out from its keep-alives.
static FRAME: [u8; 1000] = [0; 1000];

/// Long enough for anything on loopback, short enough that a hang fails fast.
const PATIENCE: Duration = Duration::from_secs(5);

fn lite(name: &str) -> Version {
    name.parse().expect("a version this moq-net knows")
}

/// Aborts its task when dropped, so a test that fails leaves nothing behind.
struct Task(JoinHandle<()>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One shared origin behind a listener: a relay, or a host.
struct Relay {
    origin: origin::Producer,
    addr: SocketAddr,
    sessions: Arc<Mutex<Vec<moq_net::Session>>>,
    _accepting: Task,
}

impl Relay {
    /// `versions` empty accepts every version, as a default server does.
    async fn start(versions: &[Version]) -> Relay {
        install_crypto();
        let origin = moq_tokio::origin::spawn();
        let mut config = moq_tokio::server::Config::default();
        config.listen.bind = Some(moq_tokio::listen::Bind::Addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))));
        config.listen.tls.generate = vec!["localhost".to_string()];
        config.listen.version = versions.to_vec();
        config.publisher = Some(origin.consume());
        config.subscriber = Some(origin.clone());
        let mut server = config.init().expect("server config").bind().await.expect("bind");
        let addr = server.local_addr().expect("bound address");
        let sessions = Arc::new(Mutex::new(Vec::new()));
        let kept = sessions.clone();
        let accepting = tokio::spawn(async move {
            while let Some(request) = server.accept().await {
                let kept = kept.clone();
                tokio::spawn(async move {
                    if let Ok(session) = request.ok().await {
                        kept.lock().unwrap().push(session);
                    }
                });
            }
        });
        Relay {
            origin,
            addr,
            sessions,
            _accepting: Task(accepting),
        }
    }

    fn hop(&self) -> Hop {
        self.origin.hop()
    }

    fn url(&self) -> url::Url {
        format!("moql://{}", self.addr).parse().unwrap()
    }

    /// Ends every session at once, the way a relay that dies does, then stops
    /// accepting. The close is given a moment to reach the wire before the
    /// endpoint goes, so peers learn of it now and not at their idle timeout.
    async fn stop(self) {
        for session in self.sessions.lock().unwrap().iter() {
            session.abort(moq_net::Error::Cancel);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn install_crypto() {
    // Both rustls backends are in the tree, so it will not pick one itself;
    // main does the same for the app.
    let _ = moq_tokio::rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// A client as the app builds one, minus iroh. `versions` empty offers all.
fn client(versions: &[Version]) -> moq_tokio::Client {
    install_crypto();
    let mut config = moq_tokio::client::Config::default();
    config.connect.tls.insecure = Some(true);
    config.connect.version = versions.to_vec();
    config.init().expect("client config").with_reconnect(false)
}

async fn established(connection: moq_tokio::Connection) -> moq_tokio::Connection {
    tokio::time::timeout(PATIENCE, connection.established())
        .await
        .expect("connected in time")
        .expect("connected")
}

/// A broadcast with one track, a group every 20 ms while it lives.
struct Publishing {
    broadcast: broadcast::Producer,
    _writing: Task,
}

fn publish(origin: &origin::Producer, announce: bool) -> Publishing {
    let broadcast = origin.create_broadcast(PATH).unwrap();
    let mut track = broadcast.create_track(TRACK, None).unwrap();
    if announce {
        broadcast.announce(origin::Route::default()).unwrap();
    }
    let writing = tokio::spawn(async move {
        for n in 0u64.. {
            if track.write_frame(Timestamp::from_millis(n * 20).unwrap(), FRAME.as_slice()).is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    Publishing {
        broadcast,
        _writing: Task(writing),
    }
}

/// The group sequences a viewer receives, as they arrive. The channel closes
/// when the subscription ends.
fn watch_groups(broadcast: broadcast::Consumer) -> (mpsc::UnboundedReceiver<u64>, Task) {
    let (tx, rx) = mpsc::unbounded_channel();
    let reading = tokio::spawn(async move {
        let Ok(track) = broadcast.track(TRACK) else { return };
        let Ok(mut subscriber) = track.subscribe(None).await else { return };
        while let Ok(Some(group)) = subscriber.recv_group().await {
            if tx.send(group.sequence).is_err() {
                return;
            }
        }
    });
    (rx, Task(reading))
}

async fn next_group(groups: &mut mpsc::UnboundedReceiver<u64>) -> Option<u64> {
    tokio::time::timeout(PATIENCE, groups.recv()).await.expect("a group in time")
}

/// Waits for `count` more groups, all past `after`.
async fn groups_past(groups: &mut mpsc::UnboundedReceiver<u64>, after: u64, count: usize) -> u64 {
    let mut last = after;
    let mut seen = 0;
    while seen < count {
        let sequence = next_group(groups).await.expect("the subscription is still live");
        if sequence > after {
            seen += 1;
        }
        last = last.max(sequence);
    }
    last
}

/// The next update for our path, within `wait`.
async fn update_within(announced: &mut announce::Consumer, wait: Duration) -> Option<announce::Update> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let update = tokio::time::timeout_at(deadline, announced.next()).await.ok()??;
        if update.prefix.as_str() == PATH {
            return Some(update);
        }
    }
}

/// Every update for our path until nothing changes for `quiet`.
async fn settle(announced: &mut announce::Consumer, quiet: Duration) -> Vec<announce::Update> {
    let mut updates = Vec::new();
    while let Some(update) = update_within(announced, quiet).await {
        updates.push(update);
    }
    updates
}

/// The session a route reached us through: the last hop of its chain is the
/// origin that answered our session, a relay's or a host's.
fn through(route: &origin::Route) -> Hop {
    *route.hops.as_slice().last().expect("a remote route names who sent it")
}

/// Two of our endpoints, both with their default version lists, settle on
/// moq-lite-06. A server that stops at moq-lite-05, as an older relay does,
/// gets moq-lite-05 from the same client: the cap is the server's.
#[tokio::test]
async fn our_endpoints_negotiate_lite06_and_an_older_server_caps_it() {
    let current = Relay::start(&[]).await;
    let older = Relay::start(&[lite("moq-lite-05"), lite("moq-lite-04")]).await;
    let client = client(&[]);

    let to_current = established(client.connect(current.url())).await;
    let to_older = established(client.connect(older.url())).await;

    assert_eq!(to_current.version(), Some(lite("moq-lite-06")));
    assert_eq!(to_older.version(), Some(lite("moq-lite-05")));
}

/// One publisher reached through two relays is one broadcast to a viewer on
/// both: announced once, both routes starting with the publisher's own hop,
/// and the last hop naming the relay. When the relay in use dies, the same
/// subscription keeps receiving through the other, and the announcement is
/// re-delivered in place naming it.
#[tokio::test]
async fn one_publisher_through_two_relays_is_one_broadcast_that_fails_over() {
    let a = Relay::start(&[]).await;
    let b = Relay::start(&[]).await;

    let outbound = moq_tokio::origin::spawn();
    let _publishing = publish(&outbound, true);
    let publisher = client(&[]).with_publisher(&outbound);
    let _p_a = established(publisher.connect(a.url())).await;
    let _p_b = established(publisher.connect(b.url())).await;

    let inbound = moq_tokio::origin::spawn();
    let viewer = client(&[]).with_subscriber(inbound.clone());
    let _v_a = established(viewer.connect(a.url())).await;
    let _v_b = established(viewer.connect(b.url())).await;

    let mut announced = inbound.consume().announced();
    let updates = settle(&mut announced, Duration::from_millis(800)).await;
    assert_eq!(updates[0].kind, announce::Kind::Announced, "{updates:?}");
    assert!(
        updates[1..].iter().all(|update| update.kind == announce::Kind::Updated),
        "a second route is an update, never a second broadcast: {updates:?}"
    );
    let route = updates.last().unwrap().route.clone();
    assert_eq!(route.hops.as_slice()[0], outbound.hop(), "the publisher's identity leads");
    assert_eq!(route.hops.len(), 2);
    let serving = through(&route);
    assert!(serving == a.hop() || serving == b.hop());

    let broadcast = inbound.consume().routed_broadcast(PATH).await.unwrap();
    let (mut groups, _reading) = watch_groups(broadcast);
    let before = groups_past(&mut groups, 0, 3).await;

    let (dying, surviving) = if serving == a.hop() { (a, b) } else { (b, a) };
    let surviving_hop = surviving.hop();
    dying.stop().await;

    let update = update_within(&mut announced, PATIENCE).await.expect("the route moves");
    assert_eq!(update.kind, announce::Kind::Updated, "not retracted and re-announced");
    assert_eq!(through(&update.route), surviving_hop);
    assert_eq!(update.route.hops.as_slice()[0], outbound.hop());

    // The same subscription, never re-requested.
    groups_past(&mut groups, before, 5).await;
    drop(surviving);
}

/// A publisher's own price on a session is honoured on moq-lite-06: the viewer
/// prefers the cheaper relay even though the dearer one connected last, which
/// would otherwise win as the newest route.
#[tokio::test]
async fn a_priced_session_loses_to_a_cheaper_one_on_lite06() {
    let cheap = Relay::start(&[]).await;
    let dear = Relay::start(&[]).await;

    let outbound = moq_tokio::origin::spawn();
    let _publishing = publish(&outbound, true);
    let publisher = client(&[]).with_publisher(&outbound);
    let _p_cheap = established(publisher.connect(cheap.url())).await;
    let _p_dear = established(publisher.clone().with_cost(100).connect(dear.url())).await;

    let inbound = moq_tokio::origin::spawn();
    let viewer = client(&[]).with_subscriber(inbound.clone());
    let _v_cheap = established(viewer.connect(cheap.url())).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _v_dear = established(viewer.connect(dear.url())).await;

    let mut announced = inbound.consume().announced();
    let updates = settle(&mut announced, Duration::from_millis(800)).await;
    let route = &updates.last().expect("announced").route;
    assert_eq!(through(route), cheap.hop(), "{updates:?}");
    assert!(route.cost.warm < 100, "{:?}", route.cost);
}

/// Over moq-lite-05 a route carries no cost, and it arrives as free to reach:
/// it outranks an equally long moq-lite-06 route, which pays a default price
/// per link. So with an older relay and a host on the newer version, viewers
/// prefer the older relay without anyone pricing anything, and a price set on
/// the lite-05 session is ignored.
#[tokio::test]
async fn a_lite05_route_outranks_a_lite06_one_of_the_same_length() {
    let older = Relay::start(&[lite("moq-lite-05")]).await;
    let current = Relay::start(&[]).await;

    let outbound = moq_tokio::origin::spawn();
    let _publishing = publish(&outbound, true);
    let publisher = client(&[]).with_publisher(&outbound);
    let _p_older = established(publisher.clone().with_cost(100).connect(older.url())).await;
    let _p_current = established(publisher.connect(current.url())).await;

    let inbound = moq_tokio::origin::spawn();
    let viewer = client(&[]).with_subscriber(inbound.clone());
    let _v_older = established(viewer.connect(older.url())).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _v_current = established(viewer.connect(current.url())).await;

    let mut announced = inbound.consume().announced();
    let updates = settle(&mut announced, Duration::from_millis(800)).await;
    let route = &updates.last().expect("announced").route;
    assert_eq!(through(route), older.hop(), "{updates:?}");
    assert_eq!(route.cost.warm, 0, "{:?}", route.cost);
}

/// A moq-lite-06 link priced at zero beats a moq-lite-05 route of the same
/// length: both are free to reach, but the older one's path is of unknown
/// price and ranks last on that, where the newer one's is known to be free.
/// This is what moving the call to a host rests on: a member dials the chosen
/// host free, and everyone's media then flows through it rather than through
/// the older relay.
#[tokio::test]
async fn a_free_lite06_route_beats_a_lite05_one_of_the_same_length() {
    let older = Relay::start(&[lite("moq-lite-05")]).await;
    let current = Relay::start(&[]).await;

    let outbound = moq_tokio::origin::spawn();
    let _publishing = publish(&outbound, true);
    let publisher = client(&[]).with_publisher(&outbound);
    let _p_older = established(publisher.clone().connect(older.url())).await;
    let _p_current = established(publisher.with_cost(0).connect(current.url())).await;

    let inbound = moq_tokio::origin::spawn();
    let viewer = client(&[]).with_subscriber(inbound.clone());
    let _v_older = established(viewer.clone().connect(older.url())).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _v_current = established(viewer.with_cost(0).connect(current.url())).await;

    let mut announced = inbound.consume().announced();
    let updates = settle(&mut announced, Duration::from_millis(800)).await;
    let route = &updates.last().expect("announced").route;
    assert_eq!(through(route), current.hop(), "{updates:?}");
    assert_eq!(route.cost.warm, 0, "{:?}", route.cost);
}

/// What one session offers of ours: an origin of its own, sharing our
/// publishing identity so every route to us still names one publisher, into
/// which our broadcast is offered or from which it is withdrawn.
struct Gate {
    origin: origin::Producer,
}

/// Our broadcast offered through one gate. Dropping it withdraws the offer.
struct Offer {
    _serving: Task,
}

impl Gate {
    fn new(outbound: &origin::Producer) -> Gate {
        Gate {
            origin: moq_tokio::origin::spawn_config(origin::Config::new(outbound.hop())),
        }
    }

    fn offer(&self, broadcast: &broadcast::Producer) -> Offer {
        let dynamic = self.origin.dynamic(PATH, origin::Route::default()).unwrap();
        let broadcast = broadcast.consume();
        // The handler owns the route: ending it retracts the advertisement.
        let serving = tokio::spawn(async move {
            while let Ok(request) = dynamic.requested_broadcast().await {
                request.accept(broadcast.clone());
            }
        });
        Offer {
            _serving: Task(serving),
        }
    }
}

/// Bytes per second a session sends, measured over `over`.
async fn upload(connection: &moq_tokio::Connection, over: Duration) -> u64 {
    let monitor = connection.monitor();
    let sent = || monitor.stats().and_then(|stats| stats.bytes_sent).expect("the transport counts bytes");
    let start = sent();
    tokio::time::sleep(over).await;
    (sent() - start) * 1000 / over.as_millis() as u64
}

/// The publisher, a gate per relay, and a viewer on both relays.
struct Offering {
    a: Relay,
    b: Relay,
    outbound: origin::Producer,
    publishing: Publishing,
    gate_a: Gate,
    gate_b: Gate,
    p_a: moq_tokio::Connection,
    _p_b: moq_tokio::Connection,
    inbound: origin::Producer,
    _v_a: moq_tokio::Connection,
    _v_b: moq_tokio::Connection,
}

impl Offering {
    async fn new() -> Offering {
        let a = Relay::start(&[]).await;
        let b = Relay::start(&[]).await;
        let outbound = moq_tokio::origin::spawn();
        let publishing = publish(&outbound, false);
        let gate_a = Gate::new(&outbound);
        let gate_b = Gate::new(&outbound);
        let p_a = established(client(&[]).with_publisher(&gate_a.origin).connect(a.url())).await;
        let p_b = established(client(&[]).with_publisher(&gate_b.origin).connect(b.url())).await;
        let inbound = moq_tokio::origin::spawn();
        let viewer = client(&[]).with_subscriber(inbound.clone());
        let v_a = established(viewer.connect(a.url())).await;
        let v_b = established(viewer.connect(b.url())).await;
        Offering {
            a,
            b,
            outbound,
            publishing,
            gate_a,
            gate_b,
            p_a,
            _p_b: p_b,
            inbound,
            _v_a: v_a,
            _v_b: v_b,
        }
    }
}

/// Two sessions stay up for receiving while our broadcast is offered on only
/// one; the other relay never hears of it. The offer then moves to the other
/// session, opened there before it is withdrawn here: a viewer's subscription
/// carries on through the move with no redial and no re-request, and the old
/// session stops uploading.
#[tokio::test]
async fn an_offer_moves_between_live_sessions_without_a_redial() {
    let call = Offering::new().await;
    let offer_a = call.gate_a.offer(&call.publishing.broadcast);

    let mut announced = call.inbound.consume().announced();
    let first = update_within(&mut announced, PATIENCE).await.expect("announced");
    assert_eq!(through(&first.route), call.a.hop());
    let mut at_b = call.b.origin.consume().announced();
    assert!(
        update_within(&mut at_b, Duration::from_millis(500)).await.is_none(),
        "the session we do not offer on carries nothing of ours"
    );

    let broadcast = call.inbound.consume().routed_broadcast(PATH).await.unwrap();
    let (mut groups, _reading) = watch_groups(broadcast);
    let before = groups_past(&mut groups, 0, 3).await;
    let uploading = upload(&call.p_a, Duration::from_millis(500)).await;

    let moved = Instant::now();
    let _offer_b = call.gate_b.offer(&call.publishing.broadcast);
    let at_relay = update_within(&mut at_b, PATIENCE).await.expect("offered at b");
    assert_eq!(at_relay.route.hops.as_slice(), [call.outbound.hop()], "one publisher identity across gates");
    drop(offer_a);

    let update = update_within(&mut announced, PATIENCE).await.expect("the route moves");
    assert_eq!(update.kind, announce::Kind::Updated);
    assert_eq!(through(&update.route), call.b.hop());
    let took = moved.elapsed();
    // Most of this is the announce cursor holding a changed route back
    // (`origin::DEFAULT_UPDATE_HOLD`); the data path does not wait for it.
    assert!(took < Duration::from_secs(2), "the move took {took:?}");

    groups_past(&mut groups, before, 5).await;
    let after = upload(&call.p_a, Duration::from_millis(500)).await;
    assert!(after * 5 < uploading, "the old session still uploads {after} B/s, was {uploading} B/s");
}

/// Withdrawing an offer is not a hang-up. Retracting the only route ends the
/// viewer's broadcast for new requests, but a track already flowing carries on
/// through the withdrawn session, uploading, for as long as the viewer reads
/// it; an offer elsewhere is announced as new rather than spliced in. So a
/// move must open the new offer before closing the old one, and a viewer that
/// sees a retraction must drop what it holds.
#[tokio::test]
async fn a_withdrawn_offer_keeps_its_tracks_in_flight() {
    let call = Offering::new().await;
    let offer_a = call.gate_a.offer(&call.publishing.broadcast);

    let mut announced = call.inbound.consume().announced();
    update_within(&mut announced, PATIENCE).await.expect("announced");
    let broadcast = call.inbound.consume().routed_broadcast(PATH).await.unwrap();
    let (mut groups, _reading) = watch_groups(broadcast);
    let before = groups_past(&mut groups, 0, 3).await;
    let uploading = upload(&call.p_a, Duration::from_millis(500)).await;

    drop(offer_a);
    let retracted = update_within(&mut announced, PATIENCE).await.expect("retracted");
    assert_eq!(retracted.kind, announce::Kind::Retracted);
    let _offer_b = call.gate_b.offer(&call.publishing.broadcast);
    let again = update_within(&mut announced, PATIENCE).await.expect("announced again");
    assert_eq!(again.kind, announce::Kind::Announced, "a new broadcast, not a move");

    groups_past(&mut groups, before, 25).await;
    let after = upload(&call.p_a, Duration::from_millis(500)).await;
    assert!(after * 2 > uploading, "the withdrawn session still uploads: {after} B/s, was {uploading} B/s");

    drop(_reading);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stopped = upload(&call.p_a, Duration::from_millis(500)).await;
    assert!(stopped * 5 < uploading, "once nobody reads, it stops: {stopped} B/s, was {uploading} B/s");
}
