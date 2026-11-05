//! A call: one peer directly, or a room of several on a relay.
//!
//! The shape is the loopback's with the pipe replaced by a real connection.
//! `--listen` runs a `moq_native::Server` and `--dial` runs a
//! `moq_native::Client` against it; after the handshake the two sides are
//! identical, and so is everything below the transport.
//!
//! **Two modes, side by side.** A 1:1 call is peer to peer with no relay: the
//! listening side is its own tiny server, two processes, one key typed into
//! the other. `--relay` instead joins a **room** on a relay, which everyone in
//! the room is a client of. Each participant publishes once and the relay fans
//! out, so the uplink cost does not grow with the room. To this module a relay
//! is just a server we dial, which is why `--dial` and `--relay` share
//! [`connect`]. A participant hosting the relay itself is ticket 22.
//!
//! Every arm converges on [`Peer`], so the claim above is structural rather
//! than a description: anything per-connection is reached the same way
//! whichever side established it.
//!
//! Each side keeps **two origins** -- `outbound` for what we publish, `inbound`
//! for what arrives. They are separate `Origin`s, so both peers can publish at
//! the same well-known path without colliding, and neither needs to know the
//! other's id to subscribe. Per-participant paths are ticket 12's job.
//!
//! What we hand the encoder is the same [`FrameFeed`] the loopback uses: the
//! pump's fan-out tap. Nothing in `camera/` knows whether a peer is involved.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_channel::mpsc::UnboundedSender;
use futures_util::StreamExt as _;
use moq_mux::catalog::Stream as _;

use super::adapt;
use super::audio;
use super::runtime::runtime;
use super::status::RemoteStatus;
use super::ParticipantEvent;
use crate::args::Peering;
use super::Metrics;
use super::metrics::{FpsMeter, PeerStats};
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
/// `ConnectionStats` for rtt and loss would be the next one.
struct Peer {
    /// Resolves when the connection ends, with why.
    closed: Pin<Box<dyn Future<Output = String> + Send>>,
    /// The send-bitrate estimate, when this side can reach one.
    bandwidth: Option<moq_net::bandwidth::Consumer>,
    /// Connection figures, when this side has a handle that outlives a single
    /// session. The listening side reports its own from inside the accept loop
    /// instead, since its session is what it has.
    stats: Option<moq_native::ConnectionStatsReader>,
}

/// Everything [`connect`] needs, named rather than positional.
struct Connect<'a> {
    config: &'a moq_native::ClientConfig,
    endpoint: &'a moq_native::iroh::Endpoint,
    outbound: &'a moq_net::origin::Producer,
    inbound: moq_net::origin::Producer,
    /// A peer's `iroh://<id>`, or a relay's room URL.
    url: url::Url,
    /// Who or what we are connected to, for the log.
    label: String,
    /// The room's label when this is a room on a relay, `None` for one peer.
    room: Option<String>,
    status: UnboundedSender<RemoteStatus>,
    keyframe: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    let mut session = client
        .with_publisher(outbound)
        .with_subscriber(inbound)
        .reconnect(url);
    // Taken before the session moves into the future below. It survives a
    // reconnect, so the estimate follows the link across one rather than being
    // restarted. Toward a relay this is our own uplink to it, which is the
    // right thing to adapt to: everyone downstream is the relay's problem.
    let bandwidth = Some(session.send_bandwidth());
    let stats = Some(session.stats());
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
                    Ok(moq_native::Status::Connected) => {
                        if connected {
                            tracing::info!("reconnected to {label}");
                            keyframe.store(true, std::sync::atomic::Ordering::Relaxed);
                        } else if room.is_some() {
                            tracing::info!("joined {label}");
                        }
                        let next = match &room {
                            Some(room) => Some(RemoteStatus::InRoom { room: room.clone() }),
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
                    Ok(moq_native::Status::Disconnected) => {
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

/// The URL with any token's value blanked, for logs and labels.
///
/// **A room URL can carry its invitation** (`?jwt=…`, the relay's token), and
/// anyone holding it can join that room until it expires, so it must never be
/// written anywhere a log or a screenshot might travel. Only our own lines are
/// covered: moq-native logs the full URL at `info` itself, which the default
/// `RUST_LOG=vdr=…` filter happens to hide.
fn redacted(url: &url::Url) -> String {
    let mut clean = url.clone();
    if url.query_pairs().any(|(key, _)| key == "jwt") {
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .map(|(key, value)| {
                // Plain ASCII: a URL would percent-encode an ellipsis.
                let value = if key == "jwt" { "redacted".to_string() } else { value.into_owned() };
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
/// reason=unauthorized`), which moq-native retries like any drop until its
/// budget runs out -- it does not tell a refusal from a flaky link until then.
/// What reaches us at the end is transport text; this says what to do.
fn explain(error: String) -> String {
    if error.contains("reason=unauthorized") {
        "the relay refused this room's token (missing, expired, or for another room)".to_string()
    } else {
        error
    }
}

/// How a room reads in the status line: its path, and the relay it is on.
///
/// The path *is* the room -- the relay roots each session at the path it
/// connected with -- so two URLs differing only in host are two rooms.
fn room_label(url: &url::Url) -> String {
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
/// One broadcast per (participant, role). A shared screen would be
/// `<id>/screen.hang`: a second source from one person, not a second person.
/// Audio rides here with the camera because the two together are one
/// participant's live presence.
const CAMERA: &str = "camera.hang";

/// Where `participant` publishes.
fn camera_path(participant: &str) -> String {
    format!("{participant}/{CAMERA}")
}

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
/// Deliberately one field. Roles, candidacy and topology opinions are tickets
/// 22 and 23, and they extend this document rather than inventing their own.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Presence {
    state: State,
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
/// be; once a relay is in the middle, token path-scoping is what makes it
/// binding (ticket 23's job, not this one's).
fn participant_of(path: &str) -> Option<&str> {
    let id = path.strip_suffix(CAMERA)?.strip_suffix('/')?;
    (!id.is_empty()).then_some(id)
}

/// The video track inside that broadcast.
const TRACK: &str = "video.avc3";

/// The encoder's pacing hint, not a promise. Matches the loopback.
const FRAMERATE: u32 = 30;

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
/// `args`; moq-native reads its own `MOQ_IROH_SECRET`, so the two are bridged
/// here rather than teaching `args` a second variable. `--ephemeral` is `None`,
/// a throwaway key, which is what every launch used to be. Otherwise a **path**,
/// which moq-native generates into on first use and reads back afterwards
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

/// Retry pacing, shared by both sides.
///
/// Only the dialling side redials -- the listener has nothing to dial -- but
/// both sides read [`moq_native::Backoff::linger`] from the same value, because
/// what it decides is how long each side holds the *other's* broadcast open
/// waiting for it to come back. The two have to agree or one side tears down a
/// call the other is still repairing.
///
/// The escalation itself is left alone: 1 s, doubling to 5 s, is a sensible
/// climb, and the jitter matters even between two peers.
fn backoff() -> moq_native::Backoff {
    let mut backoff = moq_native::Backoff::default();
    backoff.timeout = RETRY_BUDGET;
    backoff
}

/// Runs a call on its own thread for the lifetime of the process.
///
/// The thread outlives any one call: an ending is reported and then the loop
/// goes back to idle, so a call that drops for good can be replaced without
/// restarting the app. Only losing the interface itself ends this.
pub(super) fn spawn(
    decisions: futures_channel::mpsc::UnboundedReceiver<Peering>,
    feed: FrameFeed,
    status: UnboundedSender<RemoteStatus>,
    participants: UnboundedSender<ParticipantEvent>,
    controls: Arc<Controls>,
    metrics: Arc<Metrics>,
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
                match run(decisions, feed, &status, participants, controls, metrics).await {
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
async fn run(
    mut decisions: futures_channel::mpsc::UnboundedReceiver<Peering>,
    feed: FrameFeed,
    status: &UnboundedSender<RemoteStatus>,
    participants: UnboundedSender<ParticipantEvent>,
    controls: Arc<Controls>,
    metrics: Arc<Metrics>,
) -> Result<(), String> {
    let report = |next: RemoteStatus| {
        let _ = status.unbounded_send(next);
    };

    report(RemoteStatus::Binding);

    let mut client_config = moq_native::ClientConfig::default();
    client_config.backoff = backoff();
    // Read by `bind` below and applied to the endpoint's transport config, which
    // is what makes it symmetric: the listener shares this endpoint and so
    // inherits the same timeout without a config of its own.
    client_config.quic.idle_timeout = Some(IDLE_TIMEOUT);
    let mut iroh = moq_native::iroh::EndpointConfig::default();
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
    let linger = backoff().linger();

    loop {
        // The decision may already be made (`--listen` / `--dial`) or may be the
        // user's to make (19c). Either way it is awaited *here*, after binding and
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

        report(RemoteStatus::AwaitingCamera);
        let Some(waiting) = parked.take() else {
            return Err("the camera feed is gone".to_string());
        };
        let (mut feed, width, height) = tokio::task::spawn_blocking(move || {
            let mut feed = waiting;

            let first = feed.wait()?;
            Some((feed, first.width, first.height))
        })
        .await
        .map_err(|error| format!("the camera wait failed: {error}"))?
        .ok_or_else(|| "the camera closed before a frame arrived".to_string())?;

        // Encoded smaller than captured, when the camera insists on more than the
    // link can carry. The bitrate follows from the size, so this is where it
    // gets decided.
    let (width, height, scale) = encode_size(width, height);
    metrics.set_encode_size(width, height);
    let mut config = moq_video::encode::Config::new(width, height, FRAMERATE);
    let opening_bitrate = crate::camera::bitrate_for(width, height, FRAMERATE);
    config.bitrate = Some(opening_bitrate);
    // Filled in by the rate loop below, read by the encode loop. Stays empty
    // unless this attempt turns out to be one that can measure the link.
    let target: adapt::Target = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    // Set when a session comes back, read by the encode loop. A subscriber that
    // rejoins mid-stream cannot decode until a keyframe arrives, and the encoder
    // already inserts one every two seconds -- so this only buys back that wait,
    // but the wait is a frozen tile and worth buying.
    let keyframe = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let rendition = config
            .probe()
            .await
            .map_err(|error| format!("could not probe the encoder: {error:#}"))?;
        let mut encoder = moq_video::encode::Encoder::new(&config)
            .map_err(|error| format!("could not build an encoder: {error:#}"))?;
        tracing::info!(
            "encoding {width}x{height} at the {} rung with {}",
            crate::args::Args::get().quality,
            encoder.name()
        );

        let outbound = moq_net::Origin::random().produce();
        let mut broadcast = outbound
            .create_broadcast(
                camera_path(&me.to_string()),
                moq_net::broadcast::Route::new().with_announce(true),
            )
            .map_err(|error| format!("could not create a broadcast: {error:#}"))?;
        let catalog = moq_mux::catalog::Producer::new(&mut broadcast)
            .map_err(|error| format!("could not create a catalog: {error:#}"))?;
        let track = broadcast
            .create_track(TRACK, catalog.track_info())
            .map_err(|error| format!("could not create a track: {error:#}"))?;
        let mut producer =
            moq_video::encode::Producer::with_track(track, catalog.clone(), rendition)
                .map_err(|error| format!("could not publish video: {error:#}"))?;

        // Announced beside the camera, and said before anything is shown: the
        // peer knows we are here whether or not a frame ever arrives.
        let mut control = outbound
            .create_broadcast(
                control_path(&me.to_string()),
                moq_net::broadcast::Route::new().with_announce(true),
            )
            .map_err(|error| format!("could not create a control broadcast: {error:#}"))?;
        let mut presence = control
            .create_track(PRESENCE, None)
            .map(|track| {
                moq_json::snapshot::Producer::new(
                    track,
                    moq_json::snapshot::ProducerConfig::default(),
                )
            })
            .map_err(|error| format!("could not create a presence track: {error:#}"))?;
        if let Err(error) = presence.update(&Presence { state: State::Here }) {
            tracing::warn!("could not publish our presence: {error:#}");
        }

        let engine = audio::open_playback().await;
        match audio::publish(broadcast.clone(), catalog, engine.as_ref()) {
            Ok((publication, driver)) => {
                controls.attach(publication);
                audio::drive(driver);
            }
            Err(error) => tracing::warn!("[audio] the microphone is not being published: {error}"),
        }

        // **Lingers rather than tearing down.** A session dying abruptly detaches
        // the peer's broadcast from this origin, and with the default zero
        // linger that closes it on the spot: our decoder's read ends, the task
        // returns, the tile goes black -- and nothing ever re-subscribes, so the
        // transport quietly redialling underneath changes nothing. A linger
        // keeps the path announced instead, and a session re-attaching within
        // the window splices into the same broadcast, so the gap is never
        // observed above this line.
        let inbound = moq_net::origin::Info::new(moq_net::Origin::random())
            .with_linger(linger)
            .produce();

        // Read before the match below takes `peering` apart.
        let in_room = matches!(peering, Peering::Join(_));
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
            Peering::Join(ref url) => {
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
            Peering::Listen => {
                report(RemoteStatus::Listening { me: me.to_string() });
                let mut server_config = moq_native::ServerConfig::default();

                server_config.bind = Some("[::]:0".to_string());
                server_config.tls.generate = vec!["localhost".to_string()];
                let server = server_config
                    .init()
                    .map_err(|error| format!("could not build a server: {error:#}"))?
                    .with_iroh(endpoint.clone());
                let publish = outbound.consume();
                let subscribe = inbound.clone();
                // **Our own accept loop, rather than `serve_both`.** That
                // convenience method is exactly this loop, plus a private
                // helper that does `request.ok()` and waits -- but it never
                // hands the session back, and the session is where a bandwidth
                // estimate lives. Without one, a callee sends at its opening
                // rate for the whole call however bad the link gets.
                //
                // The origins stay on the server rather than moving to each
                // request: `accept` attaches them, which is why the upstream
                // helper can get away with a bare `ok()`.
                let mut server = server.with_publisher(publish).with_subscriber(subscribe);
                // Persistent, and fed from whichever session is live. The
                // dialling side gets this shape for free from `Reconnect`;
                // here it is ours to keep, because an estimate has to exist
                // before anyone has called.
                let estimate = moq_net::bandwidth::Producer::new();
                let bandwidth = Some(estimate.consume());
                // The accept loop below is an `async move`, so it would take
                // the shared handle with it and leave the next attempt without
                // one.
                let metrics = metrics.clone();
                let status = status.clone();
                let keyframe = keyframe.clone();
                // **Accepting is its own task, not a branch below.** A listener
                // has to watch two things at once -- new sessions and the ones
                // it has -- and `accept` parks half-finished handshakes on the
                // server itself, so dropping it mid-poll to take the other
                // branch could lose exactly the reconnection being waited for.
                // `true` says a session began, `false` that one ended; the
                // channel closing says the server did.
                let (sessions, mut events) = futures_channel::mpsc::unbounded::<bool>();
                let accepting = Abort(tokio::spawn(async move {
                    while let Some(request) = server.accept().await {
                        let _ = sessions.unbounded_send(true);
                        let estimate = estimate.clone();
                        let metrics = metrics.clone();
                        let sessions = sessions.clone();
                        tokio::spawn(async move {
                            match request.ok().await {
                                Ok(session) => adapt::forward(session, estimate, metrics).await,
                                Err(error) => {
                                    tracing::warn!("a session failed to start: {error:#}")
                                }
                            }
                            let _ = sessions.unbounded_send(false);
                        });
                    }
                }));
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
                                            keyframe.store(
                                                true,
                                                std::sync::atomic::Ordering::Relaxed,
                                            );
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
                    bandwidth,
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
        let rate = bandwidth
            .map(|bandwidth| adapt::spawn(bandwidth, opening_bitrate, target.clone(), metrics.clone()));
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
                in_room,
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
        // hands all three back on the way out, because the next attempt needs
        // the feed and this one still has to flush.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut encoding = tokio::task::spawn_blocking({
            let stop = stop.clone();
            let target = target.clone();
            let keyframe = keyframe.clone();
            move || {
                let outcome = encode(
                    &mut feed,
                    &mut encoder,
                    &mut producer,
                    scale,
                    &target,
                    &keyframe,
                    &stop,
                );
                (feed, encoder, producer, outcome)
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
                    let outcome = parts.3.clone();
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
        // itself, and its three values are needed here.
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let (feed, mut encoder, mut producer) = match encoded {
            Some((feed, encoder, producer, _)) => (feed, encoder, producer),
            None => match encoding.await {
                Ok((feed, encoder, producer, _)) => (feed, encoder, producer),
                Err(error) => return Err(format!("the encode thread failed: {error}")),
            },
        };

        // **Finish the media, then say goodbye, then unannounce** -- in that
        // order, because each step is only true once the one before it is.
        // Writing a flushed frame into a broadcast that has already been
        // finished re-announces it, which the peer reads as somebody leaving and
        // immediately rejoining; observed as a `left`/`joined` pair 53
        // microseconds apart before this was ordered properly.
        if let Ok(flushed) = encoder.flush() {
            if !flushed.is_empty() {
                producer.publish(&flushed).ok();
            }
        }
        producer.finish().ok();

        // **Said, then shown.** The document is advisory -- it is what lets the
        // peer say "they hung up" rather than "they vanished" -- but `finish`
        // is what ends it: a deliberate unannounce closes the broadcast on the
        // peer immediately, where an abrupt loss lingers for the reconnect
        // window instead. If the last word does not make it, the unannounce
        // still does.
        if let Err(error) = presence.update(&Presence {
            state: State::Leaving,
        }) {
            tracing::warn!("could not say we are leaving: {error:#}");
        }
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
        control.finish();
        broadcast.finish();
        // Long enough for the finishes above to leave, then the line goes. No
        // handle says when they have, so this is a bound rather than a wait
        // for something; short, because a leave should feel instant.
        tokio::time::sleep(GOODBYE_FLUSH).await;
        drop(transport);
        decode.abort();
        if let Some(rate) = rate {
            rate.abort();
        }
        if let Some(reporting) = reporting {
            reporting.abort();
        }
        metrics.clear();
        controls.detach();
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
fn encode(
    feed: &mut FrameFeed,
    encoder: &mut moq_video::encode::Encoder,
    producer: &mut moq_video::encode::Producer,
    scale: usize,
    target: &adapt::Target,
    keyframe: &std::sync::atomic::AtomicBool,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    loop {
        // The only way out other than the camera going away. Checked per frame
        // rather than per sleep, so a busy encoder still notices promptly.
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        // Applied here because this thread owns the encoder. A rate arriving
        // mid-frame waits for the next one, which is well inside what the
        // control loop ramps over.
        if let Some(bitrate) = adapt::take(target) {
            if let Err(error) = encoder.set_bitrate(bitrate) {
                tracing::warn!("could not move the encoder to {bitrate} bps: {error:#}");
            }
        }
        // A peer that has just rejoined is holding a frozen tile until it gets
        // one of these. Asked for on the next frame rather than immediately,
        // which is the same rule the bitrate above follows and for the same
        // reason: this thread owns the encoder.
        if keyframe.swap(false, std::sync::atomic::Ordering::Relaxed) {
            encoder.keyframe();
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
        // The encoder paces on these, so a wrong one stalls the stream.
        let Ok(timestamp) = moq_net::Timestamp::from_micros(frame.timestamp.as_micros() as u64)
        else {
            tracing::warn!("frame timestamp out of range; skipping");
            continue;
        };
        let encoded = encoder
            .encode(&moq_video::Frame::new(surface, timestamp))
            .map_err(|error| format!("encode failed: {error:#}"))?;
        if !encoded.is_empty() {
            producer
                .publish(&encoded)
                .map_err(|error| format!("publish failed: {error:#}"))?;
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
    /// Whether this is a room on a relay rather than one peer.
    ///
    /// Changes two rules. Everyone leaving does not end it -- a room stays open
    /// for whoever comes next, and only Hang up leaves. And no single peer is
    /// "who we are connected to", so the status line stays on the room.
    in_room: bool,
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
        in_room,
    } = reports;
    let mut announced = inbound.announced();
    // Dropping an entry aborts that participant's decoders, so leaving is the
    // same operation as the attempt ending.
    let mut attending: std::collections::HashMap<String, Vec<Abort>> =
        std::collections::HashMap::new();
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
    // Where the per-participant readers report a goodbye. One arbiter, so the
    // rule for "is the call over" is written once.
    let (saying, mut said) = futures_channel::mpsc::unbounded::<String>();

    loop {
        let update = tokio::select! {
            update = announced.next() => match update {
                Some(update) => update,
                None => break,
            },
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
        let path = update.path.as_str().to_string();

        if let Some(id) = controller_of(&path) {
            if id == me {
                continue;
            }
            match update.broadcast {
                Some(broadcast) => {
                    present.insert(id.to_string());
                    listening.insert(
                        id.to_string(),
                        Abort(tokio::spawn(listen(
                            broadcast,
                            id.to_string(),
                            saying.clone(),
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

        let Some(broadcast) = update.broadcast else {
            tracing::info!("{id} left");
            attending.remove(id);
            let _ = participants.unbounded_send(ParticipantEvent::Left { id: id.to_string() });
            continue;
        };

        tracing::info!("{id} joined");
        let slot = FrameSlot::default();
        let stats = Arc::new(PeerStats::default());
        let _ = participants.unbounded_send(ParticipantEvent::Joined {
            id: id.to_string(),
            slot: slot.clone(),
            stats: stats.clone(),
        });

        let mut tasks = vec![Abort(tokio::spawn(watch(
            broadcast.clone(),
            slot,
            id.to_string(),
            (!in_room).then(|| status.clone()),
            stats.clone(),
        )))];
        if let Some(engine) = engine.clone() {
            let peer = id.to_string();
            let talking = stats.talking_flag();
            tasks.push(Abort(tokio::spawn(async move {
                audio::subscribe(broadcast, peer, engine, talking).await;
            })));
        } else {
            tracing::warn!("[audio] {id} cannot be heard: there is no output device");
        }
        attending.insert(id.to_string(), tasks);
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
async fn listen(
    broadcast: moq_net::broadcast::Consumer,
    id: String,
    saying: UnboundedSender<String>,
) {
    let track = match broadcast.track(PRESENCE) {
        Ok(track) => track,
        Err(error) => {
            tracing::warn!("{id} has no presence track: {error:#}");
            return;
        }
    };
    // Preempts media, the way hang prioritises its own catalog: this is small
    // and structural, and late it is worthless.
    let subscription =
        moq_net::track::Subscription::default().with_priority(CONTROL_PRIORITY);
    let track = match track.subscribe(subscription).await {
        Ok(track) => track,
        Err(error) => {
            tracing::warn!("could not subscribe to {id}'s presence: {error:#}");
            return;
        }
    };
    let mut presence = moq_json::snapshot::Consumer::<Presence>::new(
        track,
        moq_json::snapshot::ConsumerConfig::default(),
    );

    loop {
        match presence.next().await {
            Ok(Some(Presence { state })) => {
                tracing::info!("{id} says {state:?}");
                if state == State::Leaving {
                    // Reported, not acted on: whether the call is over is a
                    // question about everyone, and the loop that tracks
                    // everyone answers it.
                    let _ = saying.unbounded_send(id);
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                tracing::warn!("could not read {id}'s presence: {error:#}");
                return;
            }
        }
    }
}

/// Opens a decoder from one participant's catalog and drains frames into their
/// slot.
///
/// **The catalog is theirs, never a local guess.** 20c failed here by passing a
/// *local* config and a hardcoded track name instead, and got `Net(Remote(30))`
/// for it.
async fn watch(
    broadcast: moq_net::broadcast::Consumer,
    slot: FrameSlot,
    peer: String,
    // `None` in a room, where one peer's video arriving is not the call's
    // status -- see `Reports::in_room`.
    status: Option<UnboundedSender<RemoteStatus>>,
    stats: Arc<PeerStats>,
) {
    // The catalog is the peer's description of what it is sending. 20c failed
    // here by passing a *local* config and a hardcoded track name instead, and
    // got `Net(Remote(30))` for it.
    let mut catalog = match moq_mux::catalog::Consumer::<()>::new(
        &broadcast,
        moq_mux::catalog::CatalogFormat::Hang,
    )
    .await
    {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::error!("no catalog from the peer: {error:#}");
            return;
        }
    };
    let snapshot = match catalog.next().await {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            tracing::warn!("the peer's catalog closed before a snapshot");
            return;
        }
        Err(error) => {
            tracing::error!("could not read the peer's catalog: {error:#}");
            return;
        }
    };
    let Some((name, config)) = snapshot.video.renditions.iter().next() else {
        tracing::warn!("the peer's catalog offers no video rendition");
        return;
    };

    let mut consumer = match moq_video::decode::Consumer::new(
        &broadcast,
        config,
        name,
        moq_video::decode::Config::default(),
    )
    .await
    {
        Ok(consumer) => consumer,
        Err(error) => {
            tracing::error!("no decoder for the peer's video: {error:#}");
            return;
        }
    };
    tracing::info!("decoding {peer}'s video with {}", consumer.name());
    // **The listening side can name its caller now.** iroh exposes no client
    // identity to a server, so the transport genuinely cannot say who this is --
    // but the broadcast path can, and does.
    if let Some(status) = &status {
        let _ = status.unbounded_send(RemoteStatus::Connected {
            peer: Some(peer.clone()),
        });
    }

    // The network half's own rate, independent of whether anything is painting.
    // `camera::paint::FrameRate` measures capture against the GPU; this measures
    // what actually arrived from the peer, which is the only number that tells
    // a stalled connection apart from an occluded window.
    let mut decoded = 0u32;
    let mut since = std::time::Instant::now();

    // Per tile, so a stalled peer shows as one tile at zero rather than hiding
    // behind whichever healthy peer wrote the shared number last.
    let meter = FpsMeter::new(stats);

    while let Ok(Some(frame)) = consumer.read().await {
        meter.frame();
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
    // The producer is gone, so the last frame must come off the screen rather
    // than sit there looking live.
    slot.clear();
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
    }
}

#[cfg(test)]
mod room_tests {
    use super::{explain, redacted, room_label};

    #[test]
    fn a_token_never_reaches_the_log() {
        let url: url::Url = "https://relay.example.com:4443/test?jwt=secret.token.value".parse().unwrap();
        let shown = redacted(&url);
        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("relay.example.com:4443/test"), "{shown}");
    }

    #[test]
    fn a_url_without_a_token_is_left_alone() {
        let url: url::Url = "http://localhost:4443/test".parse().unwrap();
        assert_eq!(redacted(&url), "http://localhost:4443/test");
    }

    #[test]
    fn a_refused_token_is_said_in_words() {
        let raw = "reconnect timed out after 30s: transport: webtransport error: closed: code=6 reason=unauthorized";
        assert!(explain(raw.to_string()).starts_with("the relay refused this room's token"));
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
