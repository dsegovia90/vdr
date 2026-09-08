//! First contact: one peer, both directions, over iroh.
//!
//! The shape is the loopback's with the pipe replaced by a QUIC connection.
//! `iroh_live::Call` publishes our [`LocalBroadcast`] and subscribes to the
//! peer's on a single MoQ session, so "dial" and "accept" differ only in who
//! opened the connection -- after that the two sides are identical, and so is
//! the code below.
//!
//! What we hand it is still [`TapSource`]: the same fan-out that fed the
//! loopback encoder now feeds the one that publishes. Nothing in `camera/`
//! knows the difference.

use futures_channel::mpsc::UnboundedSender;
use iroh_live::media::codec::VideoCodec;
use iroh_live::media::format::VideoPreset;
use iroh_live::media::publish::LocalBroadcast;
use iroh_live::{Call, Live};

use super::runtime::runtime;
use super::sink::drain;
use super::source::TapSource;
use super::status::RemoteStatus;
use crate::args::Peering;
use crate::camera::{FrameFeed, FrameSlot};

/// What the peer sees us publish. Matches the loopback's preset, and is the
/// only rendition we offer -- simulcast is not built yet.
const PRESET: VideoPreset = VideoPreset::P720;

/// Runs a call on its own thread for the lifetime of the process.
///
/// Terminal by design: when the call ends, the status says so and nothing
/// redials. Reconnection is a real feature with real questions behind it -- who
/// retries, how often, what the tile shows meanwhile -- and guessing at it here
/// would make the first failure harder to read, not easier.
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

/// Binds, publishes, connects, and then pumps the peer's video into `slot`.
///
/// Errors are formatted rather than typed: they come from four crates with
/// four error types, none of which the UI can do anything with beyond showing
/// the text.
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
    // `with_router` is what makes us reachable: without it the endpoint dials
    // out but never accepts, so `--listen` would wait forever with nothing
    // wrong to see.
    let live = Live::from_env()
        .await
        .map_err(|error| format!("could not bind an endpoint: {error:#}"))?
        .with_router()
        .spawn();
    let me = live.endpoint().id();
    // Logged as well as shown, because this is the string the other side needs
    // and a window is a poor place to copy from.
    tracing::info!("our endpoint id: {me}");

    // Blocking, and before anything is published: the encoder is sized from a
    // real frame (see `TapSource`). So a call cannot be placed or accepted
    // while the camera is off -- worth knowing, and worth fixing later by
    // publishing an empty broadcast and setting the source when video starts.
    report(RemoteStatus::AwaitingCamera);
    let source = tokio::task::spawn_blocking(move || TapSource::wait_for_first("call", feed))
        .await
        .map_err(|error| format!("the camera wait failed: {error}"))?
        .ok_or_else(|| "the camera closed before a frame arrived".to_string())?;

    let broadcast = LocalBroadcast::new();
    broadcast
        .video()
        .set_source(source, VideoCodec::H264, [PRESET])
        .map_err(|error| format!("could not publish video: {error:#}"))?;

    let call = match peering {
        Peering::Dial(peer) => {
            report(RemoteStatus::Dialing {
                peer: peer.to_string(),
            });
            tracing::info!("dialing {peer}");
            Call::dial(&live, peer, broadcast)
                .await
                .map_err(|error| format!("{error:#}"))?
        }
        Peering::Listen => {
            report(RemoteStatus::Listening { me: me.to_string() });
            let mut incoming = live.transport().incoming_sessions();
            let session = incoming
                .next()
                .await
                .ok_or_else(|| "the transport shut down while waiting".to_string())?;
            tracing::info!("incoming call from {}", session.remote_id());
            Call::accept(session.accept(), broadcast)
                .await
                .map_err(|error| format!("{error:#}"))?
        }
        // `use_remote` routes this case to the loopback and never gets here.
        Peering::Loopback => return Err("no peer to call".to_string()),
    };

    let peer = call.remote_id();
    tracing::info!("connected to {peer}");
    report(RemoteStatus::Connected {
        peer: peer.to_string(),
    });

    // Video only. `RemoteBroadcast::media` would also want an `AudioBackend`,
    // and audio needs its own design -- a jitter buffer, not a slot, because
    // dropping a buffer is audible where dropping a frame is not.
    let video = call
        .remote()
        .video_ready()
        .await
        .map_err(|error| format!("no video from the peer: {error:#}"))?;
    tracing::info!("decoding the peer's video with {}", video.decoder_name());

    drain(video, slot).await;
    Ok(())
}
