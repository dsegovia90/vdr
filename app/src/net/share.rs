//! Sharing a screen: a display or a window, captured, encoded, and published as
//! a second broadcast beside the camera.
//!
//! **Its own broadcast, `<id>/screen.hang`, never a track in the camera's.** A
//! screen is a second source from one person, and receivers tell the two apart
//! by path before they open anything (`call::sharer_of`).
//!
//! **Its own thread.** A capture stream is `!Send` on macOS (it keeps the
//! ScreenCaptureKit session alive), and encoding is synchronous, so the whole
//! share runs on a thread of its own under `runtime().block_on`, as the
//! microphone does. Inside the camera's encode loop it would never be polled:
//! that loop does not yield while frames arrive.
//!
//! **Not moq-video's `publish_capture`.** That releases the source whenever no
//! one is subscribed and opens it again on demand, which is a second open per
//! share (and on Wayland, a bet that the portal restores the grant without a
//! second picker). It also counts its keyframe interval in frames and has no
//! keyframe for a rejoining session. The loop here keeps the source open for
//! exactly as long as the person shares.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{oneshot, watch};

use super::call::{Keyframes, SCREEN_PRIORITY, screen_path, video_track};
use super::metrics::PeerStats;
use super::runtime::runtime;

/// What to share. Ours, so the interface never names a moq type.
#[derive(Clone, Debug, PartialEq)]
pub enum ShareSource {
    /// A whole display; `None` is the main one, or on Wayland whatever the
    /// system's own picker is given.
    Display(Option<String>),
    /// One window, by the id the listing gave it.
    Window(String),
}

impl ShareSource {
    /// A stable string for a menu row's value: the kind and the id.
    pub fn key(&self) -> String {
        match self {
            Self::Display(None) => "display".to_string(),
            Self::Display(Some(id)) => format!("display:{id}"),
            Self::Window(id) => format!("window:{id}"),
        }
    }

    fn capture(&self) -> moq_video::capture::Source {
        match self {
            Self::Display(id) => moq_video::capture::Source::Display(id.clone()),
            Self::Window(id) => moq_video::capture::Source::Window(id.clone()),
        }
    }
}

/// What we want to share, `None` for nothing: written by the bar, served by
/// each call attempt, and set back to `None` by a share that ends by itself.
///
/// A watch, not a queue: only the latest wish matters, and every attempt needs
/// to read it.
#[derive(Clone)]
pub struct ShareWish(pub(super) Arc<watch::Sender<Option<ShareSource>>>);

impl ShareWish {
    pub fn new() -> Self {
        Self(Arc::new(watch::Sender::new(None)))
    }

    /// What we currently want shared.
    pub fn get(&self) -> Option<ShareSource> {
        self.0.borrow().clone()
    }

    /// Asks for `source` to be shared, replacing any share; `None` stops.
    pub fn set(&self, source: Option<ShareSource>) {
        self.0.send_replace(source);
    }
}

impl PartialEq for ShareWish {
    /// Identity, never contents -- see `audio::Controls` for why.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// One entry in the share menu.
#[derive(Clone, Debug, PartialEq)]
pub struct ShareOption {
    pub source: ShareSource,
    pub label: String,
}

/// Displays and windows to offer, or an empty list where the system picks.
///
/// **Blocking**, and it runs the runtime itself: call it from a thread of your
/// own, never the interface's or one of the runtime's. On macOS listing needs
/// the screen-recording grant, so the first call can put up the system's
/// permission alert -- which is why the bar calls this when its menu is first
/// opened, not when a call starts.
pub fn sources() -> (Vec<ShareOption>, Vec<ShareOption>) {
    if !lists() {
        return (Vec::new(), Vec::new());
    }
    let Some(runtime) = runtime() else {
        return (Vec::new(), Vec::new());
    };
    runtime.block_on(async {
        let displays = match moq_video::capture::displays().await {
            Ok(displays) => displays,
            Err(error) => {
                tracing::warn!("[share] could not list displays: {error:#}");
                Vec::new()
            }
        };
        let windows = match moq_video::capture::windows().await {
            Ok(windows) => windows,
            Err(error) => {
                tracing::warn!("[share] could not list windows: {error:#}");
                Vec::new()
            }
        };
        let displays = displays
            .into_iter()
            .map(|display| ShareOption {
                label: format!("{} \u{b7} {}\u{d7}{}", display.name, display.width, display.height),
                source: ShareSource::Display(Some(display.id)),
            })
            .collect();
        let windows = windows
            .into_iter()
            // Sharing the call's own window shows the call inside itself, a
            // tunnel of mirrors, to everyone in it.
            .filter(|window| !is_ours(&window.app))
            .map(|window| ShareOption {
                label: window_label(&window.app, &window.title),
                source: ShareSource::Window(window.id),
            })
            .collect();
        (displays, windows)
    })
}

/// Whether this build can share a screen here at all. Not on Wayland without
/// the `wayland-share` feature: capture would fall through to X11, which sees
/// only what XWayland draws, so a share would show the wrong thing rather than
/// fail.
pub fn available() -> bool {
    shares(
        cfg!(target_os = "linux"),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        cfg!(feature = "wayland-share"),
    )
}

fn shares(linux: bool, wayland: bool, portal: bool) -> bool {
    !(linux && wayland) || portal
}

/// Whether [`sources`] has anything to offer here, which decides whether the
/// bar shows a ▾ at all. Cheap: an environment test, no listing.
pub fn lists() -> bool {
    lists_sources(cfg!(target_os = "linux"), std::env::var_os("WAYLAND_DISPLAY").is_some())
}

/// Whether this platform gives us a list to offer. **Not on Wayland:** there
/// `displays()` lists XWayland's outputs, and an `x11:` id would capture only
/// what XWayland draws. The portal's own picker chooses instead. Same test as
/// moq-video's (`WAYLAND_DISPLAY` set).
fn lists_sources(linux: bool, wayland: bool) -> bool {
    !(linux && wayland)
}

/// Whether a window belongs to this app: "Vdr" as a bundle, "vdr" from a
/// terminal.
fn is_ours(app: &str) -> bool {
    app.eq_ignore_ascii_case("vdr")
}

/// How a window reads in the menu: its app and its title, short enough for
/// one line.
fn window_label(app: &str, title: &str) -> String {
    const MAX: usize = 48;
    let full = match (app.is_empty(), title.is_empty()) {
        (_, true) => app.to_string(),
        (true, false) => title.to_string(),
        (false, false) => format!("{app} \u{2014} {title}"),
    };
    if full.chars().count() <= MAX {
        full
    } else {
        let cut: String = full.chars().take(MAX - 1).collect();
        format!("{cut}\u{2026}")
    }
}

/// The share's frame rate. Text needs sharpness more than motion, and macOS
/// sends fewer frames while the screen is still, so "low, rising with motion"
/// happens without logic of ours.
const SHARE_FPS: u32 = 15;

/// Bits per pixel per frame: twice the camera's 0.05, at half its frame rate,
/// so each frame gets four times the camera's bits. This is the share's
/// quality floor -- moq-video's encoder config has no quality or QP knob, so
/// bits per frame is the only floor there is.
const SHARE_BITS_PER_PIXEL: f64 = 0.1;

/// The share's bitrate bounds. It does not follow the link: the rate loop owns
/// one target for one encoder, and dividing an uplink between two belongs with
/// simulcast. Lowering the ceiling is the cheap fix if it congests.
const SHARE_MIN_BPS: u64 = 600_000;
const SHARE_MAX_BPS: u64 = 2_500_000;

/// The largest size we encode. A 4K monitor would otherwise ask the software
/// encoder for 8-megapixel frames.
const MAX_SHARE: (u32, u32) = (2560, 1440);

/// How often a still screen is re-sent. macOS sends nothing while nothing
/// changes, so without this a joiner and the decoder's live edge would wait
/// for the next change. One near-empty frame a second is the price.
const HEARTBEAT: Duration = Duration::from_secs(1);

/// The keyframe interval. Longer than the camera's two seconds: a screen is
/// mostly still, and a keyframe of a full screen is large.
const KEYFRAME_EVERY: Duration = Duration::from_secs(4);

/// How long a stop may take before the leave goes on without it.
pub(super) const STOP_WITHIN: Duration = Duration::from_millis(500);

/// A source that ends twice within this is gone, not resizing.
const REOPEN_WINDOW: Duration = Duration::from_secs(2);

/// The share's frame rate as the encoder takes it.
fn share_rate() -> moq_video::Rate {
    moq_video::Rate::new(SHARE_FPS, 1).expect("a positive whole frame rate")
}

/// The size to encode a `width` x `height` source at: its own size, divided by
/// the smallest whole factor that brings it within [`MAX_SHARE`], rounded down
/// to even (4:2:0 halves each axis).
fn share_size(width: u32, height: u32) -> (u32, u32) {
    let (max_w, max_h) = MAX_SHARE;
    let factor = width.div_ceil(max_w).max(height.div_ceil(max_h)).max(1);
    let even = |value: u32| ((value / factor) & !1).max(2);
    (even(width), even(height))
}

/// The share's bitrate at a size: [`SHARE_BITS_PER_PIXEL`] at [`SHARE_FPS`],
/// within the bounds.
fn share_bitrate(width: u32, height: u32) -> u64 {
    let bits = width as f64 * height as f64 * SHARE_FPS as f64 * SHARE_BITS_PER_PIXEL;
    (bits as u64).clamp(SHARE_MIN_BPS, SHARE_MAX_BPS)
}

/// The encoder config for a share at `width` x `height`, keeping `kind` (the
/// probe's choice) when given one.
fn encoder_config(width: u32, height: u32, kind: Option<moq_video::encode::Kind>) -> moq_video::encode::Config {
    let mut config = moq_video::encode::Config::new(width, height, share_rate());
    config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(share_bitrate(width, height)));
    config.gop = moq_video::encode::Gop::keyframe_every(KEYFRAME_EVERY, share_rate());
    if let Some(kind) = kind {
        config.kind = kind;
    }
    config
}

/// What went wrong, in words for the bar.
fn problem(error: &moq_video::Error, macos: bool) -> String {
    match error {
        moq_video::Error::PermissionDenied(_) if macos => "Screen recording is off for Vdr: turn it on in System Settings \u{203a} Privacy & Security \u{203a} Screen & System Audio Recording, then reopen Vdr".to_string(),
        // The portal's refusal: the person closed or cancelled its dialog.
        moq_video::Error::PermissionDenied(_) => "Screen sharing was cancelled".to_string(),
        other => {
            let text = other.to_string();
            let short: String = text.chars().take(80).collect();
            format!("Could not share the screen: {short}")
        }
    }
}

/// Everything a share needs from its call attempt.
#[derive(Clone)]
pub(super) struct Call {
    /// Our outbound origin, where the screen's broadcast is created.
    pub outbound: moq_net::origin::Producer,
    /// Our endpoint id, which names the broadcast.
    pub me: String,
    /// Ourselves: `sharing` while we publish, and why a share failed.
    pub local: Arc<PeerStats>,
    /// Set when a session comes back; the screen's flag is ours to take.
    pub keyframe: Arc<Keyframes>,
}

/// One share in progress, from the serving task's side.
struct Running {
    source: ShareSource,
    /// Tells the share thread to stop. Dropping it says the same.
    stop: oneshot::Sender<()>,
    /// Resolves (sent or dropped) once the share has finished its media and
    /// closed its broadcast.
    done: oneshot::Receiver<()>,
}

impl Running {
    /// Stops the share and waits for it to finish, within [`STOP_WITHIN`].
    async fn stop(self) {
        let Running { source, stop, done } = self;
        let _ = stop.send(());
        if tokio::time::timeout(STOP_WITHIN, done).await.is_err() {
            tracing::warn!("[share] {} did not stop within {STOP_WITHIN:?}", source.key());
        }
    }
}

/// Serves the person's wish to share for one call attempt: starts a share when
/// `wanted` holds a source, replaces it when the source changes, stops it on
/// `None`, and stops it on `quit` before returning.
///
/// **One share per person.** Picking another source replaces the current one;
/// several people sharing at once is several of these.
///
/// A share that ends by itself -- the source closed, the system's own "stop
/// sharing", a refused permission -- sets `wanted` back to `None`, so the
/// button shows it has ended.
pub(super) async fn serve(call: Call, wanted: ShareWish, mut quit: oneshot::Receiver<()>) {
    let wanted = wanted.0;
    let mut wishes = wanted.subscribe();
    let mut running: Option<Running> = None;
    loop {
        let wish = wishes.borrow_and_update().clone();
        if wish != running.as_ref().map(|running| running.source.clone()) {
            if let Some(previous) = running.take() {
                previous.stop().await;
            }
            if let Some(source) = wish {
                running = start(source, call.clone());
            }
        }
        let ended = async {
            match running.as_mut() {
                Some(running) => {
                    let _ = (&mut running.done).await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = &mut quit => break,
            changed = wishes.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            () = ended => {
                if let Some(ended) = running.take() {
                    // Only if it is still the wish: a newer pick stands.
                    wanted.send_if_modified(|wish| {
                        let same = wish.as_ref() == Some(&ended.source);
                        if same {
                            *wish = None;
                        }
                        same
                    });
                }
            }
        }
    }
    if let Some(running) = running.take() {
        running.stop().await;
    }
}

/// Starts a share on its own thread.
fn start(source: ShareSource, call: Call) -> Option<Running> {
    let (stop, stopped) = oneshot::channel();
    let (finished, done) = oneshot::channel();
    let spawned = std::thread::Builder::new().name("share".into()).spawn({
        let source = source.clone();
        move || {
            let Some(runtime) = runtime() else {
                tracing::warn!("[share] no runtime, so the screen cannot be shared");
                return;
            };
            runtime.block_on(share(source, call, stopped));
            let _ = finished.send(());
        }
    });
    match spawned {
        Ok(_) => Some(Running { source, stop, done }),
        Err(error) => {
            tracing::warn!("[share] could not spawn the share thread: {error}");
            None
        }
    }
}

/// Opens the capture, or reports why not. `None` when stopped first or refused.
async fn open(
    source: &ShareSource,
    stop: &mut oneshot::Receiver<()>,
    local: &PeerStats,
) -> Option<moq_video::capture::Stream> {
    let mut config = moq_video::capture::Config::default();
    config.source = source.capture();
    config.framerate = Some(share_rate());
    config.cursor = true;
    // Raced against the stop: on macOS the open can sit behind the permission
    // alert, on Wayland behind the portal's picker.
    tokio::select! {
        _ = stop => None,
        opened = moq_video::capture::open(&config) => match opened {
            Ok(stream) => Some(stream),
            Err(error) => {
                tracing::warn!("[share] could not open {}: {error:#}", source.key());
                local.set_share_problem(Some(problem(&error, cfg!(target_os = "macos"))));
                None
            }
        },
    }
}

/// One share, start to finish, on the share thread.
async fn share(source: ShareSource, call: Call, mut stop: oneshot::Receiver<()>) {
    let Call {
        outbound,
        me,
        local,
        keyframe,
    } = call;
    local.set_share_problem(None);
    let Some(mut stream) = open(&source, &mut stop, &local).await else {
        return;
    };
    let (width, height) = share_size(stream.width(), stream.height());
    let mut config = encoder_config(width, height, None);

    let mut broadcast = match outbound.create_broadcast(screen_path(&me)) {
        Ok(broadcast) => broadcast,
        Err(error) => {
            tracing::warn!("[share] could not create the screen's broadcast: {error:#}");
            local.set_share_problem(Some(format!("Could not share the screen: {error}")));
            return;
        }
    };
    let (_catalog, mut producer) = match video_track(&mut broadcast, &mut config, SCREEN_PRIORITY).await {
        Ok(parts) => parts,
        Err(error) => {
            tracing::warn!("[share] {error}");
            local.set_share_problem(Some(format!("Could not share the screen: {error}")));
            broadcast.close();
            return;
        }
    };
    // The seam for the shared screen's own sound, which is not sent yet: it
    // would be published here, into this broadcast and its catalog, the way
    // `audio::publish` does the microphone beside the camera.
    let _audio: Option<moq_audio::encode::Publication> = None;
    let mut encoder = match moq_video::encode::Encoder::new(&config) {
        Ok(encoder) => encoder,
        Err(error) => {
            tracing::warn!("[share] could not build an encoder: {error:#}");
            local.set_share_problem(Some(problem(&error, false)));
            broadcast.close();
            return;
        }
    };
    if let Err(error) = broadcast.announce(moq_net::origin::Route::default()) {
        tracing::warn!("[share] could not announce the screen: {error:#}");
        broadcast.close();
        return;
    }
    // A keyframe asked for before this share began is already answered: a
    // new encoder's first picture is one.
    keyframe.screen.store(false, std::sync::atomic::Ordering::Relaxed);
    local.set_sharing(true);
    tracing::info!(
        "[share] sharing {} at {width}x{height}, {} kbps, with {}",
        stream.label(),
        share_bitrate(width, height) / 1000,
        encoder.name()
    );

    let outcome = pump(&source, &mut stream, &mut encoder, &mut producer, &mut config, Steering {
        keyframe: &keyframe,
        stop: &mut stop,
        local: &local,
    })
    .await;
    match &outcome {
        Ok(()) => tracing::info!("[share] stopped sharing {}", source.key()),
        Err(error) => tracing::warn!("[share] the share ended: {error}"),
    }
    drop(stream);

    // Media finished, then presence, then the broadcast, so the room hears
    // "stopped sharing" and loses the screen together.
    if let Ok(flushed) = encoder.flush() {
        if !flushed.is_empty() {
            producer.publish(&flushed).ok();
        }
    }
    producer.finish().ok();
    local.set_sharing(false);
    broadcast.close();
}

/// What steers the share loop from outside it.
struct Steering<'a> {
    keyframe: &'a Keyframes,
    stop: &'a mut oneshot::Receiver<()>,
    local: &'a PeerStats,
}

/// Reads, encodes and publishes until stopped or the source is gone.
///
/// `Ok` for an ending that is nobody's fault: a stop, or the source going away
/// (a closed window, the system's own "stop sharing"). `Err` for a failure.
async fn pump(
    source: &ShareSource,
    stream: &mut moq_video::capture::Stream,
    encoder: &mut moq_video::encode::Encoder,
    producer: &mut moq_video::encode::Producer,
    config: &mut moq_video::encode::Config,
    steering: Steering<'_>,
) -> Result<(), String> {
    let Steering { keyframe, stop, local } = steering;
    // **Our own clock**, not the capture's: a reopened capture restarts its
    // timeline, and the stream's timestamps must keep rising across that.
    let started = Instant::now();
    // The last frame sent, re-sent with a fresh time when the screen is still.
    let mut last: Option<moq_video::Frame> = None;
    let mut reopened: Option<Instant> = None;
    let mut sent = 0u32;
    let mut repeats = 0u32;
    let mut since = Instant::now();
    loop {
        if keyframe.screen.swap(false, std::sync::atomic::Ordering::Relaxed) {
            if let Err(error) = encoder.cut() {
                tracing::warn!("[share] could not ask the encoder for a keyframe: {error:#}");
            }
        }
        let read = tokio::select! {
            _ = &mut *stop => return Ok(()),
            read = tokio::time::timeout(HEARTBEAT, stream.read()) => read,
        };
        let mut frame = match read {
            Ok(Ok(Some(frame))) => frame,
            Err(_still) => match last.take() {
                Some(frame) => {
                    repeats += 1;
                    frame
                }
                None => continue,
            },
            // A benign end (a resize, a renegotiation): reopen once, the same
            // source, on the same broadcast.
            Ok(Ok(None)) => {
                if reopened.is_some_and(|at| at.elapsed() < REOPEN_WINDOW) {
                    return Ok(());
                }
                reopened = Some(Instant::now());
                tracing::info!("[share] {} ended; reopening it", source.key());
                let Some(fresh) = open(source, stop, local).await else {
                    return Ok(());
                };
                *stream = fresh;
                last = None;
                let (width, height) = share_size(stream.width(), stream.height());
                if (width, height) != (config.width, config.height) {
                    // A new size is a new encoder on the same track: the
                    // catalog follows the new parameter sets by itself.
                    if let Ok(flushed) = encoder.flush() {
                        if !flushed.is_empty() {
                            producer.publish(&flushed).ok();
                        }
                    }
                    *config = encoder_config(width, height, Some(config.kind.clone()));
                    *encoder = moq_video::encode::Encoder::new(config)
                        .map_err(|error| format!("could not rebuild the encoder: {error:#}"))?;
                    tracing::info!("[share] source now {width}x{height}");
                }
                continue;
            }
            Ok(Err(error)) => {
                tracing::info!("[share] the source is gone: {error:#}");
                return Ok(());
            }
        };
        if frame.size() != encoder.size() {
            frame = frame
                .resize(encoder.size(), &moq_video::resize::Config::default())
                .map_err(|error| format!("could not scale the screen: {error:#}"))?;
        }
        frame.timestamp = moq_net::Timestamp::from_micros(started.elapsed().as_micros() as u64)
            .map_err(|error| format!("timestamp out of range: {error:#}"))?;
        let encoded = encoder
            .encode(&frame)
            .map_err(|error| format!("encode failed: {error:#}"))?;
        if !encoded.is_empty() {
            producer
                .publish(&encoded)
                .map_err(|error| format!("publish failed: {error:#}"))?;
        }
        last = Some(frame);
        sent += 1;
        if since.elapsed() >= Duration::from_secs(10) {
            tracing::debug!(
                "[share] {:.1} fps sent ({repeats} of {sent} repeats of a still screen)",
                sent as f32 / since.elapsed().as_secs_f32()
            );
            sent = 0;
            repeats = 0;
            since = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retina_laptop_is_shared_at_its_own_size() {
        assert_eq!(share_size(1710, 1106), (1710, 1106));
    }

    #[test]
    fn a_4k_display_is_halved() {
        assert_eq!(share_size(3840, 2160), (1920, 1080));
        // 5K halves exactly to the cap; a 6K one needs a third.
        assert_eq!(share_size(5120, 2880), (2560, 1440));
        assert_eq!(share_size(6016, 3384), (2004, 1128));
    }

    #[test]
    fn sizes_are_always_even_and_never_zero() {
        for (w, h) in [(1711, 1107), (3, 3), (1, 1), (0, 0), (2561, 1441), (7680, 4320)] {
            let (sw, sh) = share_size(w, h);
            assert!(sw % 2 == 0 && sh % 2 == 0, "{w}x{h} gave {sw}x{sh}");
            assert!(sw >= 2 && sh >= 2, "{w}x{h} gave {sw}x{sh}");
            assert!(sw <= MAX_SHARE.0 && sh <= MAX_SHARE.1, "{w}x{h} gave {sw}x{sh}");
        }
    }

    #[test]
    fn the_bitrate_is_clamped_at_both_ends() {
        assert_eq!(share_bitrate(1710, 1106), SHARE_MAX_BPS);
        assert_eq!(share_bitrate(1280, 800), 1_536_000);
        assert_eq!(share_bitrate(320, 200), SHARE_MIN_BPS);
        assert_eq!(share_bitrate(7680, 4320), SHARE_MAX_BPS);
    }

    #[test]
    fn wayland_offers_no_list() {
        assert!(!lists_sources(true, true));
        assert!(lists_sources(true, false), "X11 lists");
        assert!(lists_sources(false, false));
        // WAYLAND_DISPLAY means nothing off Linux.
        assert!(lists_sources(false, true));
    }

    #[test]
    fn wayland_shares_only_through_the_portal() {
        assert!(!shares(true, true, false));
        assert!(shares(true, true, true));
        assert!(shares(true, false, false), "X11 shares without it");
        assert!(shares(false, true, false), "WAYLAND_DISPLAY means nothing off Linux");
    }

    #[test]
    fn our_own_windows_are_not_offered() {
        assert!(is_ours("Vdr"));
        assert!(is_ours("vdr"));
        assert!(!is_ours("Safari"));
    }

    #[test]
    fn window_labels_fit_one_line() {
        assert_eq!(window_label("Safari", "Docs"), "Safari \u{2014} Docs");
        assert_eq!(window_label("Finder", ""), "Finder");
        let long = window_label("Code", &"x".repeat(100));
        assert_eq!(long.chars().count(), 48);
        assert!(long.ends_with('\u{2026}'));
    }

    #[test]
    fn keys_tell_sources_apart() {
        let keys = [
            ShareSource::Display(None).key(),
            ShareSource::Display(Some("1".into())).key(),
            ShareSource::Window("1".into()).key(),
        ];
        assert_eq!(keys.iter().collect::<std::collections::HashSet<_>>().len(), 3);
    }

    #[test]
    fn problems_are_sentences() {
        let denied = moq_video::Error::PermissionDenied("x".into());
        assert!(problem(&denied, true).starts_with("Screen recording is off for Vdr"));
        assert_eq!(problem(&denied, false), "Screen sharing was cancelled");
        let gone = moq_video::Error::SourceUnavailable("no window".into());
        assert!(problem(&gone, true).starts_with("Could not share the screen: "));
    }
}
