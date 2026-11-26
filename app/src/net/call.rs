//! A call: one peer directly, or a room of several on a relay.
//!
//! The shape is the loopback's with the pipe replaced by a real connection.
//! `--listen` runs a `moq_tokio::Server` and `--dial` runs a
//! `moq_tokio::Client` against it; after the handshake the two sides are
//! identical, and so is everything below the transport.
//!
//! **Two modes, side by side.** A 1:1 call is peer to peer with no relay: the
//! listening side is its own tiny server, two processes, one key typed into
//! the other. `--relay` instead joins a **room** on a relay, which everyone in
//! the room is a client of. Each participant publishes once and the relay fans
//! out, so the uplink cost does not grow with the room. To this module a relay
//! is just a server we dial, which is why `--dial` and `--relay` share
//! [`connect`].
//!
//! **`--host` is the third mode: a room whose meeting place is this app.** It
//! accepts as `--listen` does, through the same [`serve`], but on one origin
//! that its guests and its own broadcasts share, behind a door (see
//! [`super::door`]); and it is a room, not a call with one peer. Its guests
//! join it as they would a relay's room, which is why they need nothing new.
//!
//! **A join with somewhere else to meet holds several sessions**: the relay
//! and the room's hosts together, through [`sessions`]. A join with no hosts
//! and hosting off is the one session through [`connect`] it always was.
//!
//! Every arm converges on [`Peer`], so the claim above is structural rather
//! than a description: anything per-connection is reached the same way
//! whichever side established it.
//!
//! Each side keeps **two origins** -- `outbound` for what we publish, `inbound`
//! for what arrives. They are separate `Origin`s, so both peers can publish at
//! the same well-known path without colliding, and neither needs to know the
//! other's id to subscribe. Rooms are different: there each participant
//! publishes under its own id (see [`participant_of`]).
//!
//! What we hand the encoder is the same [`FrameFeed`] the loopback uses: the
//! pump's fan-out tap. Nothing in `camera/` knows whether a peer is involved.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_channel::mpsc::UnboundedSender;
use futures_util::StreamExt as _;

#[path = "sessions.rs"]
mod sessions;

use super::adapt;
use super::arrival;
use super::audio;
use super::quiet;
use super::door;
use super::runtime::runtime;
use super::share::{self, ShareWish};
use super::status::RemoteStatus;
use super::{Feed, ParticipantEvent};
use crate::args::Peering;
use super::Metrics;
use super::follow;
use super::forwarded::Forwarded;
use super::metrics::{Ask, Clock, Counts, FpsMeter, Path, PeerStats, Request, classify};
use super::quality::{self, Link, Seen};
use crate::audio::Controls;
use crate::camera::{FrameFeed, FrameSlot, encode_size, from_decoded};

/// Aborts a task when it goes out of scope.
///
/// `tokio::spawn` detaches, and dropping a `JoinHandle` leaves the task
/// running -- so a helper spawned inside a task that is itself aborted would
/// outlive its parent and keep writing. Every per-call helper wants this.
struct Abort(tokio::task::JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What a connected peer is, whichever side established it.
///
/// The two arms differ only in how they reach this: one dials a client, the
/// other accepts as a server. Everything after is identical -- the module doc
/// has said so since the first call worked -- and before this type existed the
/// claim was true of the media and false of the connection, because the arms
/// converged on a bare "future that resolves when it ends". That erased the
/// connection itself, so the first thing to need it (a bandwidth estimate, for
/// rate control) had to be wired inside one arm as a special case.
///
/// Anything per-connection belongs here rather than in a branch:
/// Connection stats for rtt and loss came next (`stats`).
struct Peer {
    /// Resolves when the connection ends, with why.
    closed: Pin<Box<dyn Future<Output = String> + Send>>,
    /// The transport's own send-bitrate estimate, for a side that has one
    /// lasting the whole attempt, to be mirrored into the call's budget. A
    /// listener has none; its sessions feed the budget as they come.
    bandwidth: Option<moq_net::bandwidth::Consumer>,
    /// Connection figures, when this side has a handle that outlives a single
    /// session. The listening side reports its own from inside the accept loop
    /// instead, since its session is what it has.
    stats: Option<moq_tokio::connection::Monitor>,
}

/// Samples which way our iroh traffic goes, once a second, for as long as the
/// attempt lasts; logs each change, so a log answers the question afterwards.
///
/// From the endpoint's byte counters: see [`Path`] for why
/// those and not the connection's paths, and for the relay-trickle caveat.
async fn watch_path(endpoint: moq_tokio::iroh::Endpoint, metrics: Arc<Metrics>) {
    let read = || {
        let socket = &endpoint.metrics().socket;
        Counts {
            direct: socket.send_ipv4.get()
                + socket.send_ipv6.get()
                + socket.recv_data_ipv4.get()
                + socket.recv_data_ipv6.get(),
            relay: socket.send_relay.get() + socket.recv_data_relay.get(),
        }
    };
    let mut prev = read();
    let mut shown = Path::Unknown;
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
    // The first tick is immediate and would compare a sample with itself.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let next = read();
        let path = classify(&prev, &next);
        prev = next;
        // A quiet second says nothing about the path, so the last answer
        // stands: alone in a room nothing flows for minutes, and a word that
        // blinks out is read as the connection dropping. The attempt's end
        // clears it.
        if path == Path::Unknown || path == shown {
            continue;
        }
        shown = path;
        metrics.set_path(path);
        match path {
            Path::Direct => tracing::info!("iroh path: direct"),
            Path::Relayed => tracing::info!("iroh path: via n0 relay"),
            Path::Mixed => tracing::info!("iroh path: mixed"),
            Path::Unknown => {}
        }
    }
}

/// Turns what the encode loops count into a rate per stream, once a second,
/// for as long as the attempt lasts.
///
/// Timed by the elapsed time actually measured rather than the nominal second,
/// so a late tick does not read as a burst. Writes atomics only: nothing here
/// wakes the page, which samples [`Metrics`] on its own schedule.
///
/// A host's forwarded traffic is read on the same tick, before the second is
/// kept, so each second's figures belong together; its summary is logged when
/// this ends with the attempt.
async fn meter_sent(metrics: Arc<Metrics>, forwarded: Option<Arc<Forwarded>>) {
    let _summary = forwarded.clone().map(Summarise);
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = ticker.tick().await;
    loop {
        let now = ticker.tick().await;
        let elapsed = now.duration_since(last);
        if let Some(forwarded) = &forwarded {
            metrics.set_hosting(forwarded.sample(elapsed, metrics.estimate_kbps()));
        }
        metrics.tick_sent(elapsed);
        last = now;
    }
}

/// Logs what hosting came to when the meter is dropped, which is how an
/// attempt ends: aborted, not returned from.
struct Summarise(Arc<Forwarded>);

impl Drop for Summarise {
    fn drop(&mut self) {
        if let Some(summary) = self.0.summary() {
            tracing::info!("{summary}");
        }
    }
}

/// Everything [`connect`] needs, named rather than positional.
struct Connect<'a> {
    config: &'a moq_tokio::client::Config,
    endpoint: &'a moq_tokio::iroh::Endpoint,
    outbound: &'a moq_net::origin::Producer,
    inbound: moq_net::origin::Producer,
    /// A peer's `iroh://<id>`, or a relay's room URL.
    url: url::Url,
    /// Who or what we are connected to, for the log.
    label: String,
    /// The room's label when this is a room on a relay, `None` for one peer.
    room: Option<String>,
    status: UnboundedSender<RemoteStatus>,
    keyframe: Arc<Keyframes>,
}

/// Set when a session comes back, read by the encode loops: a subscriber that
/// rejoins mid-stream cannot decode until a keyframe arrives.
///
/// **One flag per encode loop.** Each loop `swap`s its flag back to false as
/// it acts on it, so a single flag read by the camera and the screen would
/// give the keyframe to whichever loop looked first and leave the other
/// frozen.
#[derive(Default)]
pub(super) struct Keyframes {
    pub camera: std::sync::atomic::AtomicBool,
    pub screen: std::sync::atomic::AtomicBool,
}

impl Keyframes {
    /// Asks every loop for a keyframe.
    fn ask(&self) {
        self.camera.store(true, std::sync::atomic::Ordering::Relaxed);
        self.screen.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Dials out and keeps redialling: a peer directly, or a room on a relay.
///
/// **One function for both, because to the stack they are the same thing.** A
/// relay is just a server we publish into and subscribe from, and a peer
/// running `--listen` is too. What differs is only what "connected" means to
/// the person watching, which `room` decides.
fn connect(args: Connect<'_>) -> Result<Peer, String> {
    let Connect {
        config,
        endpoint,
        outbound,
        inbound,
        url,
        label,
        room,
        status,
        keyframe,
    } = args;
    let client = config
        .clone()
        .init()
        .map_err(|error| format!("could not build a client: {error:#}"))?
        .with_iroh(endpoint.clone());
    let mut refusals = Refusals::for_url(&url);
    let mut session = client
        .with_publisher(outbound)
        .with_subscriber(inbound)
        .connect(url);
    // Taken before the session moves into the future below. It survives a
    // reconnect, so the estimate follows the link across one rather than being
    // restarted. Toward a relay this is our own uplink to it, which is the
    // right thing to adapt to: everyone downstream is the relay's problem.
    let bandwidth = Some(session.send_bandwidth());
    let stats = Some(session.monitor());
    Ok(Peer {
        // **The status stream, not `closed()`.** They end at the same place --
        // the error the retry loop gives up with -- but this one also says when
        // the link went away and when it came back. Without it a drop is
        // invisible: the loop underneath has always redialled, it just did it
        // silently, and the interface went on claiming a connection that was
        // not there.
        closed: Box::pin(async move {
            // With one peer, the first connect is the subscriber's to report:
            // it arrives when the peer's video does, which is what "connected"
            // means to a person watching a tile. In a room the session itself
            // is the news, since nobody else may be there yet.
            let mut connected = false;
            loop {
                match session.status().await {
                    Ok(moq_tokio::Status::Connected) => {
                        if let Some(refusals) = refusals.as_mut() {
                            refusals.connected(std::time::Instant::now());
                        }
                        if connected {
                            tracing::info!("reconnected to {label}");
                            keyframe.ask();
                        } else if room.is_some() {
                            tracing::info!("joined {label}");
                        }
                        let next = match &room {
                            Some(room) => Some(RemoteStatus::InRoom { room: room.clone(), via: None }),
                            None if connected => Some(RemoteStatus::Connected {
                                peer: Some(label.clone()),
                            }),
                            None => None,
                        };
                        if let Some(next) = next {
                            let _ = status.unbounded_send(next);
                        }
                        connected = true;
                    }
                    Ok(moq_tokio::Status::Disconnected) => {
                        if refusals.as_mut().is_some_and(|refusals| refusals.dropped(std::time::Instant::now())) {
                            tracing::warn!("{label} turned us away: the session ended as it began, twice");
                            return TURNED_AWAY.to_string();
                        }
                        tracing::warn!("the session dropped; redialling {label}");
                        let _ = status.unbounded_send(RemoteStatus::Reconnecting);
                    }
                    // `Status` is non-exhaustive: a variant added upstream is
                    // not news we can act on.
                    Ok(_) => {}
                    Err(error) => return format!("{error:#}"),
                }
            }
        }),
        bandwidth,
        stats,
    })
}

/// How a session that was turned away ends, in place of the transport's
/// words; [`explain`] says it to the person.
const TURNED_AWAY: &str = "turned away: the session ended as soon as it began";

/// How briefly a session must last to look turned away: the stack's own bar
/// for a session that counts as healthy, its first backoff delay.
const BRIEF: std::time::Duration = std::time::Duration::from_secs(1);

/// **A heuristic for a refusal over iroh**, where the stack cannot see one.
///
/// A host that turns a guest away closes the session with the protocol's
/// "unauthorized" code, which the client is meant to treat as final. Over raw
/// QUIC it does. Over iroh the code is lost on the way up (web-transport-iroh
/// 0.7 reports any application close as a bare connection error), so the
/// client takes the refusal for a drop and redials it for the whole retry
/// budget: thirty seconds of "reconnected" and "dropped" before a failure that
/// never says refused.
///
/// So this reads the shape instead: the session came up (the host is there
/// and answered) and ended within [`BRIEF`], twice running. **What it can
/// misread**: a host that crashes or quits within a second of accepting us,
/// twice, or a link that dies the instant it forms, twice; either way that
/// host was not going to carry a call. A session that lasted is never counted,
/// and one that lasts resets the count. Only for `iroh://`: every other
/// transport reports the code, and the stack's own answer is better than this.
#[derive(Debug, Default)]
pub(super) struct Refusals {
    since: Option<std::time::Instant>,
    brief: u8,
}

impl Refusals {
    /// The rule for sessions to `url`: `None` where the stack sees refusals.
    pub(super) fn for_url(url: &url::Url) -> Option<Self> {
        (url.scheme() == "iroh").then(Self::default)
    }

    pub(super) fn connected(&mut self, now: std::time::Instant) {
        self.since = Some(now);
    }

    /// Whether this drop makes the session one that was turned away.
    pub(super) fn dropped(&mut self, now: std::time::Instant) -> bool {
        match self.since.take() {
            Some(since) if now.duration_since(since) < BRIEF => self.brief += 1,
            Some(_) => self.brief = 0,
            // A drop without a connection is not a session that began.
            None => {}
        }
        self.brief >= 2
    }
}

/// Everything [`serve`] needs, named rather than positional.
struct Serve<'a> {
    config: &'a moq_tokio::client::Config,
    endpoint: &'a moq_tokio::iroh::Endpoint,
    /// What every accepted session is offered.
    publisher: moq_net::origin::Consumer,
    /// Where every accepted session's broadcasts land.
    subscriber: moq_net::origin::Producer,
    /// The room and its door, when this side hosts one; `None` lets anyone in,
    /// as a 1:1 listener always has.
    door: Option<door::Hosting>,
    /// Where each session's bandwidth estimate goes, or `None` when the
    /// guests' links must not steer what we send; see [`host_alongside`].
    estimates: Option<Arc<adapt::Estimates>>,
    metrics: Arc<Metrics>,
    /// Where a host counts what it sends each guest; `None` for a listener.
    forwarded: Option<Arc<Forwarded>>,
    /// Where each guest's session hop is kept while it lasts, so a route
    /// straight from a guest reads as direct; `None` where nothing reads it.
    guests: Option<Arc<arrival::Guests>>,
    /// Where the last time any guest's session heard anything is kept, for a
    /// join beside this to tell its own link from its relay's; `None` where
    /// nothing reads it.
    heard: Option<Arc<quiet::Heard>>,
}

/// Starts accepting sessions on our endpoint, for as long as the returned
/// task lives. Each message on the receiver is `true` for a session that
/// began and `false` for one that ended; the channel closing says the server
/// did.
async fn serve(
    args: Serve<'_>,
) -> Result<(Abort, futures_channel::mpsc::UnboundedReceiver<bool>), String> {
    let Serve {
        config,
        endpoint,
        publisher,
        subscriber,
        door,
        estimates,
        metrics,
        forwarded,
        guests,
        heard,
    } = args;
    let mut server_config = moq_tokio::server::Config::default();
    server_config.listen.bind = Some(moq_tokio::listen::Bind::Addr(
        std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0)),
    ));
    server_config.listen.tls.generate = vec!["localhost".to_string()];
    server_config.quic = config.quic.clone();
    server_config.iroh = Some(endpoint.clone());
    // The origins stay on the server rather than moving to each request:
    // `accept` attaches them.
    server_config.publisher = Some(publisher);
    server_config.subscriber = Some(subscriber);
    let server = server_config
        .init()
        .map_err(|error| format!("could not build a server: {error:#}"))?
        .bind()
        .await
        .map_err(|error| format!("could not listen: {error:#}"))?;
    Ok(accept(server, door, estimates, metrics, forwarded, guests, heard))
}

/// The accept loop, apart from the server it runs on so it can be driven over
/// a plain local socket as well as iroh.
///
/// **Our own accept loop, rather than `serve_both`.** That convenience method
/// is exactly this loop, plus a private helper that does `request.ok()` and
/// waits -- but it never hands the session back, and the session is where a
/// bandwidth estimate lives. Without one, a callee sends at its opening rate
/// for the whole call however bad the link gets.
///
/// **The sessions belong to this task.** They used to be spawned loose, so
/// aborting the loop at the end of an attempt stopped new sessions and left
/// the accepted ones running: a listener that hung up stayed connected
/// underneath, serving its origins to whoever was still on the line. Held in a
/// `JoinSet` here, they end with the loop, and with the call.
fn accept(
    mut server: moq_tokio::Listener,
    door: Option<door::Hosting>,
    estimates: Option<Arc<adapt::Estimates>>,
    metrics: Arc<Metrics>,
    forwarded: Option<Arc<Forwarded>>,
    guests: Option<Arc<arrival::Guests>>,
    heard: Option<Arc<quiet::Heard>>,
) -> (Abort, futures_channel::mpsc::UnboundedReceiver<bool>) {
    let (events, receiver) = futures_channel::mpsc::unbounded::<bool>();
    let task = Abort(tokio::spawn(async move {
        let mut sessions = tokio::task::JoinSet::new();
        // `accept` alone is awaited, never raced: handshakes in flight are
        // parked on the server, and a loop that dropped its accept to do
        // something else could lose exactly the reconnection being waited
        // for. Finished sessions are reaped as each new one arrives instead.
        while let Some(request) = server.accept().await {
            while sessions.try_join_next().is_some() {}
            // Checked before accepting, so someone turned away never sees
            // the room: not who is in it, not a frame. Which check failed is
            // logged; what they presented is not.
            if let Some(refusal) = door.as_ref().and_then(|door| door.admits(request.path(), request.query()).err()) {
                tracing::warn!("turned a guest away: they {refusal}");
                sessions.spawn(async move {
                    let _ = request.reject(moq_tokio::server::Reject::Unauthorized).await;
                });
                continue;
            }
            let _ = events.unbounded_send(true);
            let request = match &forwarded {
                Some(forwarded) => request.with_stats(forwarded.guest()),
                None => request,
            };
            let estimates = estimates.clone();
            let metrics = metrics.clone();
            let events = events.clone();
            let heard = heard.clone();
            // The hop the guest declared, kept while its session lives.
            let declared = guests.clone().zip(request.peer_hop().map(|hop| hop.id()).filter(|&hop| hop != 0));
            if let Some((guests, hop)) = &declared {
                guests.add(*hop);
            }
            sessions.spawn(async move {
                let session = request.ok().await;
                // Ends with this task, so it never outlives the session.
                let _sampling = match (&session, heard) {
                    (Ok(session), Some(heard)) => Some(Abort(tokio::spawn(sample_heard(session.clone(), heard)))),
                    _ => None,
                };
                match (session, estimates) {
                    (Ok(session), Some(estimates)) => adapt::forward(session, estimates, metrics).await,
                    (Ok(session), None) => {
                        let _ = session.closed().await;
                    }
                    (Err(error), _) => tracing::warn!("a session failed to start: {error:#}"),
                }
                if let Some((guests, hop)) = declared {
                    guests.remove(hop);
                }
                let _ = events.unbounded_send(false);
            });
        }
    }));
    (task, receiver)
}

/// Notes in `heard` whenever a guest's session receives anything, on the
/// join's own cadence: a host's evidence that its link works while its relay
/// says nothing. Holds a handle on the session only while it is sampled.
async fn sample_heard(session: moq_net::Session, heard: Arc<quiet::Heard>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = None;
    loop {
        ticker.tick().await;
        let bytes = session.stats().bytes_received;
        if bytes.is_some() && bytes != last {
            heard.heard(std::time::Instant::now());
            last = bytes;
        }
    }
}

/// Query parameters that carry a way in: a relay's token, a host's door, and
/// the hosts an invite lists, each with its door.
const SECRET_PARAMS: [&str; 3] = ["jwt", super::door::PARAM, super::door::HOST_PARAM];

/// The URL with any token's value blanked, for logs and labels.
///
/// **A room URL can carry its invitation** (`?jwt=…`, the relay's token,
/// `?door=…`, a hosted room's, or `?host=…`, the hosts an invite lists), and
/// anyone holding it can join that room, so it must never be written anywhere
/// a log or a screenshot might travel. Only our own lines are covered;
/// moq-tokio 0.19's own connection lines print scheme, host and port only
/// (read in its `connect.rs`, `Endpoint`).
pub(crate) fn redacted(url: &url::Url) -> String {
    let mut clean = url.clone();
    let secret = |key: &str| SECRET_PARAMS.contains(&key);
    if url.query_pairs().any(|(key, _)| secret(&key)) {
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .map(|(key, value)| {
                // Plain ASCII: a URL would percent-encode an ellipsis.
                let value = if secret(&key) { "redacted".to_string() } else { value.into_owned() };
                (key.into_owned(), value)
            })
            .collect();
        clean.query_pairs_mut().clear().extend_pairs(kept);
    }
    clean.to_string()
}

/// A call's final error, in words where we know what it means.
///
/// The relay refusing a token arrives as a closed session (`code=6
/// reason=unauthorized`), which moq-native (now moq-tokio) retried like any drop until its
/// budget runs out -- it does not tell a refusal from a flaky link until then.
/// A host turning a guest away at its door closes the session the same way;
/// moq-tokio 0.19 recognises that close and gives up at once, and what
/// reaches us is then the bare word (seen in this module's local-socket test).
/// What reaches us at the end is transport text; this says what to do.
fn explain(error: String) -> String {
    if is_refusal(&error) {
        "the room refused this link: its token or door is missing, expired, or for another room".to_string()
    } else {
        error
    }
}

/// Whether a call's final error is a refusal: a token or a door not taken,
/// which no amount of redialling changes.
fn is_refusal(error: &str) -> bool {
    error.contains("reason=unauthorized") || error.trim() == "unauthorized" || error == TURNED_AWAY
}

/// How a room reads in the status line: its path, and the relay it is on.
///
/// The path *is* the room -- the relay roots each session at the path it
/// connected with -- so two URLs differing only in host are two rooms.
pub(super) fn room_label(url: &url::Url) -> String {
    let path = url.path().trim_matches('/');
    let host = url.host_str().unwrap_or("the relay");
    // An iroh relay's host is a 64-character key; the front is enough to tell
    // two apart on screen, and the log has the whole URL.
    let host = if url.scheme() == "iroh" && host.len() > 12 {
        &host[..12]
    } else {
        host
    };
    if path.is_empty() {
        format!("the relay at {host}")
    } else {
        format!("room {path} on {host}")
    }
}

/// What a participant publishes their own camera and microphone under.
///
/// The `.hang` suffix is load-bearing: catalog-format detection keys off it,
/// and without it a subscriber finds nothing and says so only by timing out. It
/// therefore has to stay **last**, which is why the participant's id goes in
/// front of it rather than behind.
///
/// One broadcast per (participant, role). A shared screen is [`SCREEN`]: a
/// second source from one person, not a second person.
/// Audio rides here with the camera because the two together are one
/// participant's live presence.
const CAMERA: &str = "camera.hang";

/// Where `participant` publishes.
fn camera_path(participant: &str) -> String {
    format!("{participant}/{CAMERA}")
}

/// What a participant publishes a shared screen under, beside their camera.
///
/// Shaped exactly like the camera's broadcast (one H.264 rendition, track
/// [`TRACK`]), so it is told apart by this path and never by its catalog. The
/// `.hang` suffix stays last for the same reason as [`CAMERA`]'s. A build that
/// predates sharing reads this path as nobody (`participant_of` is `None`), so
/// it sees no share and no error.
const SCREEN: &str = "screen.hang";

/// Where `participant` publishes their screen.
pub(super) fn screen_path(participant: &str) -> String {
    format!("{participant}/{SCREEN}")
}

/// Whose shared screen an announced path is, the twin of [`participant_of`].
fn sharer_of(path: &str) -> Option<&str> {
    let id = path.strip_suffix(SCREEN)?.strip_suffix('/')?;
    (!id.is_empty()).then_some(id)
}

/// A shared screen's delivery priority: above camera video
/// ([`VIDEO_PRIORITY`]), because the screen is what the room is looking at,
/// and below audio (80) and control ([`CONTROL_PRIORITY`]).
pub(super) const SCREEN_PRIORITY: u8 = 70;

/// Where a participant says what they are doing, rather than shows it.
///
/// **A broadcast of its own, not a track inside the camera's.** Being in a call
/// is not the same as publishing video -- someone with the camera off is still
/// here -- and the `.hang` suffix deliberately does not appear, because that is
/// what catalog-format detection keys off and this is not media.
const CONTROL: &str = "control";

/// The document inside it.
const PRESENCE: &str = "presence.json";

/// How long a departed participant's reader may keep running to deliver their
/// last word. Generous: it only ever runs out on a reader that hears nothing.
const GOODBYE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long `Leaving` is given to reach the relay before the broadcast it is
/// carried on is finished.
const GOODBYE_SAY: std::time::Duration = std::time::Duration::from_millis(150);

/// How long the connection is kept after we finish our broadcasts, so the
/// finishes reach the relay before the line closes behind them.
const GOODBYE_FLUSH: std::time::Duration = std::time::Duration::from_millis(250);

/// How much a control message preempts media. Matches what `hang` gives its own
/// catalog, which is the same kind of traffic: small, structural, and useless
/// late.
const CONTROL_PRIORITY: u8 = 100;

/// What a participant says about themselves.
///
/// **State, not messages.** The newest value wins and older ones are dropped, so
/// there is no ordering to get right, nothing to acknowledge and nothing to
/// retry -- a peer that misses an intermediate value still converges on the
/// right answer. That is what `moq-json`'s snapshot mode is for, and it is why
/// this is the cheap shape rather than a compromise.
///
/// Everything here is about the person, not the media: whether they are on the
/// call, and whether their camera and microphone are on *by their choice*. A
/// camera that is off is not a stalled stream, and only the sender can say
/// which it is. Roles, candidacy and topology opinions extend this same
/// document when they come, rather than inventing their own.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Presence {
    state: State,
    /// Absent reads as on: a peer that predates the field never turned it off.
    #[serde(default = "on")]
    camera: bool,
    #[serde(default = "on")]
    mic: bool,
    /// How our camera clock maps to the wall clock, refreshed every couple of
    /// seconds; what lets a peer measure each frame's delay. See `Clock`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clock: Option<Clock>,
    /// The subroom they are in; absent is the main room. Absent rather than
    /// `null` there, so a document from the main room is byte-for-byte what a
    /// build without subrooms sends, and such a build reads as the main room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    room: Option<String>,
    /// What we are asking of single peers: an invite to our room, a mute.
    /// Absent when empty, so a document with no requests is what a build
    /// without them sends, and such a build ignores the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requests: Vec<Request>,
    /// Whether they are sharing a screen. Absent is not sharing, so a document
    /// from someone not sharing is what a build without shares sends.
    ///
    /// The announce of the screen's broadcast says this too, but presence says
    /// it in the same document as the sharer's room. And the share goes with
    /// the control broadcast: a share whose sender left can linger on the
    /// relay for a few seconds, so a receiver drops the share's tile when the
    /// sharer's control broadcast goes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    sharing: bool,
    /// What they call themselves. Absent is no name, so a document without
    /// one is what a build without names sends, and such a build ignores the
    /// field. **A claim, not an identity**: cleaned again on arrival, shown
    /// only, and never a key -- everything stays keyed on the endpoint id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// How their connection is doing, by their own account. Absent is not
    /// reporting: an older build, or someone who turned the connection
    /// features off, so a document without it is byte-for-byte what a build
    /// without reports sends. **Read leniently**: anything unreadable here is
    /// "not reporting", never a failed document, because one parse error
    /// ends listening to that peer.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "quality::lenient_link")]
    link: Option<Link>,
    /// What they get from each other peer, keyed by endpoint id. Absent when
    /// empty, and read entry by entry as leniently as `link`.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty",
        deserialize_with = "quality::lenient_seen"
    )]
    seen: std::collections::BTreeMap<String, Seen>,
    /// That they host this room, and how to come in: present only while they
    /// serve it, so a document from someone not hosting is byte-for-byte what
    /// a build without hosting sends, and such a build ignores the field.
    /// **Read leniently**, as `link` is, and it carries a door: `Offer`'s
    /// `Debug` leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "door::lenient_offer")]
    host: Option<door::Offer>,
    /// Where they follow the call to, while they have moving the call
    /// switched on: absent otherwise, so a document without it is
    /// byte-for-byte what a build without moves sends, and such a build
    /// ignores the field. **Read leniently** (`moves::lenient`): a bad value
    /// costs only itself.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "super::moves::lenient")]
    moved: Option<super::moves::CallOn>,
}

impl Default for Presence {
    fn default() -> Self {
        Self {
            state: State::Here,
            camera: true,
            mic: true,
            clock: None,
            room: None,
            requests: Vec::new(),
            sharing: false,
            name: None,
            link: None,
            seen: std::collections::BTreeMap::new(),
            host: None,
            moved: None,
        }
    }
}

impl Presence {
    /// Here, with our camera and microphone as `local` says.
    fn here(local: &PeerStats) -> Self {
        Self {
            state: State::Here,
            camera: local.camera_on(),
            mic: local.mic_on(),
            clock: local.clock(),
            room: local.room(),
            requests: local.requests(),
            sharing: local.sharing(),
            // Cleaned on the way out as well as in: we send what we would
            // accept, whatever wrote our entry.
            name: local.name().as_deref().and_then(crate::name::clean),
            // **Off means neither publish nor show**: with the switch off
            // neither field is said, and a report already out is withdrawn
            // with the next document.
            link: local.connection_report().then(|| local.link()).flatten().map(Link::clean),
            seen: if local.connection_report() {
                quality::clean_seen(local.seen(), "")
            } else {
                std::collections::BTreeMap::new()
            },
            // Set only while we serve the room; cleaned on the way out too.
            host: local.offer().and_then(door::Offer::clean),
            // The winner we follow, repeated, so a choice outlives whoever
            // made it. Cleaned on the way out too.
            moved: local.followed_move().and_then(super::moves::CallOn::clean),
        }
    }
}

/// A peer's subroom as we hold it: a blank name, which comes off the wire
/// unchecked, is the main room. Kept as a subroom it would show as one
/// nobody can name or join, separate from main.
fn subroom(room: Option<String>) -> Option<String> {
    room.filter(|name| !name.trim().is_empty())
}

/// The serde default for the media flags.
fn on() -> bool {
    true
}

/// The one thing a participant currently says.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum State {
    /// On the call.
    #[default]
    Here,
    /// Going, deliberately. The whole point of having a channel: a peer that
    /// says this is telling us apart from a peer whose network died.
    Leaving,
}

/// Where `participant` publishes theirs.
fn control_path(participant: &str) -> String {
    format!("{participant}/{CONTROL}")
}

/// Whose control broadcast an announced path is.
fn controller_of(path: &str) -> Option<&str> {
    let id = path.strip_suffix(CONTROL)?.strip_suffix('/')?;
    (!id.is_empty()).then_some(id)
}

/// Whose broadcast an announced path is, or `None` if it is not a camera.
///
/// **Self-declared, and therefore not proof.** Between two peers who dialled
/// each other this is simply who they said they were, which is all it needs to
/// be; once a relay is in the middle, only a token scoped to each participant's
/// path would make it binding, and a room token is scoped to the room.
fn participant_of(path: &str) -> Option<&str> {
    let id = path.strip_suffix(CAMERA)?.strip_suffix('/')?;
    (!id.is_empty()).then_some(id)
}

/// The video track inside that broadcast.
const TRACK: &str = "video.avc3";

/// The encoder's pacing hint, not a promise. Matches the loopback.
pub(super) const FRAMERATE: u32 = 30;

/// Set once a hardware encoder has failed mid-call, so every later attempt in
/// this process uses software. One failure is enough to stop trusting a driver:
/// a flaky one would otherwise fail every redial the same way.
static HARDWARE_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The software encoder's backend name, as moq-video reports it.
const SOFTWARE_ENCODER: &str = "openh264";

/// Which encoder to ask moq-video for: `--encoder`, unless hardware has
/// already failed in this process.
fn encoder_kind() -> moq_video::encode::Kind {
    use crate::args::EncoderChoice;
    if HARDWARE_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
        return moq_video::encode::Kind::Software;
    }
    match crate::args::Args::get().encoder {
        EncoderChoice::Auto => moq_video::encode::Kind::Auto,
        EncoderChoice::Hardware => moq_video::encode::Kind::Hardware,
        EncoderChoice::Software => moq_video::encode::Kind::Software,
    }
}

/// Picks the encoder for `config`, then gives `broadcast` a catalog and a
/// video track carrying what that encoder will produce.
///
/// The probe encodes one real frame through the chosen backend, so a hardware
/// encoder that opens but cannot encode fails here, before anything is
/// advertised -- and then software gets a turn, recorded in `config.kind` for
/// the encoder built after. Asking for hardware only (`--encoder hardware`)
/// means no second turn. Shared by the camera and the shared screen.
///
/// One function rather than a `probe` returning the rendition, because the
/// rendition's type is `hang`'s, which is deliberately not a dependency here.
pub(super) async fn video_track(
    broadcast: &mut moq_net::broadcast::Producer,
    config: &mut moq_video::encode::Config,
    priority: u8,
) -> Result<(moq_mux::catalog::Producer, moq_video::encode::Producer), String> {
    let catalog = moq_mux::catalog::Producer::new(broadcast, Default::default())
        .map_err(|error| format!("could not create a catalog: {error:#}"))?;
    let producer = add_video(broadcast, &catalog, config, priority).await?;
    Ok((catalog, producer))
}

/// [`video_track`] into a catalog that already exists, which may already be
/// published: the camera's video joins its broadcast whenever the first frame
/// arrives, after the microphone and possibly mid-call.
async fn add_video(
    broadcast: &moq_net::broadcast::Producer,
    catalog: &moq_mux::catalog::Producer,
    config: &mut moq_video::encode::Config,
    priority: u8,
) -> Result<moq_video::encode::Producer, String> {
    config.kind = encoder_kind();
    let rendition = match config.probe().await {
        Ok(rendition) => rendition,
        Err(error) if config.kind == moq_video::encode::Kind::Auto => {
            tracing::warn!("[video] the hardware encoder failed its probe, using software: {error:#}");
            config.kind = moq_video::encode::Kind::Software;
            config
                .probe()
                .await
                .map_err(|error| format!("could not probe the encoder: {error:#}"))?
        }
        Err(error) => return Err(format!("could not probe the encoder: {error:#}")),
    };
    let track = broadcast
        .create_track(TRACK, catalog.track_info(priority))
        .map_err(|error| format!("could not create a track: {error:#}"))?;
    moq_video::encode::Producer::with_track(track, catalog.clone(), rendition)
        .map_err(|error| format!("could not publish video: {error:#}"))
}

/// [`FRAMERATE`] as the encoder takes it: an exact fraction.
fn framerate() -> moq_video::Rate {
    moq_video::Rate::new(FRAMERATE, 1).expect("a positive whole frame rate")
}

/// Video's delivery priority: below audio (80) and the control documents
/// ([`CONTROL_PRIORITY`]), as hang's own catalog ranks them.
pub(super) const VIDEO_PRIORITY: u8 = 60;

/// How many decoded frames between rate reports in the log. Matches the paint
/// path's cadence so the two logs are comparable.
const REPORT_EVERY: u32 = 120;


/// How long the transport may go unheard before the session is declared dead.
///
/// The stack's own default is 30 seconds, which suits a publisher feeding a
/// relay and does not suit a call: **it is the entire delay before anything on
/// screen changes**, because iroh exposes no keep-alive knob and nothing else
/// ends a session whose peer has gone quiet. Ten seconds of nothing is already
/// a broken call to the person watching it, and redialling an endpoint iroh
/// already knows is cheap.
///
/// Set on the endpoint rather than on a connection, so both roles share it.
///
/// The opposite error is real -- declaring a congested link dead when it would
/// have recovered on its own -- so if that shows up this number goes back up
/// rather than the reconnection coming out.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long to keep redialling a dropped peer before giving up on the call.
const RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// Which secret key the endpoint comes up with, and therefore who we are.
///
/// Three answers, in order. `--secret` is forwarded into `IROH_SECRET` by
/// `args`; moq-tokio reads its own `MOQ_IROH_SECRET`, so the two are bridged
/// here rather than teaching `args` a second variable. `--ephemeral` is `None`,
/// a throwaway key, which is what every launch used to be. Otherwise a **path**,
/// which moq-tokio generates into on first use and reads back afterwards
/// (hex parses as a key, anything else is treated as a file) -- so our id
/// outlives the process and a key someone wrote down still dials.
///
/// The one hazard is two instances on one machine picking up the same file and
/// being the same endpoint. That is what `--ephemeral` is for, and what the
/// self-dial check below catches when nobody remembered.
fn identity() -> Option<String> {
    if let Ok(secret) = std::env::var("IROH_SECRET") {
        return Some(secret);
    }
    if crate::args::Args::get().ephemeral {
        return None;
    }
    crate::settings::identity_path().map(|path| path.to_string_lossy().into_owned())
}

/// Retry pacing for the dialling side.
///
/// Only the dialling side redials -- the listener has nothing to dial -- but
/// both sides derive their linger from [`RETRY_BUDGET`] (it plus a second, the
/// rule moq-native's `Backoff::linger` used to apply), because what it decides
/// is how long each side holds the *other's* broadcast open waiting for it to
/// come back. The two have to agree or one side tears down a call the other is
/// still repairing.
///
/// The escalation itself is left alone: 1 s, doubling to 5 s, is a sensible
/// climb, and the jitter matters even between two peers.
fn backoff() -> moq_tokio::Backoff {
    let mut backoff = moq_tokio::Backoff::default();
    backoff.timeout = RETRY_BUDGET;
    backoff
}

/// Runs a call on its own thread for the lifetime of the process.
///
/// The thread outlives any one call: an ending is reported and then the loop
/// goes back to idle, so a call that drops for good can be replaced without
/// restarting the app. Only losing the interface itself ends this.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn(
    decisions: futures_channel::mpsc::UnboundedReceiver<Peering>,
    feed: FrameFeed,
    status: UnboundedSender<RemoteStatus>,
    participants: UnboundedSender<ParticipantEvent>,
    controls: Arc<Controls>,
    metrics: Arc<Metrics>,
    local: Arc<PeerStats>,
    share: ShareWish,
    doors: Arc<door::Doors>,
    hosts: UnboundedSender<HostNews>,
) {
    let spawned = std::thread::Builder::new()
        .name("call".into())
        .spawn(move || {
            let Some(runtime) = runtime() else {
                let _ = status.unbounded_send(RemoteStatus::Failed {
                    error: "no tokio runtime".to_string(),
                });
                return;
            };
            runtime.block_on(async move {
                // Every status passes through the sleep guard on its way out,
                // the final one included, so the guard sees them in the order
                // the interface does.
                let (relay, statuses) = futures_channel::mpsc::unbounded();
                tokio::spawn(crate::awake::follow(statuses, status));
                let status = relay;
                match run(decisions, feed, &status, participants, controls, metrics, local, share, doors, hosts).await {
                    Ok(()) => {
                        tracing::info!("call ended");
                        let _ = status.unbounded_send(RemoteStatus::Ended);
                    }
                    Err(error) => {
                        tracing::error!("call failed: {error}");
                        let _ = status.unbounded_send(RemoteStatus::Failed { error });
                    }
                }
            });
        });
    if let Err(error) = spawned {
        tracing::warn!("could not spawn the call thread: {error}");
    }
}

/// Binds, publishes, connects, and pumps every participant's video into a slot
/// of its own.
///
/// Errors are formatted rather than typed: they come from several crates with
/// several error types, none of which the UI can do anything with beyond
/// showing the text.
#[allow(clippy::too_many_arguments)]
async fn run(
    mut decisions: futures_channel::mpsc::UnboundedReceiver<Peering>,
    feed: FrameFeed,
    status: &UnboundedSender<RemoteStatus>,
    participants: UnboundedSender<ParticipantEvent>,
    controls: Arc<Controls>,
    metrics: Arc<Metrics>,
    local: Arc<PeerStats>,
    share: ShareWish,
    doors: Arc<door::Doors>,
    hosts: UnboundedSender<HostNews>,
) -> Result<(), String> {
    let report = |next: RemoteStatus| {
        let _ = status.unbounded_send(next);
    };

    report(RemoteStatus::Binding);

    let mut client_config = moq_tokio::client::Config::default();
    client_config.connect.backoff = backoff();
    // Read by `bind` below and applied to the endpoint's transport config, which
    // is what makes it symmetric: the listener shares this endpoint and so
    // inherits the same timeout without a config of its own.
    client_config.quic.idle_timeout = IDLE_TIMEOUT;
    let mut iroh = moq_tokio::iroh::Config::default();
    iroh.enabled = Some(true);
    iroh.secret = identity();
    let endpoint = iroh
        .bind(&client_config.quic)
        .await
        .map_err(|error| format!("could not bind an endpoint: {error:#}"))?
        .ok_or_else(|| "iroh disabled -- unreachable, we set enabled".to_string())?;

    let me = endpoint.id();
    // Logged as well as shown, because this is the string the other side needs
    // and a window is a poor place to copy from.
    tracing::info!("our endpoint id: {me}");

    // Bound once, reused for every attempt below: cancelling a choice must not
    // cost the endpoint, and our key has to stay the same string the whole time
    // or anyone we already gave it to is stranded.
    // Carries a destination chosen while another attempt was still running, so
    // clicking Connect during a dial redials rather than silently cancelling.
    let mut queued: Option<Peering> = None;
    // Parked between attempts: the blocking camera wait needs to own it, and a
    // cancelled attempt has to give it back for the next one.
    let mut parked = Some(feed);
    // How the last call ended, shown in place of the idle line until the user
    // does something else. Reporting `Idle` unconditionally below would replace
    // it in the same instant, so the reason a call dropped would never be
    // readable -- which is the whole point of having given up.
    let mut ending: Option<RemoteStatus> = None;
    // How long a broadcast outlives losing its session, on both sides. See
    // `backoff`: it is the retry budget plus a second.
    let linger = RETRY_BUDGET + std::time::Duration::from_secs(1);

    loop {
        // The decision may already be made (`--listen` / `--dial`) or may be the
        // user's to make, in the app. Either way it is awaited *here*, after binding and
        // before the camera: our key must be visible while we wait, and waiting on
        // the camera first would hide it behind a device that may never open.
        report(
            ending
                .take()
                .unwrap_or_else(|| RemoteStatus::Idle { me: me.to_string() }),
        );
        let peering = match queued.take() {
            Some(peering) => peering,
            None => match decisions.next().await {
                // `Idle` on this channel means "take that back" -- see the cancel
                // branch below. Arriving here it just means we are already idle.
                Some(Peering::Idle) => continue,
                Some(peering) => peering,
                None => return Err("the connect control went away".to_string()),
            },
        };

        // Our own key dials nothing: iroh would try to reach an address that is
        // this process. Worth a sentence rather than a hang, because pasting
        // your own key is a thing people do -- and because two instances sharing
        // the saved identity file land here, where the message can say so.
        if matches!(&peering, Peering::Dial(peer) if *peer == me) {
            tracing::warn!("refusing to dial ourselves: {me}");
            ending = Some(RemoteStatus::BadKey {
                reason: "that is your own key. Two windows on one machine need --ephemeral"
                    .to_string(),
            });
            continue;
        }

        report(RemoteStatus::Preparing);
        // **The call does not wait for the camera.** Its video is added by the
        // encode thread when the first frame arrives, which for someone whose
        // camera is off may be mid-call or never; until then the broadcast
        // carries the microphone alone.
        let Some(feed) = parked.take() else {
            return Err("the camera feed is gone".to_string());
        };
        // The send budget for everything this attempt publishes, made before
        // the microphone starts, since it reserves as it does. The estimate it
        // divides is fed below, once the attempt knows which side it is; until
        // then, and on a link that never reports one, every slice is "hold
        // your rate", so each stream sends what it was configured for.
        let estimate = moq_net::bandwidth::Producer::new();
        let link = adapt::Link::new(&estimate);
    // Set when a session comes back, read by the encode loop. A subscriber that
    // rejoins mid-stream cannot decode until a keyframe arrives, and the encoder
    // already inserts one every two seconds -- so this only buys back that wait,
    // but the wait is a frozen tile and worth buying.
    let keyframe = Arc::new(Keyframes::default());
        let outbound = moq_tokio::origin::spawn();
        let mut broadcast = outbound
            .create_broadcast(camera_path(&me.to_string()))
            .map_err(|error| format!("could not create a broadcast: {error:#}"))?;
        // One catalog for the microphone now and the camera whenever it comes.
        let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default())
            .map_err(|error| format!("could not create a catalog: {error:#}"))?;

        // Announced beside the camera, and said before anything is shown: the
        // peer knows we are here whether or not a frame ever arrives.
        let control = outbound
            .create_broadcast(control_path(&me.to_string()))
            .map_err(|error| format!("could not create a control broadcast: {error:#}"))?;
        let presence = control
            .create_track(PRESENCE, None)
            .map(|track| {
                moq_json::snapshot::Producer::new(track, presence_config())
            })
            .map_err(|error| format!("could not create a presence track: {error:#}"))?;
        // Shared with the announcer below and with the leave sequence. A plain
        // mutex: every use is a quick update, never held across an await.
        let presence = Arc::new(std::sync::Mutex::new(presence));
        // The first word already carries the camera and microphone as they
        // are, so joining muted is announced from the start.
        say(&presence, Presence::here(&local));
        // Advertised only now that the presence track exists: an announce is
        // acted on at once, so a peer must find the document waiting.
        control
            .announce(moq_net::origin::Route::default())
            .map_err(|error| format!("could not announce our presence: {error:#}"))?;
        let announcing = Abort(tokio::spawn(announce(presence.clone(), local.clone())));

        let no_audio = crate::args::Args::get().no_audio;
        let engine = if no_audio {
            tracing::info!("[audio] --no-audio: neither the microphone nor the speaker is opened");
            None
        } else {
            audio::open_playback(&controls).await
        };
        let mut aec_report = None;
        let published = if no_audio {
            Err("--no-audio".to_string())
        } else {
            audio::publish(broadcast.clone(), catalog.clone(), engine.as_ref(), link.allocator.clone())
        };
        match published {
            Ok((publication, driver, canceller)) => {
                controls.attach(publication);
                audio::drive(driver);
                aec_report = canceller
                    .map(|canceller| Abort(tokio::spawn(audio::report_aec(canceller, Some(metrics.clone())))));
            }
            Err(_) if no_audio => {}
            Err(error) => tracing::warn!("[audio] the microphone is not being published: {error}"),
        }
        // The camera broadcast likewise, once its catalog and the microphone's
        // track exist. Its video track is not here yet; see the encode thread.
        broadcast
            .announce(moq_net::origin::Route::default())
            .map_err(|error| format!("could not announce our camera: {error:#}"))?;

        // **Lingers rather than tearing down.** A session dying abruptly detaches
        // the peer's broadcast from this origin, and with the default zero
        // linger that closes it on the spot: our decoder's read ends, the task
        // returns, the tile goes black -- and nothing ever re-subscribes, so the
        // transport quietly redialling underneath changes nothing. A linger
        // keeps the path announced instead, and a session re-attaching within
        // the window splices into the same broadcast, so the gap is never
        // observed above this line.
        //
        // **A host has one origin, not two.** What its guests publish is
        // offered back out to every other guest, which is what lets them see
        // each other through it; and its own broadcasts sit beside theirs, as
        // a relay room's participants do. `attend` skips our own id, so
        // reading our own announcements back is harmless.
        //
        // **A join shares one too, but only when hosting is on as it
        // starts**, so that the room's members reaching us directly and the
        // relay room are one room: what they publish is offered on to the
        // relay, and the relay's room is offered to them. The library keeps
        // that from looping (a route is never offered back to the session it
        // came in on), which this module's bridge tests show. With hosting off a
        // join is wired exactly as it always was, two origins, until a change
        // to everyone's has been tried by hand.
        let host_rooms = local.host_rooms();
        // Where the last call was moved says nothing about this one.
        local.forget_moves();
        let shared = shares_origin(&peering, host_rooms);
        // Only a host passes on other people's media, so only a host counts it.
        let forwarded = shared.then(|| Forwarded::new(me.to_string()));
        let inbound = inbound_for(&outbound, shared);
        // Where else the room can meet, as the interface put it with the
        // decision. A join with somewhere else to go, or one we host, holds
        // several sessions (see `sessions`); any other is the one session it
        // always was.
        let known = match &peering {
            Peering::Join(url) => doors.hosts(&crate::recent::identity(url)),
            _ => Vec::new(),
        };
        let several = dials_several(&peering, host_rooms, !known.is_empty());
        // Where what we hear about hosts goes: only a join has a remembered
        // room to keep them with.
        let news = match &peering {
            Peering::Join(url) => Some(RoomNews {
                room: crate::recent::identity(url),
                to: hosts.clone(),
            }),
            _ => None,
        };
        // Serving the room's members beside the join, while hosting stays on.
        let mut alongside: Option<Abort> = None;

        // Read before the match below takes `peering` apart. A hosted room is
        // a room: nobody leaving ends it, and the status line stays on it.
        let in_room = matches!(peering, Peering::Join(_) | Peering::Host(_));
        // Only an iroh call has a path worth naming: an `https://` room goes
        // over plain QUIC, and the endpoint's counters would only ever say
        // "nothing moved".
        let over_iroh = match &peering {
            Peering::Join(url) => url.scheme() == "iroh" || several,
            Peering::Dial(_) | Peering::Listen | Peering::Host(_) => true,
            Peering::Loopback | Peering::Idle => false,
        };
        // What reading each person's route needs beside the chain itself.
        let routing = Routing {
            guests: Arc::new(arrival::Guests::default()),
            ours: outbound.hop().id(),
            far: match &peering {
                Peering::Join(url) if url.scheme() == "iroh" => url.host_str().map(str::to_string),
                _ => None,
            },
        };
        // Where our media goes, for our own tile. A join over several sessions
        // says it as it moves (see `sessions`).
        local.set_sending(match &peering {
            Peering::Join(_) if !several => Some(arrival::Sending::Relay),
            Peering::Host(_) => Some(arrival::Sending::Guests),
            _ => None,
        });
        let peer_transport: Peer = match peering {
            Peering::Dial(peer) => {
                report(RemoteStatus::Dialing {
                    peer: peer.to_string(),
                });
                tracing::info!("dialing {peer}");
                // iroh dials a public key, not a hostname: the URL's host *is* the
                // endpoint id.
                let url: url::Url = format!("iroh://{peer}")
                    .parse()
                    .map_err(|error| format!("could not build a dial url: {error}"))?;
                connect(Connect {
                    config: &client_config,
                    endpoint: &endpoint,
                    outbound: &outbound,
                    inbound: inbound.clone(),
                    url,
                    label: peer.to_string(),
                    room: None,
                    status: status.clone(),
                    keyframe: keyframe.clone(),
                })?
            }
            Peering::Join(ref url) if !several => {
                let room = room_label(url);
                report(RemoteStatus::Joining { room: room.clone() });
                tracing::info!("joining {room} at {}", redacted(url));
                connect(Connect {
                    config: &client_config,
                    endpoint: &endpoint,
                    outbound: &outbound,
                    inbound: inbound.clone(),
                    url: url.clone(),
                    label: redacted(url),
                    room: Some(room),
                    status: status.clone(),
                    keyframe: keyframe.clone(),
                })?
            }
            Peering::Join(ref url) => {
                let room = room_label(url);
                report(RemoteStatus::Joining { room: room.clone() });
                tracing::info!("joining {room} at {}", redacted(url));
                // Serving first, so its guests' links are there to fall back
                // on, and so a relay that is down cannot hold it up.
                let guests = moq_net::bandwidth::Producer::new();
                let guests_link = guests.consume();
                let heard = Arc::new(quiet::Heard::default());
                if shared {
                    alongside = host_alongside(Alongside {
                        config: &client_config,
                        endpoint: &endpoint,
                        shared: outbound.clone(),
                        url,
                        doors: &doors,
                        estimates: adapt::Estimates::new(guests),
                        local: local.clone(),
                        forwarded: forwarded.clone(),
                        guests: routing.guests.clone(),
                        heard: heard.clone(),
                    })
                    .await;
                }
                // A healthy session to an `http(s)` relay then hears
                // something at least every second, which is what lets a join
                // leave one that went quiet after two (see `quiet`). Only
                // here: a join with one session has nothing to compare with,
                // and is left as it was. iroh sessions ignore it.
                let mut several_config = client_config.clone();
                several_config.quic.keep_alive = quiet::KEEP_ALIVE;
                let client = several_config
                    .init()
                    .map_err(|error| format!("could not build a client: {error:#}"))?
                    .with_iroh(endpoint.clone());
                // A scripted run's choice, said as the join starts.
                if let Some(to) = crate::args::Args::get().move_call.clone() {
                    local.move_call(to, &me.to_string());
                }
                let client = match &forwarded {
                    Some(forwarded) => client.with_stats(forwarded.outgoing()),
                    None => client,
                };
                sessions::join(sessions::Join {
                    client,
                    me: me.to_string(),
                    targets: sessions::targets(url, &known, &me.to_string(), alongside.is_some()),
                    outbound: outbound.clone(),
                    inbound: inbound.clone(),
                    hosting: shared,
                    estimate: estimate.clone(),
                    guests: alongside.is_some().then_some(guests_link),
                    heard: alongside.is_some().then_some(heard),
                    metrics: metrics.clone(),
                    local: local.clone(),
                    status: status.clone(),
                    keyframe: keyframe.clone(),
                    room,
                    bounce: crate::args::Args::get().bounce,
                })
            }
            Peering::Listen => {
                report(RemoteStatus::Listening { me: me.to_string() });
                // The call's own estimate, fed from the live sessions. The
                // dialling side gets a persistent one from `Reconnect`; here
                // the call's is the persistent one, because it has to exist
                // before anyone has called.
                //
                // **Accepting is its own task, not a branch below.** A listener
                // has to watch two things at once -- new sessions and the ones
                // it has -- and `accept` parks half-finished handshakes on the
                // server itself, so dropping it mid-poll to take the other
                // branch could lose exactly the reconnection being waited for.
                let (accepting, mut events) = serve(Serve {
                    config: &client_config,
                    endpoint: &endpoint,
                    publisher: outbound.consume(),
                    subscriber: inbound.clone(),
                    door: None,
                    estimates: Some(adapt::Estimates::new(estimate.clone())),
                    metrics: metrics.clone(),
                    forwarded: None,
                    guests: None,
                    heard: None,
                })
                .await?;
                let status = status.clone();
                let keyframe = keyframe.clone();
                Peer {
                    // Nothing here dials, so "reconnecting" is only ever a count
                    // of live sessions: the peer redials us. What this adds is
                    // the ending a listener never had -- before it, a peer that
                    // dropped or quit outright left the call sitting at
                    // "connected" on a dead session until the app was restarted.
                    closed: Box::pin(async move {
                        // Aborted with this future, which is what stops the
                        // server when the attempt is over.
                        let _accepting = accepting;
                        let mut live = 0usize;
                        // Only the empty state has a deadline. A listener that
                        // has never been called waits forever, as it always did.
                        let mut empty_since: Option<tokio::time::Instant> = None;

                        loop {
                            let expiry = empty_since.map(|at| at + linger);
                            let expired = async move {
                                match expiry {
                                    Some(at) => tokio::time::sleep_until(at).await,
                                    None => std::future::pending::<()>().await,
                                }
                            };
                            tokio::select! {
                                () = expired => return "the peer did not come back".to_string(),
                                event = events.next() => match event {
                                    Some(true) => {
                                        // Reported on the way back only. The
                                        // first connection is the subscriber's
                                        // to announce, when the peer's video
                                        // actually arrives -- a session exists
                                        // some time before that is true.
                                        if empty_since.take().is_some() {
                                            tracing::info!("the peer is back");
                                            keyframe.ask();
                                            let _ = status.unbounded_send(
                                                RemoteStatus::Connected { peer: None },
                                            );
                                        }
                                        live += 1;
                                    }
                                    Some(false) => {
                                        live = live.saturating_sub(1);
                                        if live == 0 {
                                            tracing::warn!(
                                                "the peer's session ended; waiting for it to come back"
                                            );
                                            empty_since = Some(tokio::time::Instant::now());
                                            let _ = status
                                                .unbounded_send(RemoteStatus::Reconnecting);
                                        }
                                    }
                                    None => return "the listener stopped".to_string(),
                                },
                            }
                        }
                    }),
                    bandwidth: None,
                    stats: None,
                }
            }
            Peering::Host(room) => {
                let hosting = door::Hosting::open(room)?;
                let room = format!("room {}, hosted here", hosting.room());
                let (accepting, mut events) = serve(Serve {
                    config: &client_config,
                    endpoint: &endpoint,
                    publisher: outbound.consume(),
                    subscriber: inbound.clone(),
                    door: Some(hosting.clone()),
                    estimates: Some(adapt::Estimates::new(estimate.clone())),
                    metrics: metrics.clone(),
                    forwarded: forwarded.clone(),
                    guests: Some(routing.guests.clone()),
                    // Hosting alone, there is no relay to judge.
                    heard: None,
                })
                .await?;
                if let Some(forwarded) = &forwarded {
                    forwarded.set_serving(true);
                }
                // **Stdout, not the log.** The invite carries the door, and
                // the log file is what people attach to bug reports. Quoted,
                // because an unquoted `?` is a glob to zsh.
                println!(
                    "Hosting {}. Guests join with:\n  vdr --relay '{}'",
                    hosting.room(),
                    hosting.invite(&me.to_string())
                );
                tracing::info!("hosting {}; the invite went to stdout, not to this log", hosting.room());
                report(RemoteStatus::InRoom { room, via: None });
                Peer {
                    // Guests coming and going is the room's ordinary life,
                    // not the call's: it ends only when we stop serving.
                    closed: Box::pin(async move {
                        let _accepting = accepting;
                        let mut live = 0usize;
                        loop {
                            match events.next().await {
                                Some(true) => {
                                    live += 1;
                                    tracing::info!("a guest came in; {live} connected");
                                }
                                Some(false) => {
                                    live = live.saturating_sub(1);
                                    tracing::info!("a guest's session ended; {live} connected");
                                }
                                None => return "stopped hosting".to_string(),
                            }
                        }
                    }),
                    bandwidth: None,
                    stats: None,
                }
            }
            Peering::Loopback | Peering::Idle => return Err("no peer to call".to_string()),
        };

        // Below here the two sides are the same, which is the point of `Peer`.
        let Peer {
            closed: transport,
            bandwidth,
            stats,
        } = peer_transport;
        let mirroring = bandwidth.map(|source| Abort(tokio::spawn(adapt::mirror(source, estimate.clone()))));
        let rate = adapt::spawn(adapt::Follow {
            stream: adapt::Stream::Camera,
            estimate: link.estimate.clone(),
            lane: link.camera.clone(),
            beside: None,
            metrics: metrics.clone(),
            shown: Box::new(|| true),
        });
        let reporting = stats.map(|stats| {
            let metrics = metrics.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_millis(200));
                loop {
                    ticker.tick().await;
                    if let Some(stats) = stats.stats() {
                        metrics.set_connection(&stats);
                    }
                }
            })
        });
        let pathing = over_iroh.then(|| Abort(tokio::spawn(watch_path(endpoint.clone(), metrics.clone()))));
        let metering = Abort(tokio::spawn(meter_sent(metrics.clone(), forwarded.clone())));

        // The person's wish to share a screen, served for this attempt. Its
        // own broadcast on the same outbound origin, so a share started now
        // reaches whoever this attempt connects to.
        let (quit_sharing, share_quit) = tokio::sync::oneshot::channel();
        let mut sharing = Abort(tokio::spawn(share::serve(
            share::Call {
                outbound: outbound.clone(),
                me: me.to_string(),
                local: local.clone(),
                keyframe: keyframe.clone(),
                preview: share.preview(),
                quality: share.quality_choice(),
                metrics: metrics.clone(),
                link: link.clone(),
            },
            share.clone(),
            share_quit,
        )));

        // One loop for everything that arrives, because it all starts at the
        // same place: a participant announcing themselves. It replaces two
        // waits on one well-known path, which is what limited a call to exactly
        // two people.
        //
        // `hungup` is how it says the last person left -- deliberately, which is
        // the difference a control plane buys.
        let (hangup, mut hungup) = futures_channel::mpsc::unbounded::<String>();
        let decode = tokio::spawn(attend(
            inbound.consume(),
            me.to_string(),
            Reports {
                status: status.clone(),
                participants: participants.clone(),
                hangup,
                metrics: metrics.clone(),
                in_room,
                rooms: audio::Rooms {
                    local: local.clone(),
                    controls: controls.clone(),
                },
                news,
                routing,
            },
            engine,
        ));

        // **On a blocking thread, not in the `select!` below.** Converting a
        // frame and encoding it are synchronous and take a good fraction of the
        // interval between frames, so a loop doing them yields only when the
        // camera has nothing ready. Left as a branch here it starves everything
        // it shares a task with -- which is the transport, so the call does not
        // connect at all once the machine has no slack, and the cancel channel,
        // so the button waits on a frame.
        //
        // It owns the feed, the encoder and the producer while it runs and
        // hands the feed and the producer back on the way out, because the
        // next attempt needs the feed and this one still has to flush.
        //
        // **It is also where the video starts.** It waits for the camera's
        // first frame, sizes the encoder from it, and only then adds the video
        // track to the broadcast. A camera that is off holds this thread, not
        // the call; a camera turned on mid-call starts the video then.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut encoding = tokio::task::spawn_blocking({
            let metrics = metrics.clone();
            let stop = stop.clone();
            let link = link.clone();
            let keyframe = keyframe.clone();
            let local = local.clone();
            let broadcast = broadcast.clone();
            let catalog = catalog.clone();
            let runtime = tokio::runtime::Handle::current();
            // **The encoder is built here, on the thread that uses it.** It
            // is not `Send` (a hardware session belongs to the thread that
            // opened it), so it can neither be handed in nor handed back. For
            // the same reason it is flushed here, before the feed and the
            // producer go back: the leave sequence below needs the media
            // finished before it says goodbye.
            move || {
                let mut feed = feed;
                // Taken and dropped, not encoded, as before this waited
                // outside: the tap keeps the last frame offered after the
                // camera is switched off, and encoding that would send a still
                // of someone who has turned their camera off.
                let first = loop {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        return (feed, None, Ok(()));
                    }
                    match feed.wait_timeout(std::time::Duration::from_millis(100)) {
                        Ok(Some(frame)) => break frame,
                        Ok(None) => continue,
                        // Every tap gone: the camera side is shutting down,
                        // which ends the encode loop the same way.
                        Err(_) => return (feed, None, Ok(())),
                    }
                };
                tracing::info!("[video] first camera frame {}x{}; starting video", first.width, first.height);
                // Encoded smaller than captured, when the camera insists on
                // more than the link can carry. The bitrate follows from the
                // size, so this is where it gets decided.
                let (width, height, scale) = encode_size(first.width, first.height);
                metrics.set_encode_size(width, height);
                let mut config = moq_video::encode::Config::new(width, height, framerate());
                let opening_bitrate = crate::camera::bitrate_for(width, height, FRAMERATE);
                config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(opening_bitrate));
                // A blocking thread is not inside the runtime, so it may block
                // on it. The probe is one encode, as it was on the call task.
                let mut producer = match runtime.block_on(add_video(&broadcast, &catalog, &mut config, VIDEO_PRIORITY)) {
                    Ok(producer) => producer,
                    Err(error) => return (feed, None, Err(error)),
                };
                let mut encoder = match moq_video::encode::Encoder::new(&config) {
                    Ok(encoder) => encoder,
                    Err(error) => {
                        let outcome = Err(format!("could not build an encoder: {error:#}"));
                        return (feed, Some(producer), outcome);
                    }
                };
                // Held until the encode loop ends, which is the stream's life:
                // dropping it hands the camera's room to the others. Reserved
                // at the opening rate, the most this encoder ever sends.
                let reservation = link
                    .allocator
                    .reserve(&producer.demand(), moq_net::bandwidth::Rate::from_bps(opening_bitrate));
                link.camera.follow(reservation.consumer());
                link.camera.set_ceiling(opening_bitrate);
                // The target starts at the ceiling, and the rate loop only
                // writes when it moves it, so without this a sender the link
                // never holds back would show no rate all call.
                metrics.set_send_kbps((opening_bitrate / 1000) as u32);
                metrics.set_camera_cap_kbps((opening_bitrate / 1000) as u32);
                tracing::info!(
                    "encoding {width}x{height} at the {} rung with {}",
                    crate::args::Args::get().quality,
                    encoder.name()
                );
                metrics.set_encoder(encoder.name());
                let outcome = encode(
                    &mut feed,
                    &mut encoder,
                    &mut producer,
                    Steering {
                        scale,
                        lane: &link.camera,
                        keyframe: &keyframe.camera,
                        stop: &stop,
                        local: &local,
                        metrics: &metrics,
                    },
                );
                // A hardware encoder failing mid-call demotes the rest of this
                // run to software; the call's own retry then reopens on it.
                if demotes(&outcome, encoder.name()) {
                    tracing::warn!("[video] {} failed mid-call; software from now on", encoder.name());
                    HARDWARE_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                let outcome = outcome.map_err(Stopped::into_message);
                if let Ok(flushed) = encoder.flush() {
                    if !flushed.is_empty() && producer.publish(&flushed).is_ok() {
                        metrics.camera_sent(payload_bytes(&flushed));
                    }
                }
                drop(reservation);
                (feed, Some(producer), outcome)
            }
        });

        // Set when the encode thread is what ended the attempt, so the join
        // below knows not to await a handle that has already resolved.
        let mut encoded = None;

        // **Raced by reference, so the connection outlives the race.** Moving
        // it in dropped it the moment anything else won -- a click on Leave,
        // say -- and everything below then said goodbye down a line already
        // hung up. A relay reads a publisher that vanishes like that as a crash
        // and keeps its broadcasts up for its linger (5 s by default), so a
        // deliberate leave reached the room five seconds late.
        let mut transport = transport;
        let outcome = tokio::select! {
            error = &mut transport => Some(Err(error)),
            joined = &mut encoding => match joined {
                Ok(parts) => {
                    let outcome = parts.2.clone();
                    encoded = Some(parts);
                    Some(outcome)
                }
                Err(error) => Some(Err(format!("the encode thread failed: {error}"))),
            },
            left = hungup.next() => {
                match left {
                    Some(who) => tracing::info!("{who} hung up"),
                    None => tracing::warn!("the attendance loop stopped"),
                }
                Some(Ok(()))
            }
            cancelled = decisions.next() => match cancelled {
                Some(Peering::Idle) => None,
                Some(next) => {
                    queued = Some(next);
                    None
                }
                None => Some(Err("the connect control went away".to_string())),
            },
        };

        // Whatever won the race, the encode thread has to be told to stop and
        // then waited for: a blocking task cannot be cancelled out from under
        // itself, and the feed and producer are needed here. The thread has
        // flushed the encoder by the time it returns.
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let (feed, producer) = match encoded {
            Some((feed, producer, _)) => (feed, producer),
            None => match encoding.await {
                Ok((feed, producer, _)) => (feed, producer),
                Err(error) => return Err(format!("the encode thread failed: {error}")),
            },
        };

        // **The share stops first**, by the same rule as the camera below: its
        // media finished and its broadcast closed before `Leaving` is said.
        // The wish goes too, so the next call does not start sharing.
        share.set(None);
        let _ = quit_sharing.send(());
        if tokio::time::timeout(share::STOP_WITHIN + std::time::Duration::from_millis(100), &mut sharing.0)
            .await
            .is_err()
        {
            tracing::warn!("[share] the share did not stop in time; leaving anyway");
        }
        drop(sharing);

        // **Finish the media, then say goodbye, then unannounce** -- in that
        // order, because each step is only true once the one before it is.
        // Writing a flushed frame into a broadcast that has already been
        // finished re-announces it, which the peer reads as somebody leaving and
        // immediately rejoining; observed as a `left`/`joined` pair 53
        // microseconds apart before this was ordered properly.
        if let Some(mut producer) = producer {
            producer.finish().ok();
        }

        // **Said, then shown.** The document is advisory -- it is what lets the
        // peer say "they hung up" rather than "they vanished" -- but `finish`
        // is what ends it: a deliberate unannounce closes the broadcast on the
        // peer immediately, where an abrupt loss lingers for the reconnect
        // window instead. If the last word does not make it, the unannounce
        // still does.
        // The announcer goes first, so nothing it says can follow `Leaving`.
        drop(announcing);
        say(
            &presence,
            Presence {
                state: State::Leaving,
                ..Presence::here(&local)
            },
        );
        // A beat between saying it and ending the broadcast it is said on. A
        // relay tears a cleanly finished broadcast down at once, and a
        // document written in the same instant goes down with it -- observed:
        // with the line kept open the leave arrived instantly but `Leaving`
        // never did. (Before that, an abrupt close made the relay keep the
        // broadcast up for its linger, which delivered the word by accident.)
        // Finishing the presence track first, with no pause, was tried and
        // lost the word just the same: nothing reports that the relay has
        // forwarded it, so a short bound is what there is.
        tokio::time::sleep(GOODBYE_SAY).await;
        control.close();
        broadcast.close();
        // Long enough for the finishes above to leave, then the line goes. No
        // handle says when they have, so this is a bound rather than a wait
        // for something; short, because a leave should feel instant.
        tokio::time::sleep(GOODBYE_FLUSH).await;
        drop(transport);
        // Ends every guest's session with the call, and with it what we say
        // about hosting; the goodbye above still carried it, so nobody reads
        // our leaving as having stopped.
        drop(alongside);
        local.set_offer(None);
        decode.abort();
        rate.abort();
        drop(mirroring);
        if let Some(reporting) = reporting {
            reporting.abort();
        }
        // Before the clear, which resets the path, so nothing writes after it.
        drop(pathing);
        drop(metering);
        metrics.clear();
        drop(aec_report);
        controls.detach();
        // Requests went to a room that is gone, and "muted by" was about it.
        // The mic button stays red if the mic is still off, which is honest
        // on its own.
        local.clear_asks();
        // Our report was about a link that is gone.
        local.clear_report();
        // Ends every share's turn, so the bar's "Screen recording is off"
        // does not outlive the call, and a straggling share thread can write
        // nothing into the next one.
        local.begin_share(0);
        // The subroom was in a room that is gone now: the next call starts in
        // its main room, not in a name nobody else is using.
        local.set_room(None);
        // Out of the room, our media goes nowhere.
        local.set_sending(None);
        // Aborting the loop above stops the per-participant tasks with it, but
        // the tiles are the interface's and have to be taken down by name.
        let _ = participants.unbounded_send(ParticipantEvent::Cleared);

        parked = Some(feed);

        // **An ending is not the end of the thread.** The endpoint is still
        // bound and our key is still the one we handed out, so a call that
        // dropped for good can be replaced from the same window -- which is the
        // difference between a reconnection policy and a restart.
        match outcome {
            Some(Ok(())) => {
                tracing::info!("call ended");
                ending = Some(RemoteStatus::Ended);
            }
            Some(Err(error)) => {
                tracing::error!("call failed: {error}");
                ending = Some(RemoteStatus::Failed {
                    error: explain(error),
                });
            }
            None => tracing::info!("call cancelled; back to idle"),
        }
    }
}

/// Pulls camera frames off the tap, encodes them, and publishes them.
///
/// The loopback's loop verbatim -- which is the point of having had one.
/// What the call thread steers the encode loop with, from outside it.
struct Steering<'a> {
    /// Integer downscale from capture to encode size.
    scale: usize,
    /// The camera's lane, where the rate loop leaves a new bitrate.
    lane: &'a adapt::Lane,
    /// Set when a peer rejoins and needs a keyframe.
    keyframe: &'a std::sync::atomic::AtomicBool,
    /// Set when the attempt is over.
    stop: &'a std::sync::atomic::AtomicBool,
    /// Ourselves: the encode loop records our camera-to-wall-clock pairing.
    local: &'a PeerStats,
    /// Where the bytes it publishes are counted.
    metrics: &'a Metrics,
}

/// The bytes in a batch of encoded frames, as handed to the transport.
pub(super) fn payload_bytes(encoded: &[moq_video::encode::Encoded]) -> usize {
    encoded.iter().map(|frame| frame.payload.len()).sum()
}

/// How the camera's encode loop ended badly.
///
/// **Only the encoder's own failure says anything about the encoder.** A
/// publish that fails is the track's or the session's, and treating it as
/// the hardware encoder failing would put the rest of the process on
/// software for a fault the encoder never had.
#[derive(Debug)]
enum Stopped {
    /// The encoder could not encode a frame.
    Encoder(String),
    /// What it encoded could not be published.
    Publish(String),
}

impl Stopped {
    fn into_message(self) -> String {
        match self {
            Self::Encoder(message) | Self::Publish(message) => message,
        }
    }
}

/// Whether an encode loop that ended with `outcome`, on the encoder named
/// `encoder`, is a hardware encoder failing.
fn demotes(outcome: &Result<(), Stopped>, encoder: &str) -> bool {
    matches!(outcome, Err(Stopped::Encoder(_))) && encoder != SOFTWARE_ENCODER
}

fn encode(
    feed: &mut FrameFeed,
    encoder: &mut moq_video::encode::Encoder,
    producer: &mut moq_video::encode::Producer,
    steering: Steering<'_>,
) -> Result<(), Stopped> {
    let Steering {
        scale,
        lane,
        keyframe,
        stop,
        local,
        metrics,
    } = steering;
    // For as long as this producer: the camera turned off and on again is
    // the same track, and its timeline must keep rising (see `stamps`).
    let mut rising = super::stamps::Rising::default();
    loop {
        // The only way out other than the camera going away. Checked per frame
        // rather than per sleep, so a busy encoder still notices promptly.
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        // Applied here because this thread owns the encoder. A rate arriving
        // mid-frame waits for the next one, which is well inside what the
        // control loop ramps over.
        if let Some(bitrate) = lane.take() {
            if let Err(error) = encoder.set_bitrate(moq_net::bandwidth::Rate::from_bps(bitrate)) {
                tracing::warn!("could not move the encoder to {bitrate} bps: {error:#}");
            }
        }
        // A peer that has just rejoined is holding a frozen tile until it gets
        // one of these. Asked for on the next frame rather than immediately,
        // which is the same rule the bitrate above follows and for the same
        // reason: this thread owns the encoder.
        if keyframe.swap(false, std::sync::atomic::Ordering::Relaxed) {
            if let Err(error) = encoder.cut() {
                tracing::warn!("could not ask the encoder for a keyframe: {error:#}");
            }
        }
        // `take` errors only when every tap has dropped: the camera going away
        // for good, rather than a gap between frames.
        let frame = match feed.take() {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            Err(_) => return Ok(()),
        };

        let Some(surface) = frame.to_i420_surface(scale) else {
            continue;
        };
        let stamp = rising.stamp(frame.timestamp.as_micros() as u64);
        // The encoder paces on these, so a wrong one stalls the stream.
        let Ok(timestamp) = moq_net::Timestamp::from_micros(stamp) else {
            tracing::warn!("frame timestamp out of range; skipping");
            continue;
        };
        // Pairs this frame's camera time with the wall clock, for the presence
        // document to carry. Taken as the frame is encoded, a few ms after
        // capture, so measured delays read that much short -- small next to
        // what is being measured, and stated where the numbers are reported.
        // The published stamp, not the capture's: it is what a viewer reads.
        local.set_clock(Clock::now(stamp));
        let encoded = encoder
            .encode(&moq_video::Frame::new(surface, timestamp))
            .map_err(|error| Stopped::Encoder(format!("encode failed: {error:#}")))?;
        if !encoded.is_empty() {
            if let Err(error) = producer.publish(&encoded) {
                match super::stamps::refusal(&error) {
                    // Skipped, and a keyframe asked for, since what follows
                    // a skipped frame cannot be decoded without one.
                    Some(refusal) => {
                        if refusal == super::stamps::Refusal::Rewind {
                            tracing::warn!("[video] the track refused a frame stamped below its edge; skipping it and moving on");
                            rising.refused();
                        } else {
                            tracing::debug!("[video] skipping a frame until the next keyframe");
                        }
                        if let Err(error) = encoder.cut() {
                            tracing::warn!("could not ask the encoder for a keyframe: {error:#}");
                        }
                        continue;
                    }
                    None => return Err(Stopped::Publish(format!("publish failed: {error:#}"))),
                }
            }
            metrics.camera_sent(payload_bytes(&encoded));
            metrics.camera_frames(encoded.len());
        }
    }
}

/// Everywhere the attendance loop reports, gathered so its signature stays
/// readable rather than growing a parameter per audience.
struct Reports {
    /// What the call is doing.
    status: UnboundedSender<RemoteStatus>,
    /// Who is here, and where their frames land.
    participants: UnboundedSender<ParticipantEvent>,
    /// Said once, when the last of them has gone deliberately.
    hangup: UnboundedSender<String>,
    /// Link numbers, for the stats file's link line.
    metrics: Arc<Metrics>,
    /// Whether this is a room on a relay rather than one peer.
    ///
    /// Changes two rules. Everyone leaving does not end it -- a room stays open
    /// for whoever comes next, and only Hang up leaves. And no single peer is
    /// "who we are connected to", so the status line stays on the room.
    in_room: bool,
    /// Our own subroom and the volume for everyone outside it, which each
    /// peer's audio task reads on every frame.
    rooms: audio::Rooms,
    /// Where hosts heard in this room are reported, on a join.
    news: Option<RoomNews>,
    /// What reading each person's route needs beside its chain.
    routing: Routing,
}

/// What reading a route needs beside its hop chain (see `arrival`).
struct Routing {
    /// Our guests' session hops, while we host.
    guests: Arc<arrival::Guests>,
    /// Our own origin's hop.
    ours: u64,
    /// The endpoint id we joined at, over iroh: a person, when the room is
    /// hosted with `--host`, or else a relay that is never in the room.
    far: Option<String>,
}

/// Watches who is here, and gives each of them a tile and a decoder.
///
/// **An announcement is the join, and its withdrawal is the leave** -- there is
/// no separate presence mechanism to keep in step with this one. `announced()`
/// yields `Some` for a broadcast appearing and `None` for it going away, so
/// membership is read off the same stream the media arrives on.
///
/// This replaced two separate waits on one well-known path, which was the thing
/// that made a call exactly two people. Through a relay this is the whole room:
/// the relay roots our subscription at the room's path, so every participant in
/// it is announced here relative to that path, as `<id>/camera.hang`.
async fn attend(
    inbound: moq_net::origin::Consumer,
    me: String,
    reports: Reports,
    engine: Option<moq_audio::playback::Engine>,
) {
    let Reports {
        status,
        participants,
        hangup,
        metrics,
        in_room,
        rooms,
        news,
        routing,
    } = reports;
    // The stats file, if asked for (`--stats`). A file that cannot be opened is
    // a line in the log, not a failed call.
    let mut stats_file = crate::args::Args::get().stats.as_ref().and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| tracing::warn!("cannot write stats to {}: {error}", path.display()))
            .ok()
    });
    // Once a second for the whole call: the connection report, each peer's
    // delays drained into figures, and the stats file when there is one.
    let mut stats_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut reporter = super::report::Reporter::default();
    let mut announced = inbound.announced();
    // Dropping an entry aborts that participant's decoders, so leaving is the
    // same operation as the attempt ending.
    let mut attending: std::collections::HashMap<String, Vec<Abort>> =
        std::collections::HashMap::new();
    // When each person's camera last went, so its return can say how long
    // their tile was gone: what a move of the call is judged by from logs.
    let mut gone: std::collections::HashMap<String, std::time::Instant> = std::collections::HashMap::new();
    // **Cloned per participant, never taken.** Clones of an engine share its
    // device and its mix, so each peer becomes one more sink in the same output
    // -- and the echo canceller, built from this engine, subtracts that whole
    // mix. `None` only when there is no output device at all.
    // Who has a live control broadcast. Presence is this, not the camera: a
    // participant with no video is still on the call, and this is the set whose
    // emptying means the call is over.
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut listening: std::collections::HashMap<String, Abort> =
        std::collections::HashMap::new();
    // Whose shared screen we are decoding. A map of its own: a share comes and
    // goes without its sharer doing either.
    let mut sharing: std::collections::HashMap<String, Abort> = std::collections::HashMap::new();
    // One per person, shared by their presence reader and their tile, so what
    // they say about their camera reaches the tile showing it. Either
    // broadcast may be announced first, hence get-or-insert at both.
    let mut people: std::collections::HashMap<String, Arc<PeerStats>> =
        std::collections::HashMap::new();
    // Where the per-participant readers report a goodbye. One arbiter, so the
    // rule for "is the call over" is written once.
    let (saying, mut said) = futures_channel::mpsc::unbounded::<String>();
    // What peers ask of us, shared by every presence reader in this call.
    let asked = Asked {
        me: me.clone(),
        rooms: rooms.clone(),
        seen: Arc::default(),
    };
    // Each person's route, as the hop chain of their control broadcast's
    // announcement: everything of theirs is offered the same way, and this
    // one is there whether or not a camera is. Read once a second rather than
    // as it changes, so a host's presence has had time to name its hop.
    let mut chains: std::collections::HashMap<String, Vec<u64>> = std::collections::HashMap::new();
    let mut routes: std::collections::HashMap<String, arrival::Arrival> = std::collections::HashMap::new();

    loop {
        let update = tokio::select! {
            update = announced.next() => match update {
                Some(update) => update,
                None => break,
            },
            _ = stats_tick.tick() => {
                reporter.tick(std::time::Instant::now(), &metrics, &rooms.local, &people, &present, &me);
                note_routes(&chains, &people, &mut routes, &routing, in_room);
                if let Some(file) = stats_file.as_mut() {
                    write_stats(file, &me, &people, &metrics);
                }
                continue;
            }
            goodbye = said.next() => {
                if let Some(id) = goodbye {
                    tracing::info!("{id} is leaving");
                    listening.remove(&id);
                    if present.remove(&id) && present.is_empty() {
                        alone(&hangup, id, in_room);
                    }
                }
                continue;
            }
        };
        let path = update.prefix.as_str().to_string();
        // Our own broadcasts come back to us on a join over several sessions,
        // through whoever bridges them: each session offers from a gate with a
        // hop of its own (see `sessions::Gate`), so a session's split horizon
        // no longer hides them. Skipped before asking for the broadcast, so
        // reading them back opens nothing.
        let owner = controller_of(&path)
            .or_else(|| sharer_of(&path))
            .or_else(|| participant_of(&path));
        if owner == Some(me.as_str()) {
            continue;
        }
        if let Some(id) = controller_of(&path) {
            match update.kind {
                moq_net::announce::Kind::Retracted => {
                    chains.remove(id);
                    if routes.remove(id).is_some() {
                        if let Some(stats) = people.get(id) {
                            stats.set_arrival(None);
                        }
                    }
                }
                _ => {
                    chains.insert(id.to_string(), update.route.hops.as_slice().iter().map(|hop| hop.id()).collect());
                }
            }
        }
        // An announcement names a path; the broadcast is asked for by that path.
        // A re-priced route changes nothing we act on, and a retraction is the
        // goodbye, carried as `None` the way the old cursor carried it.
        let broadcast = match update.kind {
            moq_net::announce::Kind::Updated => continue,
            moq_net::announce::Kind::Retracted => None,
            moq_net::announce::Kind::Announced => match inbound.request_broadcast(path.as_str()).await {
                Ok(broadcast) => Some(broadcast),
                Err(error) => {
                    tracing::warn!("could not open the broadcast at {path}: {error:#}");
                    continue;
                }
            },
        };

        if let Some(id) = controller_of(&path) {
            if id == me {
                continue;
            }
            match broadcast {
                Some(broadcast) => {
                    present.insert(id.to_string());
                    listening.insert(
                        id.to_string(),
                        Abort(tokio::spawn(listen(
                            inbound.clone(),
                            broadcast,
                            id.to_string(),
                            saying.clone(),
                            people.entry(id.to_string()).or_default().clone(),
                            asked.clone(),
                            news.clone(),
                        ))),
                    );
                }
                None => {
                    // An unannounce is the authoritative goodbye. Deliberate,
                    // it arrives at once; abrupt, it waits out the linger, by
                    // which time the reconnect has already given up.
                    //
                    // **The reader is let go, not killed.** Through a relay the
                    // unannounce can overtake the last document -- the
                    // `Leaving` said just before it -- and aborting the reader
                    // here threw that word away: a hang-up then looked exactly
                    // like a drop. It ends by itself once the document arrives
                    // or the track closes; the grace only bounds a reader that
                    // never hears either.
                    if let Some(reader) = listening.remove(id) {
                        tokio::spawn(async move {
                            tokio::time::sleep(GOODBYE_GRACE).await;
                            drop(reader);
                        });
                    }
                    if present.remove(id) && present.is_empty() {
                        alone(&hangup, id.to_string(), in_room);
                    }
                    // Their screen goes with them, rather than lingering on
                    // the relay after a leave. Their camera is left to its own
                    // retract, which `Leaving` already hastens.
                    if sharing.remove(id).is_some() {
                        tracing::info!("{id} left while sharing; dropping their screen");
                        let _ = participants.unbounded_send(ParticipantEvent::Left {
                            id: id.to_string(),
                            feed: Feed::Screen,
                        });
                    }
                    if !attending.contains_key(id) {
                        people.remove(id);
                    }
                }
            }
            continue;
        }

        if let Some(id) = sharer_of(&path) {
            if id == me {
                continue;
            }
            match broadcast {
                Some(broadcast) => {
                    tracing::info!("{id} is sharing a screen");
                    let slot = FrameSlot::default();
                    // The person's entry, so the share's tile reads their room.
                    let stats = people.entry(id.to_string()).or_default().clone();
                    let _ = participants.unbounded_send(ParticipantEvent::Joined {
                        id: id.to_string(),
                        slot: slot.clone(),
                        stats,
                        feed: Feed::Screen,
                    });
                    // The seam for a shared screen's sound, which is not sent
                    // yet. It would ride in this broadcast, and be played as
                    // the camera branch below plays a voice: with a clone of
                    // `engine`, `audio::subscribe(broadcast, ..)`.
                    let _audio: Option<moq_audio::playback::Engine> = None;
                    sharing.insert(
                        id.to_string(),
                        Abort(tokio::spawn(watch(
                            follow::Asking {
                                inbound: inbound.clone(),
                                path: path.clone(),
                            },
                            broadcast,
                            slot,
                            format!("{id}/screen"),
                            // A screen arriving is not the call connecting.
                            None,
                            // **A meter of its own**: `watch` writes frame
                            // rate and delay into it, and the share must not
                            // overwrite their camera's numbers.
                            Arc::new(PeerStats::default()),
                            metrics.clone(),
                        ))),
                    );
                }
                None => {
                    tracing::info!("{id} stopped sharing");
                    sharing.remove(id);
                    if !present.contains(id) && !attending.contains_key(id) {
                        people.remove(id);
                    }
                    let _ = participants.unbounded_send(ParticipantEvent::Left {
                        id: id.to_string(),
                        feed: Feed::Screen,
                    });
                }
            }
            continue;
        }

        let Some(id) = participant_of(&path) else {
            tracing::debug!("ignoring an announcement at {path}");
            continue;
        };
        // Cannot happen while each side keeps its own origin. Through a relay
        // it should not either -- the relay does not announce a session's own
        // broadcasts back to it -- but that is the relay's rule, not ours, so
        // the check stays.
        if id == me {
            continue;
        }

        let Some(broadcast) = broadcast else {
            tracing::info!("{id} left");
            gone.insert(id.to_string(), std::time::Instant::now());
            attending.remove(id);
            // Forgotten once all of their broadcasts are gone, so a rejoin
            // with the same key starts from "on" rather than their last word.
            if !present.contains(id) && !sharing.contains_key(id) {
                people.remove(id);
            }
            let _ = participants.unbounded_send(ParticipantEvent::Left {
                id: id.to_string(),
                feed: Feed::Camera,
            });
            continue;
        };

        tracing::info!("{id} joined");
        if let Some(left) = gone.remove(id) {
            tracing::info!("{id} back after {} ms", left.elapsed().as_millis());
        }
        let slot = FrameSlot::default();
        let stats = people.entry(id.to_string()).or_default().clone();
        let _ = participants.unbounded_send(ParticipantEvent::Joined {
            id: id.to_string(),
            slot: slot.clone(),
            stats: stats.clone(),
            feed: Feed::Camera,
        });

        let mut tasks = vec![Abort(tokio::spawn(watch(
            follow::Asking {
                inbound: inbound.clone(),
                path: path.clone(),
            },
            broadcast.clone(),
            slot,
            id.to_string(),
            (!in_room).then(|| status.clone()),
            stats.clone(),
            metrics.clone(),
        )))];
        if let Some(engine) = engine.clone() {
            let peer = id.to_string();
            let stats = stats.clone();
            let rooms = rooms.clone();
            if crate::args::Args::get().audio_stall.is_some() {
                static IGNORED: std::sync::Once = std::sync::Once::new();
                IGNORED.call_once(|| tracing::warn!("--audio-stall is for --loopback only; ignored in a call"));
            }
            let asking = follow::Asking {
                inbound: inbound.clone(),
                path: path.clone(),
            };
            tasks.push(Abort(tokio::spawn(async move {
                audio::subscribe(Some(asking), broadcast, peer, engine, stats, Some(rooms), None).await;
            })));
        } else if !crate::args::Args::get().no_audio {
            tracing::warn!("[audio] {id} cannot be heard: there is no output device");
        }
        attending.insert(id.to_string(), tasks);
    }
}

/// Reads each person's route from the chain last announced to us, and on a
/// change sets it on their entry (which their tile polls) and logs it, so a
/// scripted run can be judged from the log. Nobody is read before their
/// presence has arrived: until then, a host would read as the relay.
///
/// Outside a room (a call dialled to one peer) there is nobody between.
fn note_routes(
    chains: &std::collections::HashMap<String, Vec<u64>>,
    people: &std::collections::HashMap<String, Arc<PeerStats>>,
    said: &mut std::collections::HashMap<String, arrival::Arrival>,
    routing: &Routing,
    in_room: bool,
) {
    let hosts: std::collections::BTreeMap<u64, String> = people
        .iter()
        .filter_map(|(id, stats)| stats.host_hop().map(|hop| (hop, id.clone())))
        .collect();
    let guests = routing.guests.hops();
    let far = routing.far.as_deref().filter(|far| people.contains_key(*far));
    for (id, hops) in chains {
        let Some(stats) = people.get(id).filter(|stats| stats.heard()) else { continue };
        let arrival = if in_room {
            arrival::read(
                hops,
                &arrival::Known {
                    theirs: stats.host_hop(),
                    hosts: &hosts,
                    guests: &guests,
                    ours: routing.ours,
                    far,
                    who: id,
                },
            )
        } else {
            Some(arrival::Arrival::Direct)
        };
        let Some(arrival) = arrival else { continue };
        if said.get(id) != Some(&arrival) {
            tracing::info!("{id} now arrives {}", arrival::arrival_log(&arrival));
            stats.set_arrival(Some(arrival.clone()));
            said.insert(id.clone(), arrival);
        }
    }
}

/// How our presence track is written: **every document a group of its own**,
/// as the library writes its catalogs, never a snapshot followed by deltas.
/// The library carries a subscription across a change of route at a group
/// boundary; a group held open for deltas has none, and a reader of one was
/// seen to stall for good, with no error, at the first move of the call --
/// every move changes routes. Readers of either shape read this one.
fn presence_config() -> moq_json::snapshot::Config {
    moq_json::snapshot::Config::default().with_delta_ratio(0)
}

/// Publishes one presence document, logging rather than failing: a word that
/// does not get out is a degraded call, not a broken one.
fn say(presence: &std::sync::Mutex<moq_json::snapshot::Producer<Presence>>, now: Presence) {
    let Ok(mut presence) = presence.lock() else {
        return;
    };
    if let Err(error) = presence.update(&now) {
        tracing::warn!("could not publish our presence: {error:#}");
    }
}

/// Re-announces our presence whenever our camera, microphone or name changes, and
/// every [`CLOCK_EVERY`] regardless, so the clock pairing it carries stays fresh.
///
/// **Polled, four times a second.** Two booleans flipped by a click do not earn
/// a notification channel, and a quarter-second lag on "camera off" is not
/// something anyone can see. Runs until the call aborts it.
async fn announce(
    presence: Arc<std::sync::Mutex<moq_json::snapshot::Producer<Presence>>>,
    local: Arc<PeerStats>,
) {
    let mut said = Presence::here(&local);
    let mut said_at = tokio::time::Instant::now();
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
    loop {
        ticker.tick().await;
        local.expire_asks(ASK_FOR);
        let now = Presence::here(&local);
        // What a person did, for the log. The connection report changes with
        // the link, and is only logged as it starts and stops.
        // A move of the call is in it too: followers act on it as it is said,
        // so it must not wait for the clock refresh.
        let changed = (now.camera, now.mic, &now.room, &now.requests, now.sharing, &now.name, now.link.is_some(), &now.host, &now.moved)
            != (said.camera, said.mic, &said.room, &said.requests, said.sharing, &said.name, said.link.is_some(), &said.host, &said.moved);
        if changed {
            let asking = match now.requests.len() {
                0 => String::new(),
                n => format!(", asking {n}"),
            };
            let sharing = if now.sharing { ", sharing" } else { "" };
            // Whether, not what: the log keys people by id.
            let named = if now.name.is_some() { ", named" } else { "" };
            let reporting = if now.link.is_some() { ", reporting the connection" } else { "" };
            // Whether, never the door.
            let hosting = if now.host.is_some() { ", hosting" } else { "" };
            // Where we say the call is, so a scripted run can see the
            // choice leave the chooser and each follower repeat it.
            let moved = match &now.moved {
                Some(on) => format!(", call on {}", super::moves::describe(on.to.as_deref())),
                None => String::new(),
            };
            tracing::info!(
                "now camera {}, mic {}, room {}{asking}{sharing}{named}{reporting}{hosting}{moved}",
                on_off(now.camera),
                on_off(now.mic),
                room_name(&now.room)
            );
        }
        // The switch flipping goes out at once, either way (the report's
        // presence is in `changed`); a report's new figures wait for the
        // clock refresh, which is also when the reporter makes them.
        let withdrawn = now.seen.is_empty() && !said.seen.is_empty();
        if changed || withdrawn || said_at.elapsed() >= CLOCK_EVERY {
            say(&presence, now.clone());
            said = now;
            said_at = tokio::time::Instant::now();
        }
    }
}

/// How often our clock pairing is re-said. Clocks drift slowly; this is about
/// a peer that joins late having one soon, not about precision.
const CLOCK_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a request is said before we give up on it. An invite nobody
/// accepts simply goes away; there is no decline.
const ASK_FOR: std::time::Duration = std::time::Duration::from_secs(30);

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// A subroom for the log: its name, or "main" for none.
fn room_name(room: &Option<String>) -> &str {
    room.as_deref().unwrap_or("main")
}

/// Appends one second's measurements: a line per person we receive, and one
/// for our link. Read by `call-report`.
///
/// Delays are only as good as the two machines' clocks agree (NTP keeps that to
/// a few ms), and run from encode, a few ms after capture, to decode -- the
/// camera sensor and the display are not in them.
fn write_stats(
    file: &mut std::fs::File,
    me: &str,
    people: &std::collections::HashMap<String, Arc<PeerStats>>,
    metrics: &Metrics,
) {
    use std::io::Write as _;
    let t = super::metrics::wall_us() as f64 / 1e6;
    let mut lines = String::new();
    for (peer, stats) in people {
        // Drained into these once a second by the reporter, just before.
        let (p50, p95) = stats.delay_percentiles();
        let (gaps, dry) = stats.audio_trouble();
        let sync = stats.audio_sync();
        let line = serde_json::json!({
            "t": t, "me": me, "peer": peer,
            "camera": stats.camera_on(), "mic": stats.mic_on(),
            "fps": stats.video_fps(),
            "delay_p50_ms": p50, "delay_p95_ms": p95,
            "audio_fps": stats.take_audio_frames(),
            "audio_gaps": gaps, "audio_dry": dry,
            "audio_buffered_ms": sync.map(|s| s.buffered_ms),
            "audio_buffered_min_ms": sync.map(|s| s.buffered_min_ms),
            "audio_buffered_max_ms": sync.map(|s| s.buffered_max_ms),
            "audio_floor_ms": sync.map(|s| s.floor_ms),
            "audio_late_ms": sync.map(|s| s.late_ms),
            "video_late_ms": stats.video_late_ms(),
            "audio_rate": sync.map(|s| s.rate),
            "audio_removed_ms": sync.map(|s| s.removed_ms),
            "audio_skips": sync.map(|s| s.skips),
            "audio_overflow_ms": sync.map(|s| s.overflow_ms),
            "audio_discarded_ms": sync.map(|s| s.discarded_ms),
        });
        lines.push_str(&line.to_string());
        lines.push('\n');
    }
    let aec = metrics.aec().unwrap_or_default();
    let hosting = metrics.hosting();
    let link = serde_json::json!({
        "t": t, "me": me, "link": true,
        "rtt_ms": metrics.rtt(), "loss_pct": metrics.loss(), "up_kbps": metrics.send_kbps(),
        "camera_cap_kbps": metrics.camera_cap_kbps(), "camera_sent_kbps": metrics.camera_sent_kbps(),
        "camera_sent_fps": metrics.camera_fps(),
        "share_cap_kbps": metrics.share_cap_kbps(), "share_sent_kbps": metrics.share_sent_kbps(),
        "estimate_kbps": metrics.estimate_kbps(),
        "camera_slice_kbps": metrics.camera_slice_kbps(),
        "share_slice_kbps": metrics.share_slice_kbps(), "share_target_kbps": metrics.share_target_kbps(),
        "aec_erle_db": aec.erle_db, "aec_erl_db": aec.erl_db,
        "aec_delay_ms": aec.delay_ms,
        "aec_reference_frames": aec.reference_frames, "aec_reference_rms": aec.reference_rms,
        "hosting": hosting.is_some(), "guests": hosting.map(|h| h.guests),
        "guests_pulling": hosting.map(|h| h.pulling),
        "forwarded_kbps": hosting.map(|h| h.forwarded), "own_out_kbps": hosting.map(|h| h.own),
    });
    lines.push_str(&link.to_string());
    lines.push('\n');
    if let Err(error) = file.write_all(lines.as_bytes()) {
        tracing::warn!("could not write stats: {error}");
    }
}

/// The last other person has gone, deliberately.
///
/// With one peer that ends the call: there is nobody left to talk to. In a room
/// it does not -- the room is still there for whoever comes next, and leaving
/// it is the user's decision, made with Hang up.
fn alone(hangup: &UnboundedSender<String>, last: String, in_room: bool) {
    if in_room {
        tracing::info!("{last} left; alone in the room, staying");
    } else {
        let _ = hangup.unbounded_send(last);
    }
}

/// Reads one participant's state document for as long as they publish it.
///
/// **A document, not a mailbox.** Every value is the whole of what they are
/// saying, so nothing here tracks what it has seen before or asks for anything
/// to be repeated. Saying *leaving* is the one thing that ends a call from the
/// far side rather than from the transport.
///
/// **Asked for again when the subscription fails** while they are still
/// announced. Every move of the call redials a session, and a presence read
/// through the session that went was seen to fail ("dropped", "internal
/// error") with the path still announced: the announcement only changes
/// route, so nothing else would ever start a new reader, and everything they
/// said from then on -- a mute, a name, a move of the call -- was lost for
/// the rest of the call. The library says as much: a broadcast whose route
/// changes may end, and the next request serves it again. A reader that is
/// asking again ends like any other, when the retraction lets it go.
async fn listen(
    inbound: moq_net::origin::Consumer,
    broadcast: moq_net::broadcast::Consumer,
    id: String,
    saying: UnboundedSender<String>,
    stats: Arc<PeerStats>,
    asked: Asked,
    news: Option<RoomNews>,
) {
    // The door they last said, while this reader has heard them hosting.
    // What decides "they stopped": a document without one, after one with.
    let mut offered: Option<String> = None;
    // Their invite lasts exactly as long as this reader: the leave aborts it
    // rather than letting it return, so a guard is what drops it.
    let _invited = Invited {
        local: asked.rooms.local.clone(),
        from: id.clone(),
    };
    let mut broadcast = Some(broadcast);
    // Whether we are asking again, so an outage is said once and its end too.
    let mut lost = false;
    loop {
        let broadcast = match broadcast.take() {
            Some(broadcast) => broadcast,
            None => {
                tokio::time::sleep(follow::AGAIN).await;
                match inbound.request_broadcast(control_path(&id).as_str()).await {
                    Ok(broadcast) => broadcast,
                    Err(error) => {
                        tracing::debug!("{id}'s presence is not there to ask for yet: {error:#}");
                        continue;
                    }
                }
            }
        };
        let mut presence = match subscribe_presence(&broadcast).await {
            Ok(presence) => presence,
            Err(error) => {
                if !std::mem::replace(&mut lost, true) {
                    tracing::warn!("could not subscribe to {id}'s presence: {error}; asking again");
                }
                continue;
            }
        };
        loop {
            match presence.next().await {
                Ok(Some(Presence {
                    state,
                    camera,
                    mic,
                    clock,
                    room,
                    requests,
                    sharing,
                    name,
                    link,
                    seen,
                    host,
                    moved,
                })) => {
                    let room = subroom(room);
                    if let Some(clock) = clock {
                        stats.set_clock(clock);
                    }
                    tracing::debug!(
                        "{id} says {state:?}, camera {}, mic {}, room {}",
                        on_off(camera),
                        on_off(mic),
                        room_name(&room)
                    );
                    // Their choice, for their tile: a card instead of a frozen
                    // frame, a badge instead of silence nobody can explain.
                    stats.set_camera(camera);
                    stats.set_mic(mic);
                    stats.set_sharing(sharing);
                    // Never trusted as sent: cleaned here whatever their build
                    // did, and shown only.
                    stats.set_name(name.as_deref().and_then(crate::name::clean));
                    // Cleaned again here, whatever their build did: a report is a
                    // claim like a name, and their own id has no place in it.
                    stats.set_link(link.map(Link::clean));
                    stats.set_seen(quality::clean_seen(seen, &id));
                    // Read by their audio task on every frame, which is what sets
                    // how loud they are to us.
                    stats.set_room(room.clone());
                    asked.act(&id, &room, mic, &requests);
                    stats.set_hosts_room(host.is_some());
                    // Their hop names them in a route; the door stays here.
                    stats.set_host_hop(host.clone().and_then(door::Offer::clean).map(|offer| offer.hop));
                    stats.set_heard();
                    heard_hosting(&id, state, host, &mut offered, news.as_ref());
                    followed(&asked.rooms.local, moved);
                    if std::mem::take(&mut lost) {
                        tracing::info!("{id}'s presence is back");
                    }
                    if state == State::Leaving {
                        // Reported, not acted on: whether the call is over is a
                        // question about everyone, and the loop that tracks
                        // everyone answers it.
                        let _ = saying.unbounded_send(id);
                        return;
                    }
                }
                // Ended without their goodbye: the same as a failure, since
                // only the retraction says they went.
                Ok(None) => {
                    if !std::mem::replace(&mut lost, true) {
                        tracing::info!("{id}'s presence ended without a goodbye; asking again");
                    }
                    break;
                }
                Err(error) => {
                    if !std::mem::replace(&mut lost, true) {
                        tracing::info!("lost {id}'s presence ({error:#}); asking again");
                    }
                    break;
                }
            }
        }
    }
}

/// Subscribes to a participant's presence track on `broadcast`.
async fn subscribe_presence(
    broadcast: &moq_net::broadcast::Consumer,
) -> Result<moq_json::snapshot::Consumer<Presence>, String> {
    let track = broadcast.track(PRESENCE).map_err(|error| format!("no presence track: {error:#}"))?;
    // Preempts media, the way hang prioritises its own catalog: this is small
    // and structural, and late it is worthless.
    let subscription = moq_net::track::Subscription::default().with_priority(CONTROL_PRIORITY);
    let track = track.subscribe(subscription).await.map_err(|error| format!("{error:#}"))?;
    Ok(moq_json::snapshot::Consumer::<Presence>::new(
        track,
        moq_json::snapshot::consumer::Config::default(),
    ))
}

/// Whether a catalog read ended because the transport under it did -- the
/// session it came through closed, or its route went with a move or a relay
/// going down -- rather than because what arrived could not be read. The
/// first is a reader ending like any other in a call whose routes change,
/// said where the reader ends; only the second is a fault of ours or theirs.
pub(super) fn catalog_cut_off(error: &moq_mux::Error) -> bool {
    matches!(error, moq_mux::Error::Moq(_) | moq_mux::Error::Json(moq_json::Error::Net(_)))
}

/// Takes in where someone says the call is, if we follow moves at all, and
/// says so when it changes where we follow it to.
fn followed(local: &PeerStats, moved: Option<super::moves::CallOn>) {
    let Some(moved) = moved.filter(|_| local.move_calls()) else { return };
    let (to, by) = (moved.to.clone(), moved.by.clone());
    if local.hear_move(moved) {
        tracing::info!("the call moves to {} (said by {by})", super::moves::describe(to.as_deref()));
    }
}

/// What the call tells the interface about the hosts of the room it joined,
/// to keep with the room in `rooms.json`. Carries a door: no `Debug`.
pub(crate) enum HostNews {
    /// `id` says it hosts the room behind `door`; `told_by` is whose
    /// presence said so, which today is always `id`'s own.
    Heard {
        room: String,
        id: String,
        door: String,
        told_by: String,
    },
    /// `id` said it hosted the room and has now said it does not.
    Stopped { room: String, id: String },
}

/// Where a presence reader reports the hosts it hears: the room's identity
/// (`recent::identity`) and the interface's channel.
#[derive(Clone)]
struct RoomNews {
    room: String,
    to: UnboundedSender<HostNews>,
}

/// Acts on what one document says about hosting.
///
/// **Sent on a change only**: the first door heard from them, a new door, or
/// the switch going off, because every message wakes the page. A document
/// without one, from someone this reader heard hosting, is a host that has
/// stopped, and the only thing that forgets it: leaving says nothing about
/// whether they will host next time, and silence is what a host looks like on
/// the day it is needed.
fn heard_hosting(
    id: &str,
    state: State,
    host: Option<door::Offer>,
    offered: &mut Option<String>,
    news: Option<&RoomNews>,
) {
    match host.and_then(door::Offer::clean) {
        Some(offer) => {
            if offered.as_deref() == Some(offer.door.as_str()) {
                return;
            }
            tracing::info!("{id} hosts this room; keeping it with the room");
            if let Some(news) = news {
                let _ = news.to.unbounded_send(HostNews::Heard {
                    room: news.room.clone(),
                    id: id.to_string(),
                    door: offer.door.clone(),
                    told_by: id.to_string(),
                });
            }
            *offered = Some(offer.door);
        }
        // The goodbye is not a change of mind, whatever it carries.
        None if state == State::Leaving => {}
        None => {
            if offered.take().is_some() {
                tracing::info!("{id} stopped hosting this room; forgetting it");
                if let Some(news) = news {
                    let _ = news.to.unbounded_send(HostNews::Stopped {
                        room: news.room.clone(),
                        id: id.to_string(),
                    });
                }
            }
        }
    }
}

/// Whether a join holds several sessions: when the room has hosts to dial
/// beside its relay, or when we host it. Never anything else, and never a
/// join with neither, which stays the one session it always was.
fn dials_several(peering: &Peering, host_rooms: bool, has_hosts: bool) -> bool {
    matches!(peering, Peering::Join(_)) && (host_rooms || has_hosts)
}

/// Whether an attempt's inbound is its outbound: always for a room hosted
/// here, for a join only with hosting on as it starts, never otherwise.
fn shares_origin(peering: &Peering, host_rooms: bool) -> bool {
    match peering {
        Peering::Host(_) => true,
        Peering::Join(_) => host_rooms,
        Peering::Dial(_) | Peering::Listen | Peering::Loopback | Peering::Idle => false,
    }
}

/// The attempt's inbound origin: the outbound itself when shared, or one of
/// its own.
fn inbound_for(outbound: &moq_net::origin::Producer, shared: bool) -> moq_net::origin::Producer {
    if shared { outbound.clone() } else { moq_tokio::origin::spawn() }
}

/// Everything [`host_alongside`] needs, named rather than positional.
struct Alongside<'a> {
    config: &'a moq_tokio::client::Config,
    endpoint: &'a moq_tokio::iroh::Endpoint,
    /// The one origin the join and its guests share.
    shared: moq_net::origin::Producer,
    /// The room on the relay; its path is the room we host.
    url: &'a url::Url,
    doors: &'a door::Doors,
    /// Where the guests' links are gathered, for when nothing else carries
    /// our media.
    estimates: Arc<adapt::Estimates>,
    local: Arc<PeerStats>,
    forwarded: Option<Arc<Forwarded>>,
    /// Where our guests' session hops are kept, for reading routes.
    guests: Arc<arrival::Guests>,
    /// Where the last time a guest's session heard anything is kept.
    heard: Arc<quiet::Heard>,
}

/// Serves the room's members directly beside a join, behind the room's door,
/// and says so in presence, until hosting is switched off or the attempt
/// ends. `None` when it cannot: logged, and the join goes on regardless.
///
/// **The guests' links steer what we send only when nothing else carries
/// it.** They are gathered apart from the call's estimate, and the join
/// follows the session carrying our media; only a host with no outgoing
/// session left follows its guests instead (see `sessions`). Their
/// connection figures never reach the Network tab, so that it always shows
/// one link.
///
/// Switching hosting off ends this at once and every guest's session with
/// it; switching it on again takes effect from the next join, because the
/// origins are wired as the attempt starts.
async fn host_alongside(args: Alongside<'_>) -> Option<Abort> {
    let Alongside {
        config,
        endpoint,
        shared,
        url,
        doors,
        estimates,
        local,
        forwarded,
        guests,
        heard,
    } = args;
    let room = url.path().trim_matches('/').to_string();
    if room.is_empty() {
        tracing::info!("not hosting: this room is the relay's root, which has no name a guest could ask for");
        return None;
    }
    let Some(entry) = doors.get(&crate::recent::identity(url)) else {
        tracing::warn!("not hosting: no door was issued for this room");
        return None;
    };
    let hosting = door::Hosting::with_door(room, entry);
    let served = serve(Serve {
        config,
        endpoint,
        publisher: shared.consume(),
        subscriber: shared.clone(),
        door: Some(hosting.clone()),
        estimates: Some(estimates),
        // The guests' figures stay out of the Network tab, which shows the
        // link carrying our media; see the doc above.
        metrics: Arc::new(Metrics::default()),
        forwarded: forwarded.clone(),
        guests: Some(guests),
        heard: Some(heard),
    })
    .await;
    let (accepting, mut events) = match served {
        Ok(served) => served,
        Err(error) => {
            tracing::warn!("not hosting: {error}");
            return None;
        }
    };
    local.set_offer(Some(hosting.offer(shared.hop().id())));
    if let Some(forwarded) = &forwarded {
        forwarded.set_serving(true);
    }
    tracing::info!("hosting this room for its members as well as joining it");
    Some(Abort(tokio::spawn(async move {
        let mut accepting = Some(accepting);
        let mut live = 0usize;
        // Polled as the presence announcer polls: a quarter of a second to
        // notice a switch is not something anyone can see.
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            tokio::select! {
                _ = ticker.tick() => if !local.host_rooms() {
                    tracing::info!("hosting switched off; turning {live} guests away");
                    break;
                },
                event = events.next() => match event {
                    Some(true) => {
                        live += 1;
                        tracing::info!("a guest came in directly; {live} connected");
                    }
                    Some(false) => {
                        live = live.saturating_sub(1);
                        tracing::info!("a direct guest's session ended; {live} connected");
                    }
                    None => {
                        tracing::warn!("stopped hosting: the server ended");
                        break;
                    }
                },
            }
        }
        // The server and its sessions first, then the word: nobody should
        // read the door after it stopped opening.
        accepting.take();
        local.set_offer(None);
        if let Some(forwarded) = &forwarded {
            forwarded.set_serving(false);
        }
    })))
}

/// What a presence reader needs to act on requests addressed to us: who we
/// are, our own entry and mic, and what this call has already honoured.
#[derive(Clone)]
struct Asked {
    me: String,
    /// `audio::Rooms` is exactly our entry and our controls, reused as is.
    rooms: audio::Rooms,
    /// `(from, nonce)` of every mute honoured in this call. Per call, not per
    /// reader: a peer whose control broadcast drops and returns gets a fresh
    /// reader, which would otherwise honour a standing mute a second time
    /// after we had unmuted.
    seen: Arc<std::sync::Mutex<std::collections::HashSet<(String, u64)>>>,
}

impl Asked {
    /// Acts on one document from `from`, whose room is `room` and whose mic
    /// is `mic`.
    fn act(&self, from: &str, room: &Option<String>, mic: bool, requests: &[Request]) {
        let local = &self.rooms.local;
        // As the asker: drop what they have already done.
        local.settle(from, room, mic);
        // As the addressee.
        let (mute, invited) = asked_of(&self.me, requests);
        if let Some(nonce) = mute {
            // Marked seen even when the mic is already off, so a later unmute
            // is not undone by the same request.
            if honour(&self.seen, from, nonce) && local.mic_on() {
                // The mic button's own path. `set_enabled` only flips a flag
                // on the publication, so it is fine from this task.
                self.rooms.controls.set_enabled(false);
                local.set_mic(false);
                local.set_muted_by(Some(from.to_string()));
                tracing::info!("muted by {from}");
            }
        }
        // Mirrored, not once per nonce: the invite stands exactly while their
        // document carries it and we are not already in their room.
        if invited && *room != local.room() {
            local.set_invite(from, room.clone());
        } else {
            local.drop_invite(from);
        }
    }
}

/// What `from`'s requests ask of us: a mute to honour (its nonce), and
/// whether we are invited. Requests to anyone else, and kinds from a later
/// build, are ignored.
fn asked_of(me: &str, requests: &[Request]) -> (Option<u64>, bool) {
    let mut mute = None;
    let mut invited = false;
    for request in requests.iter().filter(|request| request.to == me) {
        match request.kind {
            Ask::Mute => mute = Some(request.nonce),
            Ask::Invite => invited = true,
            Ask::Unknown => {}
        }
    }
    (mute, invited)
}

/// Whether `(from, nonce)` is new, marking it seen: a request is acted on
/// once however many times the document carrying it is read.
fn honour(seen: &std::sync::Mutex<std::collections::HashSet<(String, u64)>>, from: &str, nonce: u64) -> bool {
    seen.lock()
        .map(|mut seen| seen.insert((from.to_string(), nonce)))
        .unwrap_or(false)
}

/// Drops one inviter's invite however their reader ends -- it is aborted
/// when they leave, so nothing after its loop runs.
struct Invited {
    local: Arc<PeerStats>,
    from: String,
}

impl Drop for Invited {
    fn drop(&mut self) {
        self.local.drop_invite(&self.from);
    }
}

/// Plays one participant's video into their slot, from their own catalog,
/// for as long as they are here: across changes of route, a camera turned on
/// late, and a rendition that changes (see `follow`).
///
/// **The catalog is theirs, never a local guess.** An early version failed here
/// by passing a *local* config and a hardcoded track name instead, and got `Net(Remote(30))`
/// for it.
async fn watch(
    asking: follow::Asking,
    broadcast: moq_net::broadcast::Consumer,
    slot: FrameSlot,
    peer: String,
    // `None` in a room, where one peer's video arriving is not the call's
    // status -- see `Reports::in_room`.
    status: Option<UnboundedSender<RemoteStatus>>,
    stats: Arc<PeerStats>,
    metrics: Arc<Metrics>,
) {
    let video = Video {
        slot,
        peer: peer.clone(),
        status,
        stats,
        metrics,
        opened: false,
    };
    follow::follow(Some(asking), broadcast, peer, video).await;
}

/// A participant's video, as `follow` plays it.
struct Video {
    slot: FrameSlot,
    peer: String,
    status: Option<UnboundedSender<RemoteStatus>>,
    stats: Arc<PeerStats>,
    metrics: Arc<Metrics>,
    /// Whether a decoder has opened before, so a reopening after a change of
    /// route is not announced like the first.
    opened: bool,
}

impl follow::Reader for Video {
    const WHAT: &'static str = "video";

    fn pick(&self, catalog: &follow::Catalog) -> Option<follow::Rendition> {
        catalog.video.renditions.iter().next().map(|(name, config)| follow::Rendition::new(name, config))
    }

    fn heard(&mut self, first: bool, offered: bool) {
        // **The listening side can name its caller now.** iroh exposes no
        // client identity to a server, so the transport genuinely cannot say
        // who this is -- but the broadcast path can, and does. Said on their
        // first catalog rather than their first video, which a peer with the
        // camera off never sends.
        if !first {
            return;
        }
        if let Some(status) = &self.status {
            let _ = status.unbounded_send(RemoteStatus::Connected {
                peer: Some(self.peer.clone()),
            });
        }
        // **Waits for a video rendition rather than giving up.** A peer whose
        // camera is off publishes its microphone alone, and adds video when
        // the camera produces its first frame -- possibly mid-call.
        if !offered {
            tracing::info!("{} has no video yet; waiting for it", self.peer);
        }
    }

    fn play(
        &mut self,
        broadcast: &moq_net::broadcast::Consumer,
        catalog: &follow::Catalog,
        rendition: &follow::Rendition,
    ) -> follow::Playing {
        let config = catalog.video.renditions.get(&rendition.name).cloned();
        let broadcast = broadcast.clone();
        let name = rendition.name.clone();
        let (slot, peer, stats, metrics) = (self.slot.clone(), self.peer.clone(), self.stats.clone(), self.metrics.clone());
        let again = std::mem::replace(&mut self.opened, true);
        Box::pin(async move {
            let Some(config) = config else { return follow::Played::Failed };
            let mut consumer = match moq_video::decode::Consumer::new(
                &broadcast,
                &config,
                &name,
                // Default options: a zero `max_age`, so a stalled group is skipped as
                // soon as a newer one exists -- video's newest-wins posture.
                moq_video::decode::Options::default(),
            )
            .await
            {
                Ok(consumer) => consumer,
                // The broadcast went from under it: asked for again.
                Err(moq_video::Error::Net(error)) => {
                    tracing::debug!("could not subscribe to {peer}'s video: {error:#}");
                    return follow::Played::Lost;
                }
                Err(error) => {
                    tracing::error!("no decoder for {peer}'s video: {error:#}");
                    return follow::Played::Failed;
                }
            };
            if again {
                tracing::debug!("decoding {peer}'s video again with {}", consumer.name());
            } else {
                tracing::info!("decoding {peer}'s video with {}", consumer.name());
            }
            metrics.set_decoder(consumer.name());

            // The network half's own rate, independent of whether anything is painting.
            // `camera::paint::FrameRate` measures capture against the GPU; this measures
            // what actually arrived from the peer, which is the only number that tells
            // a stalled connection apart from an occluded window.
            let mut decoded = 0u32;
            let mut since = std::time::Instant::now();

            // Per tile, so a stalled peer shows as one tile at zero rather than hiding
            // behind whichever healthy peer wrote the shared number last.
            let meter = FpsMeter::new(stats.clone());

            while let Ok(Some(frame)) = consumer.read().await {
                meter.frame();
                stats.frame_arrived(frame.timestamp.as_micros() as u64);
                decoded += 1;
                if decoded == REPORT_EVERY {
                    let elapsed = since.elapsed().as_secs_f32();
                    if elapsed > 0.0 {
                        // Named, because in a room there are several of these lines
                        // and an unnamed rate cannot be told apart from the others.
                        tracing::debug!("[remote] {peer}: {:.1} fps decoded", decoded as f32 / elapsed);
                    }
                    decoded = 0;
                    since = std::time::Instant::now();
                }
                if let Some(frame) = from_decoded(frame) {
                    slot.publish(frame);
                }
            }
            follow::Played::Lost
        })
    }

    /// The producer is gone, so the last frame must come off the screen
    /// rather than sit there looking live.
    fn stopped(&mut self) {
        self.slot.clear();
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn a_participant_is_read_off_the_path_they_publish_under() {
        let me = "3929461daf6e4354";
        assert_eq!(participant_of(&camera_path(me)), Some(me));
    }

    #[test]
    fn a_controller_is_read_the_same_way() {
        let me = "3929461daf6e4354";
        assert_eq!(controller_of(&control_path(me)), Some(me));
        // The two must not read each other's paths: a control broadcast is not
        // media, and a camera is not a document.
        assert_eq!(controller_of(&camera_path(me)), None);
        assert_eq!(participant_of(&control_path(me)), None);
    }

    #[test]
    fn control_is_deliberately_not_a_hang_broadcast() {
        // Catalog-format detection keys off the suffix, and this carries a JSON
        // document rather than media.
        assert!(!control_path("anyone").ends_with(".hang"));
    }

    #[test]
    fn nothing_else_is_a_participant() {
        // The old shared path, a bare role, a foreign broadcast, and an empty
        // id: all of them have to read as "not one of ours" rather than as
        // somebody with a strange name.
        for path in ["call.hang", "camera.hang", "/camera.hang", "abc/screen.hang", "abc"] {
            assert_eq!(participant_of(path), None, "{path} should not be a participant");
        }
    }

    #[test]
    fn the_hang_suffix_stays_last() {
        // Catalog-format detection keys off it, and a path that loses it fails
        // as a timeout rather than an error -- so the id goes in front.
        assert!(camera_path("anyone").ends_with(".hang"));
        assert!(screen_path("anyone").ends_with(".hang"));
    }

    #[test]
    fn a_sharer_is_read_off_the_screen_path() {
        assert_eq!(sharer_of("abc/screen.hang"), Some("abc"));
        assert_eq!(sharer_of(&screen_path("3929461daf6e4354")), Some("3929461daf6e4354"));
        for path in ["abc/camera.hang", "abc/control", "screen.hang", "/screen.hang"] {
            assert_eq!(sharer_of(path), None, "{path} should not be a share");
        }
    }
}

#[cfg(test)]
mod presence_tests {
    use super::{HostNews, Link, Presence, RoomNews, Seen, State, asked_of, dials_several, door, followed, heard_hosting, honour, inbound_for, quality, shares_origin, subroom};
    use crate::args::Peering;
    use crate::net::metrics::{Ask, Clock, Request};
    use crate::net::quality::Quality;

    fn request(to: &str, kind: Ask, nonce: u64) -> Request {
        Request {
            to: to.to_string(),
            kind,
            nonce,
        }
    }

    #[test]
    fn requests_survive_the_round_trip() {
        let said = Presence {
            requests: vec![request("a", Ask::Invite, 1), request("b", Ask::Mute, 2)],
            ..Presence::default()
        };
        let json = serde_json::to_string(&said).unwrap();
        assert!(json.contains(r#""kind":"invite""#), "{json}");
        let heard: Presence = serde_json::from_str(&json).unwrap();
        assert_eq!(heard, said);
    }

    #[test]
    fn no_requests_are_said_without_a_requests_key() {
        let json = serde_json::to_string(&Presence::default()).unwrap();
        assert!(!json.contains("requests"), "{json}");
    }

    #[test]
    fn an_unknown_kind_does_not_lose_the_document() {
        let json = r#"{"state":"here","mic":false,"room":"Kitchen",
            "requests":[{"to":"x","kind":"wave","nonce":1},{"to":"y","kind":"mute","nonce":2}]}"#;
        let heard: Presence = serde_json::from_str(json).unwrap();
        assert_eq!(heard.requests[0].kind, Ask::Unknown);
        assert_eq!(heard.requests[1], request("y", Ask::Mute, 2));
        assert!(!heard.mic);
        assert_eq!(heard.room.as_deref(), Some("Kitchen"));
    }

    #[test]
    fn only_requests_to_us_are_ours() {
        assert_eq!(asked_of("me", &[request("you", Ask::Mute, 1), request("you", Ask::Invite, 2)]), (None, false));
        assert_eq!(asked_of("me", &[request("me", Ask::Mute, 7)]), (Some(7), false));
        assert_eq!(asked_of("me", &[request("me", Ask::Invite, 8)]), (None, true));
        assert_eq!(asked_of("me", &[request("me", Ask::Unknown, 9)]), (None, false));
    }

    #[test]
    fn a_mute_is_honoured_once_per_nonce() {
        let seen = std::sync::Mutex::default();
        let document = [request("me", Ask::Mute, 5)];
        let mut mutes = 0;
        for _ in 0..2 {
            if let (Some(nonce), _) = asked_of("me", &document) {
                mutes += honour(&seen, "them", nonce) as u32;
            }
        }
        assert_eq!(mutes, 1, "the same document read twice mutes once");
        assert!(honour(&seen, "them", 6), "a new nonce mutes again");
        assert!(honour(&seen, "other", 5), "the nonce is per asker");
    }

    #[test]
    fn an_older_peer_without_the_flags_reads_as_on() {
        let old: Presence = serde_json::from_str(r#"{"state":"here"}"#).unwrap();
        assert_eq!(old, Presence::default());
        assert!(old.requests.is_empty());
        assert!(old.camera && old.mic);
        assert_eq!(old.room, None, "an older peer is in the main room");
    }

    #[test]
    fn a_blank_room_is_the_main_room() {
        for json in [r#"{"state":"here","room":""}"#, r#"{"state":"here","room":"  "}"#] {
            let heard: Presence = serde_json::from_str(json).unwrap();
            assert_eq!(subroom(heard.room), None, "{json}");
        }
        assert_eq!(subroom(Some("Room 1".into())).as_deref(), Some("Room 1"));
    }

    #[test]
    fn a_subroom_survives_the_round_trip() {
        let said = Presence {
            room: Some("Room 2".to_string()),
            ..Presence::default()
        };
        let heard: Presence = serde_json::from_str(&serde_json::to_string(&said).unwrap()).unwrap();
        assert_eq!(heard.room.as_deref(), Some("Room 2"));
        assert_eq!(heard, said);
    }

    #[test]
    fn sharing_survives_the_round_trip() {
        let said = Presence {
            sharing: true,
            ..Presence::default()
        };
        let json = serde_json::to_string(&said).unwrap();
        assert!(json.contains(r#""sharing":true"#), "{json}");
        let heard: Presence = serde_json::from_str(&json).unwrap();
        assert_eq!(heard, said);
    }

    #[test]
    fn not_sharing_is_said_without_a_sharing_key() {
        // So an older peer receives exactly the bytes it did before shares.
        let json = serde_json::to_string(&Presence::default()).unwrap();
        assert!(!json.contains("sharing"), "{json}");
        let old: Presence = serde_json::from_str(r#"{"state":"here"}"#).unwrap();
        assert!(!old.sharing);
    }

    #[test]
    fn the_main_room_is_said_without_a_room_key() {
        // So an older peer receives exactly the bytes it did before subrooms.
        let json = serde_json::to_string(&Presence::default()).unwrap();
        assert!(!json.contains("room"), "{json}");
    }

    #[test]
    fn the_flags_survive_the_round_trip() {
        let said = Presence {
            state: State::Here,
            camera: false,
            mic: true,
            clock: Some(Clock {
                wall_us: 1_700_000_000_000_000,
                media_us: 42,
            }),
            room: None,
            requests: Vec::new(),
            sharing: false,
            name: Some("Ada".to_string()),
            link: None,
            seen: Default::default(),
            host: None,
            moved: None,
        };
        let heard: Presence = serde_json::from_str(&serde_json::to_string(&said).unwrap()).unwrap();
        assert_eq!(heard, said);
    }

    #[test]
    fn a_name_survives_the_round_trip() {
        let said = Presence {
            name: Some("Ada \u{1F600}".to_string()),
            ..Presence::default()
        };
        let json = serde_json::to_string(&said).unwrap();
        assert!(json.contains(r#""name":"Ada"#), "{json}");
        let heard: Presence = serde_json::from_str(&json).unwrap();
        assert_eq!(heard, said);
    }

    #[test]
    fn no_name_is_said_without_a_name_key() {
        // So an older peer receives exactly the bytes it did before names.
        let json = serde_json::to_string(&Presence::default()).unwrap();
        assert!(!json.contains("name"), "{json}");
    }

    #[test]
    fn an_older_peer_has_no_name() {
        let old: Presence = serde_json::from_str(r#"{"state":"here","mic":false,"room":"Attic"}"#).unwrap();
        assert_eq!(old.name, None);
        assert!(!old.mic);
        assert_eq!(old.room.as_deref(), Some("Attic"));
    }

    /// `Presence` as a build from before names declared it, field for field.
    #[derive(Debug, serde::Deserialize)]
    struct Unnamed {
        state: State,
        #[serde(default = "super::on")]
        camera: bool,
        #[serde(default = "super::on")]
        mic: bool,
        #[serde(default)]
        clock: Option<Clock>,
        #[serde(default)]
        room: Option<String>,
        #[serde(default)]
        requests: Vec<Request>,
        #[serde(default)]
        sharing: bool,
    }

    #[test]
    fn an_older_peer_reads_a_named_document() {
        // What reaches a build without names: the field is ignored, and the
        // rest of the document still counts.
        let said = Presence {
            camera: false,
            mic: false,
            clock: Some(Clock { wall_us: 1, media_us: 2 }),
            room: Some("Attic".to_string()),
            requests: vec![request("x", Ask::Mute, 3)],
            sharing: true,
            name: Some("Ada".to_string()),
            ..Presence::default()
        };
        let json = serde_json::to_string(&said).unwrap();
        let heard: Unnamed = serde_json::from_str(&json).unwrap();
        assert_eq!(heard.state, State::Here);
        assert!(!heard.camera && !heard.mic && heard.sharing);
        assert_eq!(heard.clock, said.clock);
        assert_eq!(heard.room.as_deref(), Some("Attic"));
        assert_eq!(heard.requests, said.requests);
    }

    /// `Presence` as the build before connection reports declared it, field
    /// for field, both ways.
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct Previous {
        state: State,
        #[serde(default = "super::on")]
        camera: bool,
        #[serde(default = "super::on")]
        mic: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clock: Option<Clock>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requests: Vec<Request>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        sharing: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    }

    fn id(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    /// Our own entry with a report standing, as the reporter leaves it.
    fn reporting(on: bool) -> crate::net::PeerStats {
        let local = crate::net::PeerStats::default();
        local.set_name(Some("Ada".into()));
        local.set_mic(false);
        local.set_link(Some(Link { q: Quality::Held, est: Some(2400), rtt: Some(40), fps: Some(30), ..Link::default() }));
        let mut seen = std::collections::BTreeMap::new();
        seen.insert(id('b'), Seen { q: Quality::Good, fps: Some(30), ..Seen::default() });
        local.set_seen(seen);
        local.set_connection_report(on);
        local
    }

    #[test]
    fn switched_off_the_document_is_the_old_shape_byte_for_byte() {
        let local = reporting(false);
        let ours = serde_json::to_string(&Presence::here(&local)).unwrap();
        let old = serde_json::to_string(&Previous {
            state: State::Here,
            camera: true,
            mic: false,
            clock: None,
            room: None,
            requests: Vec::new(),
            sharing: false,
            name: Some("Ada".into()),
        })
        .unwrap();
        assert_eq!(ours, old);
        assert!(!ours.contains("link") && !ours.contains("seen"), "{ours}");
    }

    #[test]
    fn switched_on_the_report_is_said_and_survives_the_round_trip() {
        let said = Presence::here(&reporting(true));
        let json = serde_json::to_string(&said).unwrap();
        assert!(json.contains(r#""link":{"q":"held","est":2400,"rtt":40,"fps":30}"#), "{json}");
        assert!(json.contains(&format!(r#""seen":{{"{}":{{"q":"good","fps":30}}}}"#, id('b'))), "{json}");
        let heard: Presence = serde_json::from_str(&json).unwrap();
        assert_eq!(heard, said);
    }

    #[test]
    fn an_older_peer_is_not_reporting() {
        let old: Presence = serde_json::from_str(r#"{"state":"here","name":"Ada"}"#).unwrap();
        assert_eq!(old.link, None);
        assert!(old.seen.is_empty());
    }

    #[test]
    fn an_older_peer_reads_a_reporting_document() {
        let json = serde_json::to_string(&Presence::here(&reporting(true))).unwrap();
        let heard: Previous = serde_json::from_str(&json).unwrap();
        assert_eq!(heard.name.as_deref(), Some("Ada"));
        assert!(!heard.mic && heard.camera);
    }

    #[test]
    fn a_malformed_report_costs_only_itself() {
        let json = format!(
            r#"{{"state":"here","mic":false,"name":"Ada",
                "link":{{"q":"good","est":"lots"}},
                "seen":{{"{a}":{{"fps":"x"}},"{c}":{{"q":"poor","fps":12,"later":true}},"nope":{{}},"{upper}":{{}}}}}}"#,
            a = id('a'),
            c = id('c'),
            upper = "F".repeat(64),
        );
        let heard: Presence = serde_json::from_str(&json).unwrap();
        assert!(!heard.mic);
        assert_eq!(heard.name.as_deref(), Some("Ada"));
        assert_eq!(heard.link, None, "a bad figure is not reporting");
        assert_eq!(heard.seen.keys().cloned().collect::<Vec<_>>(), vec![id('c')]);
        assert_eq!(heard.seen[&id('c')].q, Quality::Poor);
        // Wrong shapes altogether.
        let heard: Presence = serde_json::from_str(r#"{"state":"here","link":5,"seen":[1,2],"camera":false}"#).unwrap();
        assert_eq!((heard.link, heard.seen.len(), heard.camera), (None, 0, false));
        let heard: Presence = serde_json::from_str(r#"{"state":"here","link":null}"#).unwrap();
        assert_eq!(heard.link, None);
    }

    #[test]
    fn a_report_from_a_later_build_reads_what_it_can() {
        let heard: Presence =
            serde_json::from_str(r#"{"state":"here","link":{"q":"superb","cam":"warp","path":"satellite","rtt":44,"jitter":3}}"#)
                .unwrap();
        let link = heard.link.unwrap();
        assert_eq!(link.q, Quality::Unknown);
        assert_eq!(link.cam, Some(quality::StreamState::Unknown));
        assert_eq!(link.path, None, "an unknown path is dropped by cleaning");
        assert_eq!(link.rtt, Some(40), "rounded on the way in");
    }

    #[test]
    fn a_report_is_cleaned_on_arrival_and_on_the_way_out() {
        let local = reporting(true);
        local.set_link(Some(Link { est: Some(2_345), rtt: Some(3), ..Link::default() }));
        let mut seen = std::collections::BTreeMap::new();
        for n in 0..30u32 {
            seen.insert(format!("{n:064x}"), Seen::default());
        }
        seen.insert("x".into(), Seen::default());
        local.set_seen(seen.clone());
        let said = Presence::here(&local);
        let link = said.link.clone().unwrap();
        assert_eq!((link.est, link.rtt), (Some(2_300), Some(10)));
        assert_eq!(said.seen.len(), quality::SEEN_MAX);
        // What `listen` does with a sender's own id in their map.
        let theirs = quality::clean_seen(said.seen.clone(), &format!("{:064x}", 0));
        assert!(!theirs.contains_key(&format!("{:064x}", 0)));
    }

    #[test]
    fn a_hostile_name_is_cleaned_on_arrival() {
        // What `listen` does with a name as it comes off the wire.
        let json = format!(r#"{{"state":"here","name":"  \u202eEvil\n\n{}  "}}"#, "x".repeat(100));
        let heard: Presence = serde_json::from_str(&json).unwrap();
        let shown = heard.name.as_deref().and_then(crate::name::clean).unwrap();
        assert!(shown.starts_with("Evil x"), "{shown}");
        assert_eq!(shown.chars().count(), crate::name::MAX);
        let blank: Presence = serde_json::from_str(r#"{"state":"here","name":" \t "}"#).unwrap();
        assert_eq!(blank.name.as_deref().and_then(crate::name::clean), None);
    }

    const DOOR: &str = "00112233445566778899aabbccddeeff";

    /// Our own entry, serving a room behind `DOOR` with hop 7.
    fn hosting(on: bool) -> crate::net::PeerStats {
        let local = reporting(false);
        if on {
            local.set_offer(Some(door::Offer { door: DOOR.to_string(), hop: 7 }));
        }
        local
    }

    #[test]
    fn not_hosting_the_document_is_the_old_shape_byte_for_byte() {
        let ours = serde_json::to_string(&Presence::here(&hosting(false))).unwrap();
        let old = serde_json::to_string(&Presence::here(&reporting(false))).unwrap();
        assert_eq!(ours, old);
        assert!(!ours.contains("host"), "{ours}");
    }

    #[test]
    fn hosting_is_said_and_survives_the_round_trip() {
        let said = Presence::here(&hosting(true));
        let json = serde_json::to_string(&said).unwrap();
        assert!(json.contains(&format!(r#""host":{{"door":"{DOOR}","hop":7}}"#)), "{json}");
        let heard: Presence = serde_json::from_str(&json).unwrap();
        assert_eq!(heard, said);
        assert!(!format!("{said:?}").contains(DOOR), "the door stays out of Debug");
    }

    #[test]
    fn an_older_peer_reads_a_hosting_document_and_is_not_hosting() {
        let json = serde_json::to_string(&Presence::here(&hosting(true))).unwrap();
        let heard: Previous = serde_json::from_str(&json).unwrap();
        assert_eq!(heard.name.as_deref(), Some("Ada"));
        let old: Presence = serde_json::from_str(r#"{"state":"here","name":"Ada"}"#).unwrap();
        assert_eq!(old.host, None);
    }

    #[test]
    fn a_malformed_host_costs_only_itself_and_a_bad_one_is_never_said() {
        let heard: Presence = serde_json::from_str(r#"{"state":"here","name":"Ada","host":{"door":"x","hop":7}}"#).unwrap();
        assert_eq!(heard.host, None);
        assert_eq!(heard.name.as_deref(), Some("Ada"));
        let local = reporting(false);
        local.set_offer(Some(door::Offer { door: "not a door".to_string(), hop: 7 }));
        assert_eq!(Presence::here(&local).host, None, "cleaned on the way out too");
    }

    /// Our entry following a move of the call to host `h`, with the switch
    /// as given.
    fn following(on: bool) -> crate::net::PeerStats {
        let local = reporting(false);
        local.set_move_calls(true);
        local.move_call(Some(id('c')), &id('a'));
        local.set_move_calls(on);
        local
    }

    #[test]
    fn a_move_is_said_only_while_we_follow_moves_and_survives_the_round_trip() {
        let off = serde_json::to_string(&Presence::here(&following(false))).unwrap();
        assert_eq!(off, serde_json::to_string(&Presence::here(&reporting(false))).unwrap(), "the old shape");
        assert!(!off.contains("moved"), "{off}");
        let said = Presence::here(&following(true));
        let json = serde_json::to_string(&said).unwrap();
        assert!(json.contains(&format!(r#""moved":{{"to":"{}","n":1,"by":"{}"}}"#, id('c'), id('a'))), "{json}");
        let heard: Presence = serde_json::from_str(&json).unwrap();
        assert_eq!(heard, said);
    }

    #[test]
    fn an_older_peer_reads_a_moved_document_and_we_read_theirs() {
        let json = serde_json::to_string(&Presence::here(&following(true))).unwrap();
        let heard: Previous = serde_json::from_str(&json).unwrap();
        assert_eq!(heard.name.as_deref(), Some("Ada"));
        let old: Presence = serde_json::from_str(r#"{"state":"here","name":"Ada"}"#).unwrap();
        assert_eq!(old.moved, None);
    }

    #[test]
    fn a_malformed_move_costs_only_itself() {
        for bad in [
            r#""moved":"relay""#.to_string(),
            r#""moved":{"to":"x","n":1,"by":"y"}"#.to_string(),
            format!(r#""moved":{{"n":-1,"by":"{}"}}"#, id('a')),
            format!(r#""moved":{{"n":9007199254740993,"by":"{}"}}"#, id('a')),
        ] {
            let heard: Presence = serde_json::from_str(&format!(r#"{{"state":"here","name":"Ada",{bad}}}"#)).unwrap();
            assert_eq!(heard.moved, None, "{bad}");
            assert_eq!(heard.name.as_deref(), Some("Ada"), "{bad}");
        }
    }

    #[test]
    fn a_heard_move_is_followed_only_with_the_switch_on() {
        let theirs = super::super::moves::CallOn { to: Some(id('c')), n: 3, by: id('b') };
        let local = reporting(false);
        followed(&local, Some(theirs.clone()));
        local.set_move_calls(true);
        assert_eq!(local.call_host(), None, "heard while off, so not taken in");
        followed(&local, Some(theirs));
        assert_eq!(local.call_host(), Some(id('c')));
        local.set_move_calls(false);
        assert_eq!(local.call_host(), None, "off puts us back in the usual order");
    }

    #[test]
    fn a_catalog_cut_off_by_its_transport_is_told_apart_from_one_that_does_not_read() {
        assert!(super::catalog_cut_off(&moq_mux::Error::Moq(moq_net::Error::Cancel)));
        assert!(super::catalog_cut_off(&moq_mux::Error::Json(moq_json::Error::Net(moq_net::Error::Dropped))));
        assert!(!super::catalog_cut_off(&moq_mux::Error::Json(moq_json::Error::Json("expected value".into()))));
        assert!(!super::catalog_cut_off(&moq_mux::Error::Json(moq_json::Error::MissingSnapshot)));
    }

    /// The wiring table: whether the origins are shared, and whether a join
    /// holds several sessions. A join with no hosts and hosting off is the
    /// one session over two origins it always was.
    #[test]
    fn only_a_hosting_join_or_a_hosted_room_shares_one_origin() {
        let url: url::Url = "https://relay.example.com/team".parse().unwrap();
        let peer: iroh::EndpointId = iroh::SecretKey::from_bytes(&[1; 32]).public();
        for host_rooms in [false, true] {
            assert!(shares_origin(&Peering::Host("team".into()), host_rooms));
            assert_eq!(shares_origin(&Peering::Join(url.clone()), host_rooms), host_rooms);
            for other in [Peering::Dial(peer), Peering::Listen, Peering::Loopback, Peering::Idle] {
                assert!(!shares_origin(&other, host_rooms), "{other:?}");
            }
        }
        let join = Peering::Join(url);
        assert!(!dials_several(&join, false, false), "no hosts, not hosting: as before");
        // The spare route lives only in the set, so it cannot touch a join
        // that never reaches it; and a host's join keeps none.
        assert!(super::sessions::keeps_spare(false, true), "on by default for a member of a room with hosts");
        assert!(!super::sessions::keeps_spare(true, true), "a hosting join offers everything everywhere already");
        assert!(dials_several(&join, false, true), "a room with hosts");
        assert!(dials_several(&join, true, false), "a room we host");
        assert!(dials_several(&join, true, true));
        for other in [Peering::Host("team".into()), Peering::Dial(peer), Peering::Listen, Peering::Loopback, Peering::Idle] {
            for (host_rooms, has_hosts) in [(false, false), (true, true)] {
                assert!(!dials_several(&other, host_rooms, has_hosts), "{other:?}");
            }
        }
    }

    /// With hosting off a join keeps two origins, as it always has: the
    /// inbound is a separate origin, with a hop of its own.
    #[tokio::test]
    async fn an_unshared_inbound_is_an_origin_of_its_own() {
        let outbound = moq_tokio::origin::spawn();
        assert_ne!(inbound_for(&outbound, false).hop(), outbound.hop());
        assert_eq!(inbound_for(&outbound, true).hop(), outbound.hop());
    }

    fn news() -> (RoomNews, futures_channel::mpsc::UnboundedReceiver<HostNews>) {
        let (to, from) = futures_channel::mpsc::unbounded();
        (RoomNews { room: "room".to_string(), to }, from)
    }

    fn offer(door: &str) -> Option<door::Offer> {
        Some(door::Offer { door: door.to_string(), hop: 7 })
    }

    /// What reaches the interface, as `(heard, door)` or `(stopped, "")`.
    fn drained(from: &mut futures_channel::mpsc::UnboundedReceiver<HostNews>) -> Vec<(&'static str, String)> {
        std::iter::from_fn(|| from.try_recv().ok())
            .map(|news| match news {
                HostNews::Heard { door, .. } => ("heard", door),
                HostNews::Stopped { .. } => ("stopped", String::new()),
            })
            .collect()
    }

    #[test]
    fn a_host_is_reported_on_a_change_only_and_forgotten_only_when_it_stops() {
        let (news, mut from) = news();
        let other = "ffeeddccbbaa99887766554433221100";
        let mut offered = None;
        let mut say = |state, host| heard_hosting(&id('h'), state, host, &mut offered, Some(&news));
        say(State::Here, None);
        say(State::Here, offer(DOOR));
        say(State::Here, offer(DOOR));
        say(State::Here, offer(other));
        say(State::Leaving, None);
        say(State::Here, None);
        say(State::Here, None);
        say(State::Here, offer("junk"));
        assert_eq!(
            drained(&mut from),
            [("heard", DOOR.to_string()), ("heard", other.to_string()), ("stopped", String::new())]
        );
    }
}

#[cfg(test)]
mod room_tests {
    use super::{Refusals, TURNED_AWAY, explain, redacted, room_label};

    fn iroh() -> Refusals {
        Refusals::for_url(&format!("iroh://{}/team", "a".repeat(64)).parse().unwrap()).expect("iroh is judged")
    }

    #[test]
    fn only_an_iroh_session_is_judged_by_its_shape() {
        for other in ["http://localhost:4443/team", "https://relay.example.com/team", "moqt://localhost:1/team"] {
            assert!(Refusals::for_url(&other.parse().unwrap()).is_none(), "{other}");
        }
    }

    #[test]
    fn two_brief_sessions_running_are_a_refusal_and_one_that_lasted_resets_it() {
        let start = std::time::Instant::now();
        let at = |ms: u64| start + std::time::Duration::from_millis(ms);
        let mut refusals = iroh();
        refusals.connected(at(0));
        assert!(!refusals.dropped(at(200)), "one brief session is a blip");
        refusals.connected(at(1500));
        assert!(refusals.dropped(at(1700)), "two running are the door");

        let mut refusals = iroh();
        refusals.connected(at(0));
        assert!(!refusals.dropped(at(200)));
        refusals.connected(at(1000));
        assert!(!refusals.dropped(at(5000)), "a session that lasted is not a refusal");
        refusals.connected(at(6000));
        assert!(!refusals.dropped(at(6100)), "and it reset the count");
        assert!(!refusals.dropped(at(9000)), "a drop with no session is not counted");
        refusals.connected(at(9500));
        assert!(refusals.dropped(at(9600)));
    }

    #[test]
    fn a_token_never_reaches_the_log() {
        let url: url::Url = "https://relay.example.com:4443/test?jwt=secret.token.value".parse().unwrap();
        let shown = redacted(&url);
        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("relay.example.com:4443/test"), "{shown}");
    }

    #[test]
    fn a_door_never_reaches_the_log_or_the_status_line() {
        let door = "00112233445566778899aabbccddeeff";
        let url: url::Url = format!("iroh://{}/team?door={door}&x=1", "a".repeat(64)).parse().unwrap();
        let shown = redacted(&url);
        assert!(!shown.contains(door), "{shown}");
        assert!(shown.contains("door=redacted") && shown.contains("x=1"), "{shown}");
        let status = crate::net::RemoteStatus::InRoom { room: room_label(&url), via: None }.to_string();
        assert!(!status.contains(door), "{status}");
    }

    #[test]
    fn an_invites_hosts_never_reach_the_log() {
        const DOOR: &str = "00112233445566778899aabbccddeeff";
        let url: url::Url = format!("https://relay.example.com/team?jwt=t&host={}.{DOOR}", "a".repeat(64)).parse().unwrap();
        let shown = redacted(&url);
        assert!(!shown.contains(DOOR), "{shown}");
        assert!(shown.contains("host=redacted"), "{shown}");
    }

    #[test]
    fn a_url_without_a_token_is_left_alone() {
        let url: url::Url = "http://localhost:4443/test".parse().unwrap();
        assert_eq!(redacted(&url), "http://localhost:4443/test");
    }

    #[test]
    fn a_door_that_turned_us_away_over_iroh_is_said_as_refused() {
        assert!(explain(TURNED_AWAY.to_string()).starts_with("the room refused this link"));
    }

    #[test]
    fn a_refused_token_is_said_in_words() {
        let raw = "reconnect timed out after 30s: transport: webtransport error: closed: code=6 reason=unauthorized";
        assert!(explain(raw.to_string()).starts_with("the room refused this link"));
        assert!(explain("unauthorized".to_string()).starts_with("the room refused this link"));
        assert_eq!(explain("something else".to_string()), "something else");
    }

    fn label(url: &str) -> String {
        room_label(&url.parse().unwrap())
    }

    #[test]
    fn a_room_is_its_path_on_its_host() {
        assert_eq!(label("http://localhost:4443/test"), "room test on localhost");
        assert_eq!(label("https://relay.example.com/a/b/"), "room a/b on relay.example.com");
    }

    #[test]
    fn no_path_is_the_relay_itself() {
        assert_eq!(label("http://localhost:4443"), "the relay at localhost");
    }

    #[test]
    fn an_iroh_relay_is_shortened() {
        let key = "a".repeat(64);
        assert_eq!(label(&format!("iroh://{key}/test")), "room test on aaaaaaaaaaaa");
    }
}

/// The accept loop over a real local socket: raw QUIC to 127.0.0.1, which
/// carries the request in the SETUP as an iroh dial does. No camera, no
/// microphone, no iroh.
#[cfg(test)]
mod served_tests {
    use super::*;

    /// Long for a local socket; a run that needs it has failed anyway.
    const WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

    /// One shared origin, served on a local port through [`accept`].
    async fn host(door: Option<door::Hosting>) -> (Abort, u16) {
        let shared = moq_tokio::origin::spawn();
        let mut config = moq_tokio::server::Config::default();
        config.listen.bind = Some(moq_tokio::listen::Bind::Addr(([127, 0, 0, 1], 0).into()));
        config.listen.tls.generate = vec!["localhost".to_string()];
        config.publisher = Some(shared.consume());
        config.subscriber = Some(shared);
        let server = config.init().unwrap().bind().await.unwrap();
        let port = server.local_addr().unwrap().port();
        let estimates = adapt::Estimates::new(moq_net::bandwidth::Producer::new());
        let (task, _) = accept(server, door, Some(estimates), Arc::new(Metrics::default()), None, None, None);
        (task, port)
    }

    /// Someone who dials `target` once and publishes as `id`, with their own
    /// two origins as the app has.
    struct Guest {
        connection: moq_tokio::Connection,
        inbound: moq_net::origin::Producer,
        _outbound: moq_net::origin::Producer,
        _camera: moq_net::broadcast::Producer,
    }

    fn guest(port: u16, target: &str, id: &str) -> Guest {
        dial(port, target, id, false)
    }

    /// `redial` as the app has it, which is what decides how long a refusal
    /// takes to be final.
    fn dial(port: u16, target: &str, id: &str, redial: bool) -> Guest {
        let outbound = moq_tokio::origin::spawn();
        let camera = outbound.create_broadcast(camera_path(id)).unwrap();
        camera.announce(moq_net::origin::Route::default()).unwrap();
        let inbound = moq_tokio::origin::spawn();
        let mut config = moq_tokio::client::Config::default();
        // On the loopback address as the servers are: a dual-stack port can
        // share its number with another test's server, which then takes
        // this client's answers (see `sessions`' socket tests).
        config.connect.bind = Some(std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)));
        config.connect.tls.insecure = Some(true);
        let url: url::Url = format!("moqt://localhost:{port}{target}").parse().unwrap();
        let connection = config
            .init()
            .unwrap()
            .with_publisher(outbound.consume())
            .with_subscriber(inbound.clone())
            .with_reconnect(redial)
            .connect(url);
        Guest {
            connection,
            inbound,
            _outbound: outbound,
            _camera: camera,
        }
    }

    /// Waits for `id`'s camera to be announced to `guest`.
    async fn sees(guest: &Guest, id: &str) {
        let mut announced = guest.inbound.consume().announced();
        let wanted = camera_path(id);
        tokio::time::timeout(WITHIN, async {
            while let Some(update) = announced.next().await {
                if update.prefix.as_str() == wanted && update.kind.is_active() {
                    return;
                }
            }
            panic!("the announcements ended before {wanted}");
        })
        .await
        .unwrap_or_else(|_| panic!("{wanted} was never announced"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn guests_see_each_other_through_one_shared_origin() {
        let (_accepting, port) = host(None).await;
        let a = guest(port, "/room", "a");
        let b = guest(port, "/room", "b");
        sees(&a, "b").await;
        sees(&b, "a").await;
    }

    fn team() -> door::Hosting {
        door::Hosting::open("team".to_string()).unwrap()
    }

    /// The path and query of the invite, which is what a guest dials with.
    fn invited(hosting: &door::Hosting) -> String {
        let invite: url::Url = hosting.invite(&"a".repeat(64)).parse().unwrap();
        format!("{}?{}", invite.path(), invite.query().unwrap())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invited_guests_see_each_other_through_the_door() {
        let hosting = team();
        let (_accepting, port) = host(Some(hosting.clone())).await;
        let a = guest(port, &invited(&hosting), "a");
        let b = guest(port, &invited(&hosting), "b");
        sees(&a, "b").await;
        sees(&b, "a").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_door_turns_away_a_wrong_door_a_missing_one_and_another_room() {
        let hosting = team();
        let (_accepting, port) = host(Some(hosting.clone())).await;
        let room_only = "/team".to_string();
        let wrong = format!("/team?door={}", "0".repeat(32));
        let elsewhere = invited(&hosting).replace("/team?", "/other?");
        // Redialling, as the app does: a refusal the client took for a drop
        // would keep it trying for the whole retry budget and time out here.
        // The session does come up first -- the door is checked after SETUP
        // -- so it is the ending that says refused.
        for target in [room_only, wrong, elsewhere] {
            let refused = dial(port, &target, "x", true);
            let ended = tokio::time::timeout(WITHIN, refused.connection.closed()).await;
            let Ok(Err(error)) = ended else { panic!("{target} was not refused in time") };
            assert_eq!(error.connect_error(), Some(moq_tokio::ConnectError::Unauthorized), "{target}: {error}");
            assert!(explain(format!("{error:#}")).starts_with("the room refused"), "{error:#}");
        }
    }

    /// A server on a local port whose sessions publish `publisher` and land
    /// in `subscriber`, through [`accept`] with no door.
    async fn serving(publisher: moq_net::origin::Consumer, subscriber: moq_net::origin::Producer) -> (Abort, u16) {
        let mut config = moq_tokio::server::Config::default();
        config.listen.bind = Some(moq_tokio::listen::Bind::Addr(([127, 0, 0, 1], 0).into()));
        config.listen.tls.generate = vec!["localhost".to_string()];
        config.publisher = Some(publisher);
        config.subscriber = Some(subscriber);
        let server = config.init().unwrap().bind().await.unwrap();
        let port = server.local_addr().unwrap().port();
        let estimates = adapt::Estimates::new(moq_net::bandwidth::Producer::new());
        let (task, _) = accept(server, None, Some(estimates), Arc::new(Metrics::default()), None, None, None);
        (task, port)
    }

    /// A member hosting while in a relay room, as the app wires it: one
    /// origin for its session to the relay, for its guests and for its own
    /// camera.
    struct Bridge {
        _connection: moq_tokio::Connection,
        _accepting: Abort,
        port: u16,
        _camera: moq_net::broadcast::Producer,
    }

    async fn bridge(relay: u16, id: &str) -> Bridge {
        let shared = moq_tokio::origin::spawn();
        let camera = shared.create_broadcast(camera_path(id)).unwrap();
        camera.announce(moq_net::origin::Route::default()).unwrap();
        let mut config = moq_tokio::client::Config::default();
        // On the loopback address as the servers are: a dual-stack port can
        // share its number with another test's server, which then takes
        // this client's answers (see `sessions`' socket tests).
        config.connect.bind = Some(std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)));
        config.connect.tls.insecure = Some(true);
        let url: url::Url = format!("moqt://localhost:{relay}/room").parse().unwrap();
        let connection = config
            .init()
            .unwrap()
            .with_publisher(shared.consume())
            .with_subscriber(shared.clone())
            .with_reconnect(false)
            .connect(url);
        let (accepting, port) = serving(shared.consume(), shared).await;
        Bridge {
            _connection: connection,
            _accepting: accepting,
            port,
            _camera: camera,
        }
    }

    /// Waits for `path` to become announced (`active`) or retracted on a
    /// cursor. The cursor is taken before the change it waits for, so a
    /// retraction that has already happened is still seen: a fresh cursor
    /// says nothing about a path that is gone.
    async fn becomes(announced: &mut moq_net::announce::Consumer, path: &str, active: bool) {
        tokio::time::timeout(WITHIN, async {
            while let Some(update) = announced.next().await {
                if update.prefix.as_str() == path && update.kind.is_active() == active {
                    return;
                }
            }
            panic!("the announcements ended before {path}");
        })
        .await
        .unwrap_or_else(|_| panic!("{path} never became {}", if active { "announced" } else { "retracted" }));
    }

    /// The bridge: a guest of the host and a member on the relay see each
    /// other, through the host's one origin. And when the member leaves, they
    /// are gone everywhere: a route the host had offered back to the relay
    /// would keep them announced in a loop that never ends.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_hosting_member_bridges_its_guests_and_the_relay_room() {
        let relay = moq_tokio::origin::spawn();
        let (_relaying, relay_port) = serving(relay.consume(), relay.clone()).await;
        let host = bridge(relay_port, "h").await;
        let guest_of_host = guest(host.port, "/room", "g");
        let member = guest(relay_port, "/room", "c");
        sees(&guest_of_host, "c").await;
        sees(&guest_of_host, "h").await;
        sees(&member, "g").await;
        sees(&member, "h").await;

        let route = relay.consume().routed(camera_path("c")).await.expect("the relay has c");
        assert_eq!(route.hops.len(), 1, "c straight from c, not through the host: {:?}", route.hops);

        let mut at_relay = relay.consume().announced();
        let mut at_guest = guest_of_host.inbound.consume().announced();
        becomes(&mut at_relay, &camera_path("c"), true).await;
        becomes(&mut at_guest, &camera_path("c"), true).await;
        drop(member);
        becomes(&mut at_relay, &camera_path("c"), false).await;
        becomes(&mut at_guest, &camera_path("c"), false).await;
    }

    /// What the relay is offered by a hosting member: its own camera and its
    /// guests', and **nothing the relay itself sent**. The relay's session
    /// with the host lands in an origin of its own here, so everything in it
    /// is exactly what the host offered; the relay's one route for a member
    /// is then that member's own.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_relay_is_offered_the_host_and_its_guests_and_no_reflection() {
        let relay = moq_tokio::origin::spawn();
        let (_members, members_port) = serving(relay.consume(), relay.clone()).await;
        let from_host = moq_tokio::origin::spawn();
        let (_for_host, host_port) = serving(relay.consume(), from_host.clone()).await;
        let member = guest(members_port, "/room", "c");
        let host = bridge(host_port, "h").await;
        let guest_of_host = guest(host.port, "/room", "g");
        // The host has the member, so it could reflect them if it were going to.
        sees(&guest_of_host, "c").await;
        let mut offered_by_host = from_host.consume().announced();
        becomes(&mut offered_by_host, &camera_path("h"), true).await;
        let mut offered_by_host = from_host.consume().announced();
        becomes(&mut offered_by_host, &camera_path("g"), true).await;

        let mut offered = from_host.consume().announced();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        while let Some(update) = offered.try_next() {
            assert_ne!(update.prefix.as_str(), camera_path("c"), "the host offered the relay its own member back");
        }
        let route = relay.consume().routed(camera_path("c")).await.expect("the relay has c");
        assert_eq!(route.hops.len(), 1, "{:?}", route.hops);
        drop(member);
    }

    /// Weaker than it looks: over this socket, dropping the listener closes its
    /// QUIC connections by itself, so loose session tasks would pass too. Over
    /// iroh the endpoint outlives the server (read in moq-tokio's `Listener`,
    /// not run), and there the sessions' tasks ending is what ends them.
    #[tokio::test(flavor = "multi_thread")]
    async fn sessions_end_with_the_accept_loop() {
        let (accepting, port) = host(None).await;
        let guest = guest(port, "/room", "a");
        let connection = tokio::time::timeout(WITHIN, guest.connection.clone().established())
            .await
            .expect("never connected")
            .expect("refused");
        drop(accepting);
        let ended = tokio::time::timeout(WITHIN, connection.closed()).await;
        assert!(ended.is_ok(), "the session outlived the call");
    }

    /// Writes 1000 bytes on `broadcast`'s video track every 20 ms (400 kbps)
    /// for as long as the returned task lives.
    fn stream(broadcast: &moq_net::broadcast::Producer) -> Abort {
        let mut track = broadcast.create_track(TRACK, None).unwrap();
        Abort(tokio::spawn(async move {
            for n in 0u64.. {
                if track.write_frame(moq_net::Timestamp::from_millis(n * 20).unwrap(), [0u8; 1000].as_slice()).is_err() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }))
    }

    /// Reads every group of `path`'s video track as it arrives.
    async fn pull(inbound: &moq_net::origin::Producer, path: String) -> Abort {
        let broadcast = tokio::time::timeout(WITHIN, inbound.consume().routed_broadcast(path))
            .await
            .expect("announced in time")
            .expect("a broadcast");
        Abort(tokio::spawn(async move {
            let Ok(track) = broadcast.track(TRACK) else { return };
            let Ok(mut subscriber) = track.subscribe(None).await else { return };
            while let Ok(Some(mut group)) = subscriber.recv_group().await {
                while let Ok(Some(_)) = group.read_frame().await {}
            }
        }))
    }

    /// One second of the host's counters, read the way the meter reads them.
    async fn second_of(forwarded: &Forwarded) -> crate::rates::Hosting {
        let start = tokio::time::Instant::now();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        forwarded.sample(start.elapsed(), None).expect("serving")
    }

    /// A host behind its door, as the app wires one: one origin for its own
    /// camera and its guests, each guest counted. Its own camera streams
    /// throughout; a guest's media is what it forwards.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_counts_what_it_forwards_apart_from_its_own() {
        let me = "h".repeat(64);
        let hosting = team();
        let shared = moq_tokio::origin::spawn();
        let own = shared.create_broadcast(camera_path(&me)).unwrap();
        let _own_stream = stream(&own);
        own.announce(moq_net::origin::Route::default()).unwrap();
        let mut config = moq_tokio::server::Config::default();
        config.listen.bind = Some(moq_tokio::listen::Bind::Addr(([127, 0, 0, 1], 0).into()));
        config.listen.tls.generate = vec!["localhost".to_string()];
        config.publisher = Some(shared.consume());
        config.subscriber = Some(shared);
        let server = config.init().unwrap().bind().await.unwrap();
        let port = server.local_addr().unwrap().port();
        let forwarded = Forwarded::new(me.clone());
        forwarded.set_serving(true);
        let estimates = adapt::Estimates::new(moq_net::bandwidth::Producer::new());
        let (_accepting, _) = accept(
            server,
            Some(hosting.clone()),
            Some(estimates),
            Arc::new(Metrics::default()),
            Some(forwarded.clone()),
            None,
            None,
        );

        let a = guest(port, &invited(&hosting), "a");
        let _a_stream = stream(&a._camera);
        let b = guest(port, &invited(&hosting), "b");
        sees(&b, "a").await;
        sees(&b, &me).await;

        // Connected, everything announced, nobody pulling: nothing but
        // control traffic, which is not payload.
        second_of(&forwarded).await;
        let idle = second_of(&forwarded).await;
        assert_eq!(idle, crate::rates::Hosting { forwarded: 0, own: 0, guests: 2, pulling: 0 });

        // b pulls a's video, which we forward, and ours, which is our own.
        let _from_a = pull(&b.inbound, camera_path("a")).await;
        let _from_us = pull(&b.inbound, camera_path(&me)).await;
        second_of(&forwarded).await;
        let busy = second_of(&forwarded).await;
        assert!((300..=500).contains(&busy.forwarded), "{busy:?}");
        assert!((300..=500).contains(&busy.own), "{busy:?}");
        assert_eq!((busy.guests, busy.pulling), (2, 1), "{busy:?}");
    }
}

#[cfg(test)]
mod encode_tests {
    use super::*;

    #[test]
    fn only_the_encoders_own_failure_demotes_it() {
        let encoder = Err(Stopped::Encoder("encode failed".into()));
        let publish = Err(Stopped::Publish("publish failed".into()));
        assert!(demotes(&encoder, "videotoolbox"));
        assert!(!demotes(&publish, "videotoolbox"), "a publish error is not the encoder's");
        assert!(!demotes(&encoder, SOFTWARE_ENCODER), "software has nowhere to go");
        assert!(!demotes(&Ok(()), "videotoolbox"));
    }

    /// The camera off and on under one producer, as the encode loop keeps
    /// it, with the capture's clock starting again: the track refuses the
    /// restart as it is, and takes it on the published stamps.
    #[tokio::test]
    async fn a_restarted_capture_is_refused_as_it_is_and_taken_rising() {
        let (width, height) = (160, 90);
        let origin = moq_tokio::origin::spawn();
        let mut broadcast = origin.create_broadcast("restart").unwrap();
        let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
        let mut config = moq_video::encode::Config::new(width, height, framerate());
        config.kind = moq_video::encode::Kind::Software;
        let rendition = config.probe().await.unwrap();
        let track = broadcast.create_track(TRACK, catalog.track_info(VIDEO_PRIORITY)).unwrap();
        let mut producer = moq_video::encode::Producer::with_track(track, catalog.clone(), rendition).unwrap();
        let mut encoder = moq_video::encode::Encoder::new(&config).unwrap();
        let mut encode = |n: u64, micros: u64| {
            let frame = crate::camera::pattern::frame(width, height, n, std::time::Duration::from_micros(micros));
            let surface = frame.to_i420_surface(1).unwrap();
            let timestamp = moq_net::Timestamp::from_micros(micros).unwrap();
            encoder.cut().unwrap();
            encoder.encode(&moq_video::Frame::new(surface, timestamp)).unwrap()
        };

        let mut rising = super::super::stamps::Rising::default();
        for n in 0..5u64 {
            let encoded = encode(n, rising.stamp(1_000_000 + n * 33_333));
            producer.publish(&encoded).unwrap();
        }
        // The capture's clock starts again near zero.
        let refused = producer.publish(&encode(5, 0)).unwrap_err();
        assert_eq!(super::super::stamps::refusal(&refused), Some(super::super::stamps::Refusal::Rewind));
        for n in 0..5u64 {
            let encoded = encode(10 + n, rising.stamp(n * 33_333));
            producer.publish(&encoded).unwrap_or_else(|error| panic!("frame {n} after the restart: {error:#}"));
        }
    }
}
