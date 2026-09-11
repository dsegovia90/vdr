//! First contact: one peer, both directions, over iroh and MoQ.
//!
//! The shape is the loopback's with the pipe replaced by a real connection.
//! `--listen` runs a `moq_native::Server` and `--dial` runs a
//! `moq_native::Client` against it; after the handshake the two sides are
//! identical, and so is everything below the transport.
//!
//! **Peer to peer, no relay.** The listening side is its own tiny server rather
//! than a client of a third party, which keeps today's behaviour exactly: two
//! processes, one key typed into the other. A relay only enters when the
//! topology switch is built (ticket 22), and ticket 21 already proved a
//! participant can host one.
//!
//! Both arms converge on [`Peer`], so the claim above is structural rather than
//! a description: anything per-connection is reached the same way whichever
//! side established it.
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
use crate::args::Peering;
use super::Metrics;
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

/// The broadcast path both sides publish on.
///
/// The `.hang` suffix is load-bearing: catalog-format detection keys off it,
/// and without it a subscriber finds nothing and says so only by timing out.
const BROADCAST: &str = "call.hang";

/// The video track inside that broadcast.
const TRACK: &str = "video.avc3";

/// The encoder's pacing hint, not a promise. Matches the loopback.
const FRAMERATE: u32 = 30;

/// How many decoded frames between rate reports in the log. Matches the paint
/// path's cadence so the two logs are comparable.
const REPORT_EVERY: u32 = 120;

/// How often the incoming frame rate is published to the interface.
///
/// Faster than the log on purpose: the log is for comparing runs afterwards,
/// this is for watching a link misbehave while it happens.
const FPS_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

/// Runs a call on its own thread for the lifetime of the process.
///
/// Terminal by design: when the call ends, the status says so and nothing
/// redials. Reconnection is a real feature with real questions behind it -- who
/// retries, how often, what the tile shows meanwhile -- and guessing at it here
/// would make the first failure harder to read, not easier. Ticket 19b.
pub(super) fn spawn(
    decisions: futures_channel::mpsc::UnboundedReceiver<Peering>,
    feed: FrameFeed,
    slot: FrameSlot,
    status: UnboundedSender<RemoteStatus>,
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
                match run(decisions, feed, slot, &status, controls, metrics).await {
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

/// Binds, publishes, connects, and pumps the peer's video into `slot`.
///
/// Errors are formatted rather than typed: they come from several crates with
/// several error types, none of which the UI can do anything with beyond
/// showing the text.
async fn run(
    mut decisions: futures_channel::mpsc::UnboundedReceiver<Peering>,
    feed: FrameFeed,
    slot: FrameSlot,
    status: &UnboundedSender<RemoteStatus>,
    controls: Arc<Controls>,
    metrics: Arc<Metrics>,
) -> Result<(), String> {
    let report = |next: RemoteStatus| {
        let _ = status.unbounded_send(next);
    };

    report(RemoteStatus::Binding);

    let client_config = moq_native::ClientConfig::default();
    let mut iroh = moq_native::iroh::EndpointConfig::default();
    iroh.enabled = Some(true);
    // `--secret` is forwarded into `IROH_SECRET` by `args`; moq-native reads its
    // own `MOQ_IROH_SECRET`, so the two are bridged here rather than teaching
    // `args` a second variable. Unset means a throwaway key, as before.
    iroh.secret = std::env::var("IROH_SECRET").ok();
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

    loop {
        // The decision may already be made (`--listen` / `--dial`) or may be the
        // user's to make (19c). Either way it is awaited *here*, after binding and
        // before the camera: our key must be visible while we wait, and waiting on
        // the camera first would hide it behind a device that may never open.
        report(RemoteStatus::Idle { me: me.to_string() });
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
        let rendition = config
            .probe()
            .await
            .map_err(|error| format!("could not probe the encoder: {error:#}"))?;
        let mut encoder = moq_video::encode::Encoder::new(&config)
            .map_err(|error| format!("could not build an encoder: {error:#}"))?;
        tracing::info!("encoding {width}x{height} with {}", encoder.name());

        let outbound = moq_net::Origin::random().produce();
        let mut broadcast = outbound
            .create_broadcast(
                BROADCAST,
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

        let engine = audio::open_playback().await;
        match audio::publish(broadcast.clone(), catalog, engine.as_ref()) {
            Ok((publication, driver)) => {
                controls.attach(publication);
                audio::drive(driver);
            }
            Err(error) => tracing::warn!("[audio] the microphone is not being published: {error}"),
        }

        let inbound = moq_net::Origin::random().produce();

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
                let client = moq_native::ClientConfig::default()
                    .init()
                    .map_err(|error| format!("could not build a client: {error:#}"))?
                    .with_iroh(endpoint.clone());
                let session = client
                    .with_publisher(&outbound)
                    .with_subscriber(inbound.clone())
                    .reconnect(url);
                // Taken before the session moves into the future below. It
                // survives a reconnect, so the estimate follows the link across
                // one rather than being restarted.
                let bandwidth = Some(session.send_bandwidth());
                let stats = Some(session.stats());
                Peer {
                    closed: Box::pin(async move {
                        match session.closed().await {
                            Ok(()) => "the peer closed the session".to_string(),
                            Err(error) => format!("{error:#}"),
                        }
                    }),
                    bandwidth,
                    stats,
                }
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
                Peer {
                    closed: Box::pin(async move {
                        while let Some(request) = server.accept().await {
                            let estimate = estimate.clone();
                            let metrics = metrics.clone();
                            tokio::spawn(async move {
                                match request.ok().await {
                                    Ok(session) => {
                                        adapt::forward(session, estimate, metrics).await
                                    }
                                    Err(error) => {
                                        tracing::warn!("a session failed to start: {error:#}")
                                    }
                                }
                            });
                        }
                        "the listener stopped".to_string()
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

        let peer_label = match &peering {
            Peering::Dial(peer) => Some(peer.to_string()),
            _ => None,
        };
        let decode = tokio::spawn(subscribe(
            inbound.consume(),
            slot.clone(),
            status.clone(),
            peer_label,
            metrics.clone(),
        ));

        let audio_decode = engine.map(|engine| {
            let inbound = inbound.consume();
            let controls = controls.clone();
            tokio::spawn(async move {
                let Some(broadcast) = inbound.announced_broadcast(BROADCAST).await else {
                    tracing::warn!("[audio] the peer never announced {BROADCAST}");
                    return;
                };
                audio::subscribe(broadcast, engine, controls).await;
            })
        });

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
            move || {
                let outcome = encode(&mut feed, &mut encoder, &mut producer, scale, &target, &stop);
                (feed, encoder, producer, outcome)
            }
        });

        // Set when the encode thread is what ended the attempt, so the join
        // below knows not to await a handle that has already resolved.
        let mut encoded = None;

        let outcome = tokio::select! {
            error = transport => Some(Err(error)),
            joined = &mut encoding => match joined {
                Ok(parts) => {
                    let outcome = parts.3.clone();
                    encoded = Some(parts);
                    Some(outcome)
                }
                Err(error) => Some(Err(format!("the encode thread failed: {error}"))),
            },
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

        if let Ok(flushed) = encoder.flush() {
            if !flushed.is_empty() {
                producer.publish(&flushed).ok();
            }
        }
        producer.finish().ok();
        decode.abort();
        if let Some(audio_decode) = audio_decode {
            audio_decode.abort();
        }
        if let Some(rate) = rate {
            rate.abort();
        }
        if let Some(reporting) = reporting {
            reporting.abort();
        }
        metrics.clear();
        controls.detach();
        slot.clear();

        parked = Some(feed);

        match outcome {
            Some(outcome) => return outcome,
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

/// Waits for the peer's broadcast, opens a decoder from its catalog, and drains
/// decoded frames into `slot`.
///
/// **`announced_broadcast`, never `request_broadcast`.** The latter answers on
/// the spot, so asking it first races the announcement and reports a live
/// broadcast as unroutable -- which presents as "the peer has no video" against
/// a peer that is publishing perfectly.
async fn subscribe(
    inbound: moq_net::origin::Consumer,
    slot: FrameSlot,
    status: UnboundedSender<RemoteStatus>,
    peer: Option<String>,
    metrics: Arc<Metrics>,
) {
    let Some(broadcast) = inbound.announced_broadcast(BROADCAST).await else {
        tracing::warn!("the peer never announced {BROADCAST}");
        return;
    };

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
    tracing::info!("decoding the peer's video with {}", consumer.name());
    let _ = status.unbounded_send(RemoteStatus::Connected { peer });

    // The network half's own rate, independent of whether anything is painting.
    // `camera::paint::FrameRate` measures capture against the GPU; this measures
    // what actually arrived from the peer, which is the only number that tells
    // a stalled connection apart from an occluded window.
    let mut decoded = 0u32;
    let mut since = std::time::Instant::now();

    // **Counted here, timed elsewhere.** Publishing the rate from inside this
    // loop would mean it only updates when a frame arrives, so a stalled stream
    // would leave the last healthy number on screen for as long as it was
    // stalled -- showing 30 fps through the freeze it is meant to reveal. A
    // ticker reading a counter reports the truth, which during a stall is zero.
    let counted = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let _ticker = Abort(tokio::spawn({
        let counted = counted.clone();
        let metrics = metrics.clone();
        async move {
            let mut ticker = tokio::time::interval(FPS_WINDOW);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let frames = counted.swap(0, std::sync::atomic::Ordering::Relaxed);
                metrics.set_video_fps(frames as f32 / FPS_WINDOW.as_secs_f32());
            }
        }
    }));

    while let Ok(Some(frame)) = consumer.read().await {
        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        decoded += 1;
        if decoded == REPORT_EVERY {
            let elapsed = since.elapsed().as_secs_f32();
            if elapsed > 0.0 {
                tracing::debug!(
                    "[remote] {:.1} fps decoded from the peer",
                    decoded as f32 / elapsed
                );
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
