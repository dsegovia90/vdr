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
//! Each side keeps **two origins** -- `outbound` for what we publish, `inbound`
//! for what arrives. They are separate `Origin`s, so both peers can publish at
//! the same well-known path without colliding, and neither needs to know the
//! other's id to subscribe. Per-participant paths are ticket 12's job.
//!
//! What we hand the encoder is the same [`FrameFeed`] the loopback uses: the
//! pump's fan-out tap. Nothing in `camera/` knows whether a peer is involved.

use std::future::Future;
use std::pin::Pin;

use futures_channel::mpsc::UnboundedSender;
use moq_mux::catalog::Stream as _;

use super::runtime::runtime;
use super::status::RemoteStatus;
use crate::args::Peering;
use crate::camera::{from_decoded, FrameFeed, FrameSlot};

/// The broadcast path both sides publish on.
///
/// The `.hang` suffix is load-bearing: catalog-format detection keys off it,
/// and without it a subscriber finds nothing and says so only by timing out.
const BROADCAST: &str = "call.hang";

/// The video track inside that broadcast.
const TRACK: &str = "video.avc3";

/// The encoder's pacing hint, not a promise. Matches the loopback.
const FRAMERATE: u32 = 30;

/// How many decoded frames between rate reports. Matches the paint path's
/// cadence so the two logs are comparable.
const REPORT_EVERY: u32 = 120;

/// Runs a call on its own thread for the lifetime of the process.
///
/// Terminal by design: when the call ends, the status says so and nothing
/// redials. Reconnection is a real feature with real questions behind it -- who
/// retries, how often, what the tile shows meanwhile -- and guessing at it here
/// would make the first failure harder to read, not easier. Ticket 19b.
pub(super) fn spawn(
    peering: Peering,
    feed: FrameFeed,
    slot: FrameSlot,
    status: UnboundedSender<RemoteStatus>,
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
                match run(peering, feed, slot, &status).await {
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
    peering: Peering,
    feed: FrameFeed,
    slot: FrameSlot,
    status: &UnboundedSender<RemoteStatus>,
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

    // Blocking, and before anything is published: the encoder is sized from a
    // real frame, because macOS overrides the resolution we asked for outright.
    // So a call still cannot be placed while the camera is off -- ticket 19a,
    // deliberately not fixed here.
    report(RemoteStatus::AwaitingCamera);
    let (mut feed, width, height) = tokio::task::spawn_blocking(move || {
        let mut feed = feed;
        // Discarded rather than encoded. Minutes can pass before a peer
        // subscribes, and the publish path anchors its pacing on the first
        // frame it is given -- encoding this one would peg the stream's clock
        // to a moment long gone.
        let first = feed.wait()?;
        Some((feed, first.width, first.height))
    })
    .await
    .map_err(|error| format!("the camera wait failed: {error}"))?
    .ok_or_else(|| "the camera closed before a frame arrived".to_string())?;

    // `probe` asks the encoder what it will emit, so the catalog is published
    // before anything is encoded -- which is what lets a peer open a decoder
    // against a track nothing has written to yet.
    let config = moq_video::encode::Config::new(width, height, FRAMERATE);
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
    let mut producer = moq_video::encode::Producer::with_track(track, catalog, rendition)
        .map_err(|error| format!("could not publish video: {error:#}"))?;

    let inbound = moq_net::Origin::random().produce();

    // Both arms end in a future that resolves only when the transport is done,
    // so the select below treats "the connection went away" the same either way.
    let transport: Pin<Box<dyn Future<Output = String> + Send>> = match peering {
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
            let client = client_config
                .init()
                .map_err(|error| format!("could not build a client: {error:#}"))?
                .with_iroh(endpoint);
            let session = client
                .with_publisher(&outbound)
                .with_subscriber(inbound.clone())
                .reconnect(url);
            Box::pin(async move {
                match session.closed().await {
                    Ok(()) => "the peer closed the session".to_string(),
                    Err(error) => format!("{error:#}"),
                }
            })
        }
        Peering::Listen => {
            report(RemoteStatus::Listening { me: me.to_string() });
            let mut server_config = moq_native::ServerConfig::default();
            // **Bind explicitly.** Unset means `[::]:443`, which every instance
            // would try to claim (ticket 20b). Nothing dials this listener --
            // iroh carries the call -- so an ephemeral port is exactly right.
            server_config.bind = Some("[::]:0".to_string());
            server_config.tls.generate = vec!["localhost".to_string()];
            let server = server_config
                .init()
                .map_err(|error| format!("could not build a server: {error:#}"))?
                .with_iroh(endpoint);
            let publish = outbound.consume();
            let subscribe = inbound.clone();
            Box::pin(async move {
                match server.serve_both(publish, subscribe).await {
                    Ok(()) => "the listener stopped".to_string(),
                    Err(error) => format!("{error:#}"),
                }
            })
        }
        // `use_remote` routes this case to the loopback and never gets here.
        Peering::Loopback => return Err("no peer to call".to_string()),
    };

    let peer_label = match &peering {
        Peering::Dial(peer) => Some(peer.to_string()),
        // iroh exposes no client identity to the accepting side, so there is
        // honestly nothing to name here.
        _ => None,
    };
    let decode = tokio::spawn(subscribe(
        inbound.consume(),
        slot,
        status.clone(),
        peer_label,
    ));

    // The encode loop and the transport both run until the call ends; whichever
    // stops first ends the call, and the decode task follows when its track
    // closes.
    let outcome = tokio::select! {
        error = transport => Err(error),
        result = encode(&mut feed, &mut encoder, &mut producer) => result,
    };

    if let Ok(flushed) = encoder.flush() {
        if !flushed.is_empty() {
            producer.publish(&flushed).ok();
        }
    }
    producer.finish().ok();
    decode.abort();
    outcome
}

/// Pulls camera frames off the tap, encodes them, and publishes them.
///
/// The loopback's loop verbatim -- which is the point of having had one.
async fn encode(
    feed: &mut FrameFeed,
    encoder: &mut moq_video::encode::Encoder,
    producer: &mut moq_video::encode::Producer,
) -> Result<(), String> {
    loop {
        // `take` errors only when every tap has dropped: the camera going away
        // for good, rather than a gap between frames.
        let frame = match feed.take() {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                continue;
            }
            Err(_) => return Ok(()),
        };

        let Some(surface) = frame.to_i420_surface() else {
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

    while let Ok(Some(frame)) = consumer.read().await {
        decoded += 1;
        if decoded == REPORT_EVERY {
            let elapsed = since.elapsed().as_secs_f32();
            if elapsed > 0.0 {
                tracing::debug!("[remote] {:.1} fps decoded from the peer", decoded as f32 / elapsed);
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
