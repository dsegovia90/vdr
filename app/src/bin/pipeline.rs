//! Headless capture -> encode -> publish -> subscribe -> decode.
//!
//! A measuring harness for the media path, with no UI in the way. Without
//! `--relay` it runs entirely in-process, so a failure has one possible cause;
//! with `--relay` the same frames cross the network and back, and the
//! difference between the two runs separates a broken pipeline from a broken
//! connection.
//!
//! ```sh
//! cargo run --bin pipeline -- --seconds 20
//! cargo run --bin pipeline -- --camera c922 --width 1280 --height 720
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use cameras::{CameraSource, PixelFormat, Resolution, StreamConfig};

/// What the run is here to measure.
#[derive(Default)]
struct Counts {
    captured: AtomicU64,
    encoded: AtomicU64,
    published: AtomicU64,
    decoded: AtomicU64,
    /// Frames the encoder never saw because a newer one replaced them first.
    ///
    /// The equivalent of `FrameTap`'s overwrite count. The buffer between
    /// capture and encoder must be one frame deep and newest-wins: a bounded
    /// queue here drops the *newest* frame instead, so a consumer that parks
    /// resumes on stale frames and paces itself off their timestamps.
    /// `tokio::sync::watch` has the right contract; this counts what it drops.
    overwritten: AtomicU64,
}

impl Counts {
    fn report(&self, elapsed: Duration) {
        let secs = elapsed.as_secs_f64().max(0.001);
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        println!();
        println!("  over {:.1}s", secs);
        println!(
            "  capture   {:>6}  ({:.1} fps)",
            g(&self.captured),
            g(&self.captured) as f64 / secs
        );
        println!(
            "  encode    {:>6}  ({:.1} fps)",
            g(&self.encoded),
            g(&self.encoded) as f64 / secs
        );
        println!(
            "  publish   {:>6}  ({:.1} fps)",
            g(&self.published),
            g(&self.published) as f64 / secs
        );
        println!(
            "  decode    {:>6}  ({:.1} fps)",
            g(&self.decoded),
            g(&self.decoded) as f64 / secs
        );
        println!("  tap overwrites {:>6}", g(&self.overwritten));
        println!();
    }
}

fn main() -> anyhow::Result<()> {
    let mut camera_filter: Option<String> = None;
    let mut width = 1280u32;
    let mut height = 720u32;
    let mut framerate = 30u32;
    let mut seconds = 15u64;
    let mut relay: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().context("flag needs a value");
        match arg.as_str() {
            "--camera" => camera_filter = Some(value()?),
            "--width" => width = value()?.parse()?,
            "--height" => height = value()?.parse()?,
            "--fps" => framerate = value()?.parse()?,
            "--seconds" => seconds = value()?.parse()?,
            // Publish to this relay and subscribe back from it, so the media
            // actually crosses the network.
            "--relay" => relay = Some(value()?),
            other => anyhow::bail!("unknown flag {other}"),
        }
    }

    // `env-filter` is not enabled on tracing-subscriber here, so a fixed level
    // rather than RUST_LOG parsing.
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();

    if relay.is_some() {
        rustls::crypto::aws_lc_rs::default_provider().install_default().ok();
    }

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(run(camera_filter, width, height, framerate, seconds, relay))
}

/// Capture, encode, publish, subscribe and decode, counting frames at each step.
async fn run(
    camera_filter: Option<String>,
    width: u32,
    height: u32,
    framerate: u32,
    seconds: u64,
    relay: Option<String>,
) -> anyhow::Result<()> {
    let counts = Arc::new(Counts::default());

    // ---- capture -----------------------------------------------------------
    let devices = cameras::devices().context("enumerating cameras")?;
    anyhow::ensure!(!devices.is_empty(), "no cameras found");
    let device = match &camera_filter {
        Some(want) => devices
            .iter()
            .find(|d| d.name.to_lowercase().contains(&want.to_lowercase()))
            .with_context(|| format!("no camera matching {want:?}"))?,
        None => &devices[0],
    }
    .clone();
    println!("camera: {}", device.name);

    let config = StreamConfig {
        resolution: Resolution { width, height },
        framerate,
        // Requested, but ignored on macOS: `cameras` hardcodes BGRA in
        // `macos/session.rs` and reads this field nowhere. Both attached
        // cameras are natively NV12, so AVFoundation converts NV12->BGRA and we
        // convert BGRA->I420 below -- twice, for a format no device produces.
        pixel_format: PixelFormat::Nv12,
    };
    let camera = cameras::open_source(CameraSource::Usb(device), config).context("opening camera")?;

    // Newest-wins handoff, one frame deep. A queue here is the bug that
    // presents as a network fault.
    let (tx, mut rx) = tokio::sync::watch::channel::<Option<cameras::Frame>>(None);
    let captured = Arc::clone(&counts);
    let _pump = cameras::pump::spawn(camera, move |frame| {
        captured.captured.fetch_add(1, Ordering::Relaxed);
        let _ = tx.send(Some(frame));
    });

    // The encoder is sized from real dimensions rather than the request.
    rx.changed().await.ok();
    let first = rx.borrow_and_update().clone().context("camera closed before first frame")?;
    let (real_w, real_h) = (first.width, first.height);
    println!(
        "first frame: {real_w}x{real_h} {:?} stride={}",
        first.pixel_format, first.stride
    );

    // ---- encoder + publisher ----------------------------------------------
    let rate = moq_video::Rate::new(framerate, 1).context("a positive frame rate")?;
    let enc_config = moq_video::encode::Config::new(real_w, real_h, rate);
    let rendition = enc_config.probe().await.context("probing encoder")?;
    let mut encoder = moq_video::encode::Encoder::new(&enc_config).context("building encoder")?;
    println!("encoder: {} ({:?})", encoder.name(), encoder.codec());

    let origin = moq_tokio::origin::spawn();
    let mut broadcast = origin
        .create_broadcast("cam.hang")
        .context("creating broadcast")?;
    let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default())
        .context("creating catalog")?;
    let track = broadcast
        .create_track("video.avc3", catalog.track_info(60))
        .context("creating video track")?;
    let mut producer = moq_video::encode::Producer::with_track(track, catalog, rendition.clone())
        .context("creating video producer")?;
    broadcast
        .announce(moq_net::origin::Route::default())
        .context("announcing broadcast")?;

    // ---- subscriber --------------------------------------------------------
    // In-process consumes the producer's own broadcast; with a relay the same
    // broadcast goes out and comes back.
    let (consumer_broadcast, _session) = match &relay {
        None => {
            println!("in-process, no network");
            (broadcast.consume(), None)
        }
        Some(url) => {
            println!("via relay {url}");
            let url: url::Url = url.parse().context("--relay must be a URL")?;

            let client_config = moq_tokio::client::Config::default();
            let mut iroh = moq_tokio::iroh::Config::default();
            iroh.enabled = Some(true);
            let endpoint = iroh
                .bind(&client_config.quic)
                .await
                .context("binding iroh endpoint")?
                .context("iroh disabled")?;
            let client = client_config.init()?.with_iroh(endpoint);

            // Separate origins so the returning broadcast is unambiguously the
            // relay's copy rather than our own.
            let inbound = moq_tokio::origin::spawn();
            let session = client
                .with_publisher(&origin)
                .with_subscriber(inbound.clone())
                .connect(url);

            // Wait for the relay to announce our broadcast back to us.
            let mut announced = inbound.consume().announced();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            let found = loop {
                let next = tokio::time::timeout_at(deadline, announced.next()).await;
                match next {
                    Err(_) => anyhow::bail!("relay never announced our broadcast back"),
                    Ok(None) => anyhow::bail!("announce stream ended"),
                    Ok(Some(update)) => {
                        // Ours, not merely the first announced: a relay host
                        // publishes its own broadcasts too, and pointing the
                        // decoder at one of those reports a missing track.
                        let path = update.prefix.to_string();
                        let is_ours = path.rsplit('/').next() == Some("cam.hang");
                        if is_ours && update.kind == moq_net::announce::Kind::Announced {
                            println!("relay announced our broadcast back: {path}");
                            break inbound
                                .consume()
                                .request_broadcast(path.as_str())
                                .await
                                .context("opening our broadcast from the relay")?;
                        }
                        tracing::debug!(%path, "ignoring announce (not ours)");
                    }
                }
            };
            (found, Some(session))
        }
    };
    let decoded = Arc::clone(&counts);
    let decode_task = tokio::spawn(async move {
        let mut consumer = match moq_video::decode::Consumer::new(
            &consumer_broadcast,
            &rendition,
            "video.avc3",
            moq_video::decode::Options::default(),
        )
        .await
        {
            Ok(c) => c,
            Err(err) => {
                tracing::error!(?err, "decoder failed to open");
                return;
            }
        };
        println!("decoder: {}", consumer.name());
        loop {
            match consumer.read().await {
                Ok(Some(_frame)) => {
                    decoded.decoded.fetch_add(1, Ordering::Relaxed);
                }
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!(?err, "decode ended");
                    break;
                }
            }
        }
    });

    // ---- the loop ----------------------------------------------------------
    let started = Instant::now();
    let deadline = started + Duration::from_secs(seconds);
    let mut last_seen = counts.captured.load(Ordering::Relaxed);

    while Instant::now() < deadline {
        if rx.changed().await.is_err() {
            break;
        }
        let Some(frame) = rx.borrow_and_update().clone() else {
            continue;
        };

        let seen_now = counts.captured.load(Ordering::Relaxed);
        counts
            .overwritten
            .fetch_add(seen_now.saturating_sub(last_seen + 1), Ordering::Relaxed);
        last_seen = seen_now;

        let surface = to_i420(&frame).context("converting frame")?;
        let ts = moq_net::Timestamp::from_micros(frame.timestamp.as_micros() as u64)
            .context("frame timestamp out of range")?;
        let video_frame = moq_video::Frame::new(surface, ts);

        let encoded = encoder.encode(&video_frame).context("encoding")?;
        counts.encoded.fetch_add(1, Ordering::Relaxed);
        if !encoded.is_empty() {
            producer.publish(&encoded).context("publishing")?;
            counts.published.fetch_add(encoded.len() as u64, Ordering::Relaxed);
        }
    }

    let flushed = encoder.flush().unwrap_or_default();
    if !flushed.is_empty() {
        producer.publish(&flushed).ok();
    }
    producer.finish().ok();

    tokio::time::sleep(Duration::from_millis(500)).await;
    decode_task.abort();

    counts.report(started.elapsed());
    Ok(())
}

/// Convert a captured frame to planar I420.
///
/// **Not via `Surface::rgba`.** That is not a constructor -- it calls
/// `I420::from_rgba` immediately -- and it assumes RGBA channel order, so BGRA
/// input silently comes out with red and blue swapped. `I420::from_nv12` is
/// `pub(crate)`, so the conversion is ours either way.
///
/// `stride` may exceed `width * bytes_per_pixel` because of alignment padding,
/// so every row is indexed through it rather than assumed packed.
fn to_i420(frame: &cameras::Frame) -> anyhow::Result<moq_video::Surface> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    anyhow::ensure!(w >= 2 && h >= 2, "frame too small: {w}x{h}");
    // Odd dimensions have no well-defined 4:2:0 chroma plane.
    let (cw, ch) = (w / 2, h / 2);

    let mut y_plane = vec![0u8; w * h];
    let mut u_plane = vec![0u8; cw * ch];
    let mut v_plane = vec![0u8; cw * ch];

    match frame.pixel_format {
        PixelFormat::Nv12 => {
            // Y plane as-is; UV is interleaved and needs splitting. No colour
            // conversion at all, which is why this format is worth wanting.
            let src = &frame.plane_primary;
            let stride = frame.stride as usize;
            for row in 0..h {
                let from = row * stride;
                anyhow::ensure!(from + w <= src.len(), "luma plane short at row {row}");
                y_plane[row * w..(row + 1) * w].copy_from_slice(&src[from..from + w]);
            }
            let uv = if frame.plane_secondary.is_empty() {
                &src[stride * h..]
            } else {
                &frame.plane_secondary[..]
            };
            let uv_stride = stride;
            for row in 0..ch {
                for col in 0..cw {
                    let i = row * uv_stride + col * 2;
                    anyhow::ensure!(i + 1 < uv.len(), "chroma plane short at {row},{col}");
                    u_plane[row * cw + col] = uv[i];
                    v_plane[row * cw + col] = uv[i + 1];
                }
            }
        }
        PixelFormat::Bgra8 | PixelFormat::Rgba8 => {
            // BT.601 limited range, which is what H.264 expects by default.
            let src = &frame.plane_primary;
            let stride = frame.stride as usize;
            let (ri, bi) = if frame.pixel_format == PixelFormat::Bgra8 {
                (2usize, 0usize)
            } else {
                (0usize, 2usize)
            };
            let at = |x: usize, y: usize| -> (f32, f32, f32) {
                let p = y * stride + x * 4;
                (src[p + ri] as f32, src[p + 1] as f32, src[p + bi] as f32)
            };
            anyhow::ensure!(src.len() >= stride * h, "colour plane short");
            for y in 0..h {
                for x in 0..w {
                    let (r, g, b) = at(x, y);
                    y_plane[y * w + x] =
                        (0.257 * r + 0.504 * g + 0.098 * b + 16.0).clamp(0.0, 255.0) as u8;
                }
            }
            // Average each 2x2 block before converting, rather than point
            // sampling: point sampling shimmers on detailed content.
            for cy in 0..ch {
                for cx in 0..cw {
                    let mut rs = 0.0;
                    let mut gs = 0.0;
                    let mut bs = 0.0;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let (r, g, b) = at(cx * 2 + dx, cy * 2 + dy);
                            rs += r;
                            gs += g;
                            bs += b;
                        }
                    }
                    let (r, g, b) = (rs / 4.0, gs / 4.0, bs / 4.0);
                    u_plane[cy * cw + cx] =
                        (-0.148 * r - 0.291 * g + 0.439 * b + 128.0).clamp(0.0, 255.0) as u8;
                    v_plane[cy * cw + cx] =
                        (0.439 * r - 0.368 * g - 0.071 * b + 128.0).clamp(0.0, 255.0) as u8;
                }
            }
        }
        other => anyhow::bail!("no I420 conversion for {other:?} yet"),
    }

    let mut data = y_plane;
    data.extend_from_slice(&u_plane);
    data.extend_from_slice(&v_plane);
    let i420 = moq_video::I420::new(moq_video::Size::new(frame.width, frame.height), data)
        .map_err(|e| anyhow::anyhow!("I420::new: {e}"))?;
    Ok(moq_video::Surface::I420(i420))
}
