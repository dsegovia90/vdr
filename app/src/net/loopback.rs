//! The network path with the network taken out.
//!
//! Camera frames go through a real H.264 encode, a real packet channel and a
//! real decode, and come back out as frames for a second tile. Everything here
//! except `media_pipe` is what a call will use; swapping the pipe for a MoQ
//! session over iroh is the next step, and nothing upstream or downstream of it
//! has to change.
//!
//! What it proves, and the order it proves it in: that the pump's fan-out feeds
//! an encoder without disturbing the preview, that a decoded frame reaches
//! `VideoPaintSource` and paints, and that the two tiles stay in step. Those
//! are the failures worth telling apart before a peer is involved, because over
//! a connection they all look the same from the outside -- a black tile.

use moq_media::codec::{DynamicVideoDecoder, VideoCodec};
use moq_media::format::{DecodeConfig, VideoEncoderConfig, VideoPreset};
use moq_media::pipeline::{PipelineContext, VideoDecoderPipeline, VideoEncoderPipeline};
use moq_media::stats::EncodeOpts;
use moq_media::subscribe::VideoTrack;
use moq_media::traits::{VideoEncoder, VideoSource};
use moq_media::transport::media_pipe;

use super::runtime::runtime;
use super::sink::drain;
use super::source::TapSource;
use crate::camera::{FrameFeed, FrameSlot};

/// What the encoder aims for, before `ScaleMode::Fit` narrows it to the source's
/// aspect ratio.
///
/// 720p and not the capture resolution: a 1080p capture that macOS refuses to
/// narrow (see `camera::format`) should not become a 1080p encode as well. This
/// is where "capture max, send less" actually happens today.
const PRESET: VideoPreset = VideoPreset::P720;

/// Runs a loopback on its own thread for the lifetime of the process.
///
/// Teardown is by cascade rather than by signal: when the last
/// [`FrameTap`](crate::camera::FrameTap) drops, `TapSource::pop_frame` starts
/// erroring, the encoder pipeline ends, its sink closes, the decoder follows,
/// and the drain loop sees the track close and returns.
pub(super) fn spawn(feed: FrameFeed, slot: FrameSlot) {
    let spawned = std::thread::Builder::new()
        .name("loopback".into())
        .spawn(move || run(feed, slot));
    if let Err(error) = spawned {
        tracing::warn!("could not spawn the loopback thread: {error}");
    }
}

/// Owns the pipelines for as long as frames keep coming.
///
/// Both halves must be held here: `VideoEncoderPipeline` cancels its worker on
/// drop, so letting it fall out of scope after construction would encode
/// nothing and leave the tile black with no error anywhere.
fn run(feed: FrameFeed, slot: FrameSlot) {
    let Some(runtime) = runtime() else {
        return;
    };

    // Blocks until the camera produces something, which may be never -- the
    // user can start with the camera off. That is why this is a thread and not
    // part of the hook.
    let Some(source) = TapSource::wait_for_first("loopback", feed) else {
        tracing::debug!("loopback: the camera closed before a frame arrived");
        return;
    };

    let built = {
        // Only construction needs the runtime in context: the decode pipeline
        // spawns a packet-forwarding task. The guard must not still be held at
        // `block_on` below -- tokio refuses to block a thread that has entered
        // a runtime.
        let _guard = runtime.enter();
        build(source)
    };
    let (_encoder, track) = match built {
        Ok(built) => built,
        Err(error) => {
            tracing::error!("loopback: could not build the pipeline: {error:#}");
            return;
        }
    };
    tracing::info!("loopback: decoding with {}", track.decoder_name());

    runtime.block_on(drain(track, slot));
    tracing::info!("loopback: stopped");
}

/// Wires encoder -> pipe -> decoder and returns the two ends to keep alive.
fn build(source: TapSource) -> anyhow::Result<(VideoEncoderPipeline, VideoTrack)> {
    let [width, height] = source.format().dimensions;
    // `ScaleMode::Fit` never upscales, so a camera below the preset is encoded
    // at its own size rather than blown up to 720p and back down for display.
    let target = VideoEncoderConfig::from_preset(PRESET).resolve_for_source(width, height);
    let (encoded_width, encoded_height) = (target.width, target.height);
    let encoder = VideoCodec::H264.create_encoder(target)?;
    tracing::info!(
        "loopback: encoding {width}x{height} as {encoded_width}x{encoded_height} with {}",
        encoder.name(),
    );

    // The decoder is configured from the encoder rather than from a catalog,
    // which is the one thing a call will do differently: there the config
    // arrives over the wire, and the two sides can disagree.
    let config = encoder.config();
    let (sink, packets) = media_pipe(32);
    let encoder = VideoEncoderPipeline::new(source, encoder, sink, EncodeOpts::default());
    let decoder = VideoDecoderPipeline::new::<DynamicVideoDecoder>(
        "loopback".to_string(),
        packets,
        &config,
        &DecodeConfig::default(),
        PipelineContext {
            stats: Default::default(),
            // No audio yet, so there is no clock to sync to; the decoder paces
            // on presentation timestamps instead.
            sync: None,
        },
    )?;
    Ok((encoder, VideoTrack::from_pipeline(decoder)))
}
