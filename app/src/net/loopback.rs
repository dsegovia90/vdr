//! The network path with the network taken out.
//!
//! Camera frames go through a real H.264 encode, a real MoQ broadcast and a
//! real decode, and come back out as frames for a second tile. Everything here
//! except the missing connection is what a call will use.
//!
//! What it proves, and the order it proves it in: that the pump's fan-out feeds
//! an encoder without disturbing the preview, that a decoded frame reaches
//! `VideoPaintSource` and paints, and that the two tiles stay in step. Those
//! are the failures worth telling apart before a peer is involved, because over
//! a connection they all look the same from the outside -- a black tile.
//!
//! `call.rs` now runs the same stack, so the two differ only in the transport:
//! this one hands the encoder's output straight back to a decoder in-process,
//! and that one puts a connection in between.

use std::sync::Arc;

use super::audio;
use super::metrics::{FpsMeter, PeerStats};
use super::runtime::runtime;
use crate::audio::Controls;
use crate::camera::{FrameFeed, FrameSlot, encode_size, from_decoded};

/// Runs a loopback on its own thread for the lifetime of the process.
///
/// Teardown is by cascade rather than by signal: when the last
/// [`FrameTap`](crate::camera::FrameTap) drops, `FrameFeed::take` starts
/// erroring, the encode loop ends, the producer finishes, and the decode task
/// sees the track close and returns.
pub(super) fn spawn(
    feed: FrameFeed,
    slot: FrameSlot,
    stats: Arc<PeerStats>,
    controls: Arc<Controls>,
) {
    let spawned = std::thread::Builder::new()
        .name("loopback".into())
        .spawn(move || run(feed, slot, stats, controls));
    if let Err(error) = spawned {
        tracing::warn!("could not spawn the loopback thread: {error}");
    }
}

fn run(mut feed: FrameFeed, slot: FrameSlot, stats: Arc<PeerStats>, controls: Arc<Controls>) {
    let Some(runtime) = runtime() else {
        return;
    };

    // Blocks until the camera produces something, which may be never -- the
    // user can start with the camera off. That is why this is a thread and not
    // part of the hook. The encoder is sized from a real frame rather than from
    // the request, because macOS discards the request entirely.
    let Some(first) = feed.wait() else {
        tracing::debug!("loopback: the camera closed before a frame arrived");
        return;
    };
    let (width, height) = (first.width, first.height);

    runtime.block_on(async move {
        if let Err(error) = pump(feed, slot, stats, width, height, controls).await {
            tracing::error!("loopback: {error:#}");
        }
        tracing::info!("loopback: stopped");
    });
}

/// Wires encoder -> broadcast -> decoder and runs until the camera goes away.
async fn pump(
    mut feed: FrameFeed,
    slot: FrameSlot,
    stats: Arc<PeerStats>,
    width: u32,
    height: u32,
    controls: Arc<Controls>,
) -> anyhow::Result<()> {
    // Framerate is the encoder's pacing hint, not a promise.
    let (width, height, scale) = encode_size(width, height);
    let framerate = moq_video::Rate::new(30, 1).expect("a positive whole frame rate");
    let mut config = moq_video::encode::Config::new(width, height, framerate);
    config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(crate::camera::bitrate_for(width, height, 30)));
    // `probe` asks the encoder what it will emit, so the catalog is published
    // before anything is encoded -- which is what lets the decoder below open
    // against a track nothing has written to yet.
    let rendition = config.probe().await?;
    let mut encoder = moq_video::encode::Encoder::new(&config)?;
    tracing::info!(
        "loopback: encoding {width}x{height} with {} ({scale}x downscale)",
        encoder.name()
    );

    // Kept for the whole run: dropping the origin would end the broadcast.
    let origin = moq_tokio::origin::spawn();
    let mut broadcast = origin.create_broadcast("loopback.hang")?;
    let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default())?;
    // Video's priority, as hang's catalog ranks it (audio is 80).
    let track = broadcast.create_track("video.avc3", catalog.track_info(60))?;
    let mut producer =
        moq_video::encode::Producer::with_track(track, catalog.clone(), rendition.clone())?;

    let engine = audio::open_playback(&controls).await;
    match audio::publish(broadcast.clone(), catalog, engine.as_ref()) {
        Ok((publication, driver, canceller)) => {
            controls.attach(publication);
            audio::drive(driver);
            // Loopback plays our own voice back, which the canceller's
            // reference holds: the easy case, useful for seeing it settle.
            if let Some(canceller) = canceller {
                tokio::spawn(audio::report_aec(canceller, None));
            }
        }
        Err(error) => {
            tracing::warn!("loopback: [audio] the microphone is not being published: {error}")
        }
    }
    broadcast.announce(moq_net::origin::Route::default())?;
    let audio_decode = engine.map(|engine| {
        let broadcast = broadcast.consume();
        tokio::spawn(audio::subscribe(
            broadcast,
            "loopback".to_string(),
            engine,
            stats.clone(),
        ))
    });

    // In-process, so the producer's own consumer is the whole "network". A call
    // resolves the rendition from the catalog it receives instead.
    let consumer_broadcast = broadcast.consume();
    let decode = tokio::spawn(async move {
        // The same per-tile rate a peer's tile shows, so loopback can be read
        // the same way.
        let meter = FpsMeter::new(stats);
        let mut consumer = match moq_video::decode::Consumer::new(
            &consumer_broadcast,
            &rendition,
            "video.avc3",
            moq_video::decode::Options::default(),
        )
        .await
        {
            Ok(consumer) => consumer,
            Err(error) => {
                tracing::error!("loopback: no decoder: {error:#}");
                return;
            }
        };
        tracing::info!("loopback: decoding with {}", consumer.name());

        while let Ok(Some(frame)) = consumer.read().await {
            meter.frame();
            if let Some(frame) = from_decoded(frame) {
                slot.publish(frame);
            }
        }
        // The producer is gone, so the last frame must come off the screen
        // rather than sit there looking live.
        slot.clear();
    });

    loop {
        let frame = match feed.take() {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                continue;
            }
            Err(_) => break,
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
        let encoded = encoder.encode(&moq_video::Frame::new(surface, timestamp))?;
        if !encoded.is_empty() {
            producer.publish(&encoded)?;
        }
    }

    if let Ok(flushed) = encoder.flush() {
        if !flushed.is_empty() {
            producer.publish(&flushed).ok();
        }
    }
    producer.finish().ok();
    decode.await.ok();
    if let Some(audio_decode) = audio_decode {
        audio_decode.abort();
    }
    controls.detach();
    Ok(())
}
