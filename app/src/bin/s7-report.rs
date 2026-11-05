//! Reads the stats files of a test call and says whether it met S7.
//!
//! ```sh
//! cargo run --release --bin s7-report -- alice.jsonl bob.jsonl carol.jsonl dave.jsonl
//! ```
//!
//! Each file is what one participant wrote with `vdr --stats`: once a second,
//! a line per person it received and a line about its own link. S7 asks, for
//! every pair of (receiver, sender):
//!
//! - **frame rate:** at least 24 fps (a rolling 5 s mean, less half a frame of
//!   counting slack) in 95% of the seconds the sender's camera was on;
//! - **delay:** the 95th percentile of the per-second p95 under 300 ms;
//! - **audio:** playback never ran dry while the sender's mic was on -- what a
//!   listener actually hears. Late arrivals (more than 60 ms between frames)
//!   are shown too, as jitter, but do not fail: the first runs found every one
//!   absorbed by the playback buffer, with all 50 frames of the second still
//!   arriving.
//!
//! The first 5 s of each stream are skipped (a joiner catches up on a burst of
//! cached frames), and gaps within 2 s of a muted second are skipped too
//! (unmuting resumes the frames after a pause that is not a fault).
//!
//! **What the delay is:** encode to decode, across two machines' clocks. It
//! is as good as their NTP agreement (a few ms), and leaves out the camera
//! sensor and the display, typically 30 to 60 ms more.

use std::collections::BTreeMap;

use serde_json::Value;

const WARMUP_S: f64 = 5.0;
const MIN_FPS: f64 = 24.0;
/// A 1 s frame count jitters by a frame either way, so a camera that delivers
/// exactly 24 fps would "fail" a third of the time. Judged instead on a rolling
/// 5 s mean, with half a frame of slack for that same counting.
const FPS_WINDOW: usize = 5;
const FPS_SLACK: f64 = 0.5;
const FPS_SHARE: f64 = 0.95;
const MAX_DELAY_MS: f64 = 300.0;
const MUTE_MARGIN_S: f64 = 2.0;

/// One second of one participant's link: round trip, and loss.
type LinkSample = (Option<f64>, Option<f64>);

/// One second of one received stream.
struct Second {
    t: f64,
    camera: bool,
    mic: bool,
    fps: Option<f64>,
    delay_p50: Option<f64>,
    delay_p95: Option<f64>,
    audio_gaps: u64,
    audio_dry: u64,
}

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: s7-report <stats.jsonl>...");
        std::process::exit(2);
    }

    let mut streams: BTreeMap<(String, String), Vec<Second>> = BTreeMap::new();
    let mut links: BTreeMap<String, Vec<LinkSample>> = BTreeMap::new();
    for path in &paths {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("cannot read {path}: {error}");
                std::process::exit(2);
            }
        };
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                eprintln!("{path}: skipping a line that is not JSON");
                continue;
            };
            let me = short(value["me"].as_str().unwrap_or("?"));
            if value["link"].as_bool() == Some(true) {
                links
                    .entry(me)
                    .or_default()
                    .push((value["rtt_ms"].as_f64(), value["loss_pct"].as_f64()));
                continue;
            }
            let Some(peer) = value["peer"].as_str() else { continue };
            streams.entry((me, short(peer))).or_default().push(Second {
                t: value["t"].as_f64().unwrap_or(0.0),
                camera: value["camera"].as_bool().unwrap_or(true),
                mic: value["mic"].as_bool().unwrap_or(true),
                fps: value["fps"].as_f64(),
                delay_p50: value["delay_p50_ms"].as_f64(),
                delay_p95: value["delay_p95_ms"].as_f64(),
                audio_gaps: value["audio_gaps"].as_u64().unwrap_or(0),
                audio_dry: value["audio_dry"].as_u64().unwrap_or(0),
            });
        }
    }

    println!(
        "{:<9} {:<9} {:>5} {:>7} {:>7} {:>8} {:>8} {:>6}  verdict",
        "receiver", "sender", "secs", "fps p50", "fps≥24", "delay50", "delay95", "late"
    );
    let mut all_pass = true;
    for ((me, peer), seconds) in &streams {
        let verdict = judge(seconds);
        all_pass &= verdict.pass;
        println!(
            "{:<9} {:<9} {:>5} {:>7} {:>6.0}% {:>8} {:>8} {:>6}  {}",
            me,
            peer,
            verdict.seconds,
            fmt(verdict.fps_p50, 1),
            verdict.fps_share * 100.0,
            fmt(verdict.delay_p50, 0),
            fmt(verdict.delay_p95, 0),
            verdict.gaps,
            verdict.why,
        );
    }

    println!();
    for (me, samples) in &links {
        let rtts: Vec<f64> = samples.iter().filter_map(|(rtt, _)| *rtt).collect();
        let loss = samples.iter().filter_map(|(_, loss)| *loss).fold(0.0_f64, f64::max);
        println!(
            "link {me:<9} rtt p50 {} ms, loss up to {loss:.2}%",
            fmt(percentile(rtts, 0.5), 0)
        );
    }

    println!();
    if streams.is_empty() {
        println!("NO DATA: no received streams in these files.");
        std::process::exit(1);
    }
    println!("S7: {}", if all_pass { "PASS" } else { "FAIL" });
    std::process::exit(if all_pass { 0 } else { 1 });
}

/// The verdict on one received stream.
#[derive(Debug)]
struct Verdict {
    seconds: usize,
    fps_p50: Option<f64>,
    fps_share: f64,
    delay_p50: Option<f64>,
    delay_p95: Option<f64>,
    gaps: u64,
    pass: bool,
    why: String,
}

fn judge(seconds: &[Second]) -> Verdict {
    let start = seconds.first().map_or(0.0, |s| s.t);
    let measured: Vec<&Second> = seconds.iter().filter(|s| s.t - start >= WARMUP_S).collect();

    // Frame rate, only while their camera was on: off is a choice, not a stall.
    let fps: Vec<f64> = measured.iter().filter(|s| s.camera).filter_map(|s| s.fps).collect();
    let smoothed: Vec<f64> = fps
        .windows(FPS_WINDOW.min(fps.len()).max(1))
        .map(|window| window.iter().sum::<f64>() / window.len() as f64)
        .collect();
    let fps_share = if smoothed.is_empty() {
        0.0
    } else {
        smoothed.iter().filter(|&&f| f >= MIN_FPS - FPS_SLACK).count() as f64 / smoothed.len() as f64
    };

    let delay_p50 = percentile(measured.iter().filter_map(|s| s.delay_p50).collect(), 0.5);
    let delay_p95 = percentile(measured.iter().filter_map(|s| s.delay_p95).collect(), 0.95);

    // New gaps per second, except near a muted second.
    let muted: Vec<f64> = seconds.iter().filter(|s| !s.mic).map(|s| s.t).collect();
    let near_mute = |t: f64| muted.iter().any(|&m| (t - m).abs() <= MUTE_MARGIN_S);
    let (mut gaps, mut dry) = (0, 0);
    for pair in measured.windows(2) {
        if near_mute(pair[1].t) {
            continue;
        }
        gaps += pair[1].audio_gaps.saturating_sub(pair[0].audio_gaps);
        dry += pair[1].audio_dry.saturating_sub(pair[0].audio_dry);
    }

    let mut problems = Vec::new();
    if fps.is_empty() {
        problems.push("no video data".to_string());
    } else if fps_share < FPS_SHARE {
        problems.push(format!("fps ≥{MIN_FPS} only {:.0}% of the time", fps_share * 100.0));
    }
    match delay_p95 {
        None => problems.push("no delay data (clocks?)".to_string()),
        Some(p95) if p95 >= MAX_DELAY_MS => problems.push(format!("delay p95 {p95:.0} ms")),
        Some(_) => {}
    }
    if dry > 0 {
        problems.push(format!("audio ran dry {dry}×"));
    }

    Verdict {
        seconds: measured.len(),
        fps_p50: percentile(fps, 0.5),
        fps_share,
        delay_p50,
        delay_p95,
        gaps,
        pass: problems.is_empty(),
        why: if problems.is_empty() { "PASS".to_string() } else { format!("FAIL: {}", problems.join("; ")) },
    }
}

fn percentile(mut values: Vec<f64>, q: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[((values.len() - 1) as f64 * q).round() as usize])
}

fn fmt(value: Option<f64>, decimals: usize) -> String {
    value.map_or("—".to_string(), |v| format!("{v:.decimals$}"))
}

/// Enough of an endpoint id to tell people apart, as the tiles show it.
fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn second(t: f64, fps: f64, p95: f64, gaps: u64, mic: bool) -> Second {
        Second {
            t,
            camera: true,
            mic,
            fps: Some(fps),
            delay_p50: Some(p95 / 2.0),
            delay_p95: Some(p95),
            audio_gaps: gaps,
            audio_dry: gaps,
        }
    }

    #[test]
    fn a_healthy_stream_passes() {
        let seconds: Vec<_> = (0..60).map(|t| second(t as f64, 30.0, 120.0, 0, true)).collect();
        assert!(judge(&seconds).pass);
    }

    #[test]
    fn slow_video_fails() {
        let seconds: Vec<_> = (0..60).map(|t| second(t as f64, 15.0, 120.0, 0, true)).collect();
        let verdict = judge(&seconds);
        assert!(!verdict.pass && verdict.why.contains("fps"), "{}", verdict.why);
    }

    #[test]
    fn a_24_fps_camera_counting_23_or_25_passes() {
        let seconds: Vec<_> = (0..60)
            .map(|t| second(t as f64, [23.0, 24.0, 25.0][t % 3], 120.0, 0, true))
            .collect();
        assert!(judge(&seconds).pass);
    }

    #[test]
    fn high_delay_fails() {
        let seconds: Vec<_> = (0..60).map(|t| second(t as f64, 30.0, 450.0, 0, true)).collect();
        assert!(!judge(&seconds).pass);
    }

    #[test]
    fn the_warmup_burst_is_not_held_against_it() {
        let mut seconds: Vec<_> = (0..60).map(|t| second(t as f64, 30.0, 120.0, 0, true)).collect();
        seconds[1].fps = Some(55.0);
        seconds[2].delay_p95 = Some(900.0);
        assert!(judge(&seconds).pass);
    }

    #[test]
    fn a_gap_on_unmute_is_not_a_dropout() {
        let mut seconds: Vec<_> = (0..60).map(|t| second(t as f64, 30.0, 120.0, 0, true)).collect();
        seconds[20].mic = false;
        for s in seconds.iter_mut().skip(21) {
            s.audio_gaps = 1;
        }
        assert!(judge(&seconds).pass);
    }

    #[test]
    fn late_audio_that_never_ran_dry_passes() {
        let mut seconds: Vec<_> = (0..60).map(|t| second(t as f64, 30.0, 120.0, 0, true)).collect();
        for s in seconds.iter_mut().skip(30) {
            s.audio_gaps = 2;
            s.audio_dry = 0;
        }
        assert!(judge(&seconds).pass);
    }

    #[test]
    fn playback_running_dry_while_talking_fails() {
        let mut seconds: Vec<_> = (0..60).map(|t| second(t as f64, 30.0, 120.0, 0, true)).collect();
        for s in seconds.iter_mut().skip(30) {
            s.audio_dry = 1;
        }
        assert!(!judge(&seconds).pass);
    }

    #[test]
    fn no_clock_means_no_delay_data_not_zero() {
        let seconds: Vec<_> = (0..60)
            .map(|t| Second {
                delay_p50: None,
                delay_p95: None,
                ..second(t as f64, 30.0, 0.0, 0, true)
            })
            .collect();
        let verdict = judge(&seconds);
        assert!(!verdict.pass && verdict.why.contains("no delay data"), "{}", verdict.why);
    }
}
