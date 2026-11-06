//! The audio half of a call: a microphone out, every other participant's voice in.
//!
//! Each peer gets a sink of their own on one shared playback engine, which mixes
//! them into a single device stream. That single mix is also what the echo
//! canceller subtracts, so one canceller covers everyone in the room.
//!
//! Deliberately **not** a parallel of the video path, and the difference is the
//! whole design. Video is newest-wins from end to end: the capture tap keeps one
//! frame and overwrites it, and the decoder skips a stalled group the instant a
//! newer one exists, because a dropped frame is invisible. Neither is true of
//! audio. A dropped buffer is audible, so playout wants the *next* samples
//! rather than the freshest, and it wants a little padding to ride out jitter.
//!
//! The two streams are otherwise independent: they share a broadcast and a
//! catalog, but nothing aligns them, and no presentation timestamp is consulted
//! on the way out. Audio plays at the pace the output device pulls it and video
//! paints whenever a frame lands.
//!
//! Almost none of the hard part is ours. `moq-audio` ships the jitter buffer,
//! the resampling drift correction, the device lifecycle across stop / replace /
//! failure, and the echo canceller. What is here is the wiring and the handful
//! of settings whose defaults are wrong for a call.

use std::sync::Arc;
use std::time::Duration;

use moq_audio::{capture, decode, encode, playback};
use moq_mux::catalog::Stream as _;

use crate::audio::{Controls, Speaking, Talking, loudness};

/// How far ahead newer audio has to be before a stalled group is abandoned.
///
/// **This has to be set.** The default is `None`, which moq-mux reads as zero
/// and so skips as aggressively as it can -- correct for video, wrong here.
///
/// It is a *skip threshold*, not a playout delay: it costs nothing in the
/// steady state and only bounds how far behind live the consumer will chase a
/// group that has stalled. The padding that actually rides out jitter is
/// `playback::Sink`'s own 50 ms, which is not ours to choose.
const LATENCY_MAX: Duration = Duration::from_millis(200);

/// How many decoded audio frames between rate reports.
///
/// 20 ms a frame, so this is one line every ten seconds -- rarer than the video
/// counters on purpose, because audio arriving at all is the interesting bit and
/// its rate is fixed by the codec rather than by a camera.
const REPORT_EVERY: u32 = 500;

/// Opens the output the user chose, and registers it with `controls` so a
/// choice made mid-call can move it.
///
/// A chosen device that will not open -- unplugged since it was picked -- falls
/// back to the system default with a warning, never to silence. `None` rather
/// than an error when nothing opens at all: a machine with no usable output
/// should still carry a call. The peer simply cannot be heard, which the log
/// says once.
pub(super) async fn open_playback(controls: &Controls) -> Option<playback::Engine> {
    let chosen = controls.output();
    let mut config = playback::Config::default();
    config.device = chosen.clone();
    let (opened, device) = match playback::Engine::open(config).await {
        Err(error) if chosen.is_some() => {
            tracing::warn!("[audio] could not open output {chosen:?}, using the system default: {error:#}");
            (playback::Engine::open(playback::Config::default()).await, None)
        }
        opened => (opened, chosen),
    };
    let engine = match opened {
        Ok(engine) => {
            tracing::info!("[audio] playing through {}", device.as_deref().unwrap_or("the system default output"));
            Some(engine)
        }
        Err(error) => {
            tracing::warn!("[audio] no output device, so the peer will be silent: {error:#}");
            None
        }
    };
    controls.attach_output(engine.as_ref());
    engine
}

/// Registers the microphone track on an existing broadcast and catalog.
///
/// Audio joins the *same* broadcast as video rather than opening a second one,
/// which is why this takes both by value -- `catalog::Producer` is `Clone`, so
/// the caller keeps its own handle for the video rendition.
///
/// The track is registered here, but its catalog rendition describes the PCM
/// layout of the input, which is not known until a device opens. So the
/// broadcast advertises **no audio** until the driver's first successful probe,
/// and capture is on demand: the microphone opens only once a subscriber is
/// listening, and is released when the last one leaves. A peer therefore sees
/// audio appear in the catalog some time after video, which is what
/// [`subscribe`] is built to wait for.
///
/// Returns the driver alongside the control handle. See [`drive`] for why it
/// cannot simply be spawned.
///
/// `engine` is the output the peer is being played through, and it is here for
/// the echo canceller: cancelling an echo means subtracting what the speaker
/// actually emitted, so the canceller has to come from the engine doing the
/// emitting. Without one there is no echo to model and no playback either, so
/// the microphone is published plain.
pub(super) fn publish(
    broadcast: moq_net::broadcast::Producer,
    catalog: moq_mux::catalog::Producer,
    engine: Option<&playback::Engine>,
) -> Result<(encode::Publication, encode::Driver, Option<moq_audio::aec::Control>), String> {
    // Opus at 20 ms, the system default input.
    let mut options = encode::PublicationOptions::default();
    // A clone handed back for `report_aec`: clones share one adaptive filter,
    // so this one sees what the microphone's does.
    let mut canceller = None;
    // **Discontinuous transmission stays off, and that is a decision.** Turning
    // it on is tempting: it saves bandwidth through silence, and it marks that
    // silence on the wire so a listener can tell a quiet microphone from a
    // stalled one for free. We did turn it on, and it was audible -- a soft
    // knock roughly twice a second whenever the other person was not talking,
    // which is Opus refreshing the decoder's comfort-noise model. Every packet
    // is still published either way, so this is not a gap in the stream; it is
    // the periodic real frame among the near-empty ones being heard.
    //
    // Nothing here needs the bandwidth: one peer, one voice. And the talking
    // indicator reads the decoded level instead, which is better anyway,
    // because it works on a sender who has made the opposite choice.
    match engine {
        // Defaults are noise suppression and auto gain both on, which is what a
        // browser gives a call. Costs up to 10 ms of capture latency.
        Some(engine) => {
            match engine.canceller(moq_audio::aec::Config::default()) {
                Ok(control) => {
                    canceller = Some(control);
                    options.capture.aec = canceller.clone();
                }
                // A second canceller on one engine is refused; the microphone
                // then goes out uncancelled, which the log should say.
                Err(error) => tracing::warn!("[audio] no echo cancellation: {error:#}"),
            }
        }
        None => tracing::warn!(
            "[audio] no output device, so nothing to cancel and nothing to cancel it against"
        ),
    }
    // Said out loud because a call on the wrong input and a call on a muted one
    // sound identical from the inside.
    tracing::info!("[audio] publishing {}", describe(&options.capture.source));
    encode::Publication::new(broadcast, catalog, options)
        .map(|(publication, driver)| (publication, driver, canceller))
        .map_err(|error| format!("could not publish the microphone: {error:#}"))
}

/// Logs how well our echo canceller is doing, every few seconds, and keeps the
/// latest numbers in `metrics` for the stats file.
///
/// **The canceller grades itself** (sonora's statistics): how much echo it
/// removes (ERLE), how loud the echo was to begin with (ERL), where it thinks
/// the echo sits in time, and how likely some got through. A call with echo
/// then says whose machine fails to cancel and why, rather than leaving it to
/// guesswork. Runs until the call aborts it.
pub(super) async fn report_aec(
    canceller: moq_audio::aec::Control,
    metrics: Option<Arc<super::Metrics>>,
) {
    let mut ticker = tokio::time::interval(AEC_REPORT_EVERY);
    loop {
        ticker.tick().await;
        let Some(stats) = canceller.statistics() else { continue };
        let snapshot = super::metrics::AecStats::new(&stats, canceller.reference_activity());
        tracing::info!("[aec] {snapshot}");
        if let Some(metrics) = &metrics {
            metrics.set_aec(snapshot);
        }
    }
}

/// How often the echo canceller's numbers are logged. Often enough to see it
/// settle after a join, rare enough not to drown the log.
const AEC_REPORT_EVERY: Duration = Duration::from_secs(5);

/// Runs the capture driver on a thread of its own, until the publication drops.
///
/// **A thread rather than a branch of the call's `select!`, and that is the
/// whole point.** The obvious shape is to await this beside the video encode
/// loop, since a native capture stream is `!Send` and so cannot be handed to
/// `tokio::spawn`, while the call thread reaches its work through
/// `Runtime::block_on`, which imposes no such bound. It compiles, it looks
/// right, and it silently does not work: **the video encode loop never yields
/// while frames are arriving.** Converting a 1080p frame to I420 and encoding it
/// are both synchronous, and the loop's only `.await` is the short sleep it
/// reaches when there is *no* frame waiting. Two futures in one `select!` are
/// one task, so for as long as video has work, this half is never polled at all.
///
/// The symptom is worth writing down, because it does not look like a
/// scheduling problem from the outside. Audio sets up perfectly -- the track
/// publishes, the peer's rendition is found, the device opens -- and then no
/// audio ever arrives. It also looks *fixed* in a release build, where the same
/// work fits inside the gap between camera frames and audio slips through the
/// slack. It is not fixed there; it is unmeasured. A debug build, a larger
/// frame, or a slower machine brings it straight back.
///
/// So the driver gets its own thread and the operating system arbitrates,
/// rather than a cooperative scheduler that video declines to cooperate with.
/// `Publication` is `Send + Sync`, so the controls stay wherever they like; only
/// this end is pinned here.
///
/// Ends when the last [`Publication`](encode::Publication) clone drops, which is
/// what the call does when the attempt finishes.
pub(super) fn drive(driver: encode::Driver) {
    let spawned = std::thread::Builder::new()
        .name("microphone".into())
        .spawn(move || {
            let Some(runtime) = super::runtime::runtime() else {
                tracing::warn!("[audio] no runtime, so the microphone cannot run");
                return;
            };

            runtime.block_on(async move {
                match driver.run().await {
                    Ok(()) => tracing::info!("[audio] the microphone publication ended"),
                    Err(error) => tracing::warn!("[audio] the microphone stopped: {error:#}"),
                }
            });
        });
    if let Err(error) = spawned {
        tracing::warn!("[audio] could not spawn the microphone thread: {error}");
    }
}

/// What decides how loud a peer outside our subroom is: where we are, and the
/// volume chosen for everyone who is somewhere else.
#[derive(Clone)]
pub(super) struct Rooms {
    /// Ourselves, whose `room()` is the subroom we are in.
    pub local: Arc<super::PeerStats>,
    /// Holds the out-of-room volume and its mute.
    pub controls: Arc<Controls>,
}

/// Waits for one peer's audio rendition, decodes it, and plays it.
///
/// Runs until the track closes or the peer leaves, which aborts it. Every
/// failure is terminal *for this peer's audio only* and says so in the log
/// rather than propagating: the call keeps its video.
///
/// `engine` is a clone of the call's one engine -- clones share the device and
/// its mix -- so several of these run at once, one per peer.
pub(super) async fn subscribe(
    broadcast: moq_net::broadcast::Consumer,
    peer: String,
    engine: playback::Engine,
    // Their tile's talking dot, the audio counters for the stats file, and
    // the subroom they say they are in.
    stats: Arc<super::PeerStats>,
    // `None` plays them at full volume always: the loopback has no presence,
    // so no room to be outside of.
    rooms: Option<Rooms>,
) {
    let mut catalog = match moq_mux::catalog::Consumer::<()>::new(
        &broadcast,
        moq_mux::catalog::CatalogFormat::Hang,
    )
    .await
    {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::error!("[audio] no catalog from {peer}: {error:#}");
            return;
        }
    };

    let (name, config) = loop {
        match catalog.next().await {
            Ok(Some(snapshot)) => {
                if let Some((name, config)) = snapshot.audio.renditions.iter().next() {
                    break (name.clone(), config.clone());
                }
                tracing::debug!("[audio] {peer} has no audio rendition yet; waiting");
            }
            Ok(None) => {
                tracing::warn!("[audio] {peer}'s catalog closed before it offered audio");
                return;
            }
            Err(error) => {
                tracing::error!("[audio] could not read {peer}'s catalog: {error:#}");
                return;
            }
        }
    };

    let mut consumer =
        match decode::Consumer::new(&broadcast, &config, &name, decode_config()).await {
            Ok(consumer) => consumer,
            Err(error) => {
                tracing::error!("[audio] no decoder for {peer}'s audio: {error:#}");
                return;
            }
        };

    let mut input = playback::Input::default();
    input.format = moq_audio::Format::F32;
    input.sample_rate = consumer.sample_rate();
    input.layout = consumer.layout();
    let mut sink = match engine.sink(input) {
        Ok(sink) => sink,
        Err(error) => {
            tracing::error!("[audio] could not add {peer} to the mix: {error:#}");
            return;
        }
    };
    tracing::info!(
        "[audio] playing {peer} at {} Hz, {} channel(s)",
        consumer.sample_rate(),
        consumer.layout().channels()
    );

    let mut warned = false;
    let mut decoded = 0u32;
    let mut since = std::time::Instant::now();
    let mut talking = Talking::default();
    // Released when this task ends *or is aborted*, which is how a peer leaving
    // mid-word stops counting as someone talking.
    let mut speaking = Speaking::new(stats.talking_flag());
    // When the last frame came, to spot a gap a listener would hear.
    let mut last_frame: Option<std::time::Instant> = None;
    // The sink's own default. A peer already outside our subroom is turned down
    // below before their first frame is written.
    let mut volume = 1.0f32;

    while let Ok(Some(frame)) = consumer.read().await {
        stats.audio_frame();
        let now = std::time::Instant::now();
        if last_frame.is_some_and(|last| now - last > super::metrics::AUDIO_GAP) {
            stats.audio_gap();
        }
        // Checked before this frame refills it: an empty buffer with frames
        // still arriving is playback that went silent while it should not have.
        // moq-audio counts no underruns itself, so this is the closest there is.
        // Not on the first frame, whose buffer is empty because nothing has
        // played yet -- counted, that read as one dropout per stream.
        if last_frame.is_some() && sink.buffered().is_zero() {
            stats.audio_ran_dry();
        }
        last_frame = Some(now);
        speaking.set(talking.push(loudness(&frame.data)));
        decoded += 1;
        if decoded == REPORT_EVERY {
            let elapsed = since.elapsed().as_secs_f32();
            if elapsed > 0.0 {
                tracing::debug!(
                    "[audio] {:.1} frames/s decoded from {peer}",
                    decoded as f32 / elapsed
                );
            }
            decoded = 0;
            since = std::time::Instant::now();
        }
        // **Derived on every frame, never set on a change.** The volume follows
        // from where they are and where we are *now*, so there is no "they
        // left" or "I joined" event whose loss could leave someone in another
        // subroom at full volume. Any change -- their presence, our click, the
        // slider, a reconnect -- lands on the next frame, about 20 ms later.
        // Applied after `loudness` above, so their talking dot is unaffected,
        // and the quieter audio still goes into the mix the echo canceller
        // hears.
        let wanted = match &rooms {
            Some(rooms) if stats.room() != rooms.local.room() => rooms.controls.crosstalk_volume(),
            _ => 1.0,
        };
        if wanted != volume {
            sink.set_volume(wanted);
            volume = wanted;
        }
        if let Err(error) = sink.write(&frame.data) {
            if !warned {
                tracing::warn!("[audio] playback is dropping samples: {error:#}");
                warned = true;
            }
        }
    }
    tracing::info!("[audio] {peer}'s audio track ended");
}

/// The decoder settings, which differ from the defaults in exactly one place.
///
/// Built field by field because [`decode::Options`] is `#[non_exhaustive]`, so
/// there is no struct literal to write. `max_age` is what moq-audio 0.1 calls
/// the old `latency_max`, with the same trap: its default of zero skips a
/// stalled group the moment a newer one exists.
fn decode_config() -> decode::Options {
    let mut options = decode::Options::default();
    options.max_age = LATENCY_MAX;
    options
}

/// Names the input a config will open, for the log.
fn describe(source: &capture::Source) -> String {
    match source {
        capture::Source::Microphone(None) => "the system default input".to_string(),
        capture::Source::Microphone(Some(id)) => id.clone(),
        capture::Source::System => "system audio".to_string(),
        other => format!("{other:?}"),
    }
}
