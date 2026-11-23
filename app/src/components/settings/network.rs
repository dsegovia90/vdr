//! The Network tab: the last minute of what each outgoing stream sent, against
//! its cap and its slice of the link.
//!
//! **One budget, divided.** The transport's estimate for the whole connection
//! is split among the microphone, the screen share and the camera, in that
//! order (`net::adapt`), and each video stream's rate follows its own slice.
//! So each stream gets a chart against its cap and its slice, and while a
//! screen is shared a third adds them up against the whole estimate: that is
//! where it shows whether everything together fit. With only the camera
//! sending it would repeat the camera's chart, so it is not shown.
//!
//! **Drawn as one inline SVG per chart**, stretched to the pane's width: a
//! filled, rounded line for what was sent and plain lines for the cap, the
//! target and the slice (or, for the total, the estimate). Blitz hands the
//! subtree to usvg and re-parses it when it changes, which the icons already
//! rely on (the microphone gains a stroke when muted); here that happens once
//! a second, only while the tab is open. No text inside the SVG, so no fonts
//! are involved: the scale and the numbers are ordinary elements.
//!
//! **The sent line is a few seconds' average, not each second.** An encoder
//! meets its rate over seconds, so the raw figure is a comb of keyframe
//! spikes that crosses the cap constantly and says nothing about the link.
//!
//! **Sampled only while it can be seen.** The history fills whether or not the
//! tab is open (see `rates`); the poller that carries it here runs only while
//! the charts are mounted, which is only while the window is open on this tab,
//! and it sends only when the minute changed. Any wake of the page repaints
//! the whole window.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dioxus_native::prelude::*;

use super::Heading;
use crate::channel::use_channel;
use crate::net::Metrics;
use crate::rates::{self, Point, Second};

/// How often the poller looks. The history moves once a second, so faster
/// would only find the same minute again.
const POLL: Duration = Duration::from_secs(1);

/// The SVG's own coordinate space. It is stretched to the pane's width and
/// drawn [`HEIGHT`] px tall, so vertical units are pixels and horizontal ones
/// are not.
const WIDTH: f32 = 600.0;
const HEIGHT: f32 = 120.0;

/// Plain colours: usvg drops Tailwind's `oklch()` palette (see the icons).
const SENT: &str = "rgb(59,130,246)";
const CAP: &str = "rgb(239,68,68)";
const TARGET: &str = "rgb(251,191,36)";
const LINK: &str = "rgb(52,211,153)";
const GRID: &str = "rgb(38,38,38)";

#[component]
pub fn NetworkTab(metrics: Arc<Metrics>, open: bool) -> Element {
    rsx! {
        div { class: "flex flex-col",
            div { class: "text-lg font-semibold text-neutral-100", "Network" }
            p { class: "mt-1 text-sm text-neutral-400",
                "What your call sent over the last minute, newest at the right. The link's estimate is divided between your microphone, your screen share and your camera, in that order; each video stream follows its slice, and never goes over its cap."
            }
            if open {
                Charts { metrics }
            }
        }
    }
}

#[component]
fn Charts(metrics: Arc<Metrics>) -> Element {
    // Read once here, so a tab opened mid-call shows the last minute at once
    // rather than after the poller's first look.
    let mut minute = use_signal({
        let metrics = metrics.clone();
        move || metrics.last_minute()
    });

    let stop = use_hook(|| Arc::new(AtomicBool::new(false)));
    use_drop({
        let stop = stop.clone();
        move || stop.store(true, Ordering::Relaxed)
    });

    let tx = use_channel(move |next: Vec<Second>| minute.set(next));
    use_hook({
        let metrics = metrics.clone();
        let stop = stop.clone();
        let first = minute.peek().clone();
        move || {
            let spawned = std::thread::Builder::new()
                .name("network-tab".into())
                .spawn(move || {
                    let mut sent = first;
                    loop {
                        std::thread::sleep(POLL);
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        let next = metrics.last_minute();
                        // Only a change is sent: any message wakes the page,
                        // and a woken page repaints the whole window.
                        if next != sent {
                            if tx.unbounded_send(next.clone()).is_err() {
                                return;
                            }
                            sent = next;
                        }
                    }
                });
            if let Err(error) = spawned {
                tracing::warn!("could not spawn the network tab's poller: {error}");
            }
        }
    });

    let seconds = minute();
    let camera: Vec<Point> = seconds.iter().map(Point::camera).collect();
    let share: Vec<Point> = seconds.iter().map(Point::share).collect();
    let total: Vec<Point> = seconds.iter().map(Point::total).collect();

    let has_camera = camera.iter().any(|point| point.cap.is_some());
    // "Shared within the minute", not "sharing now": a share that just ended
    // keeps its chart, and the total, until its seconds have scrolled away.
    let has_share = share.iter().any(|point| point.cap.is_some() || point.sent.is_some());

    if !has_camera && !has_share {
        return rsx! {
            p { class: "mt-6 text-sm text-neutral-300",
                "Nothing to show yet. The charts fill in once a call is sending video."
            }
        };
    }

    rsx! {
        Heading { text: "Camera" }
        if has_camera {
            Chart { points: camera, draw_cap: true, link: SLICE }
        } else {
            p { class: "text-sm text-neutral-300", "The camera is not sending." }
        }
        if has_share {
            Heading { text: "Screen share" }
            Chart { points: share, draw_cap: true, link: SLICE }
            Heading { text: "Everything sent" }
            Chart { points: total, draw_cap: false, link: ESTIMATE }
            p { class: "mt-1 text-xs text-neutral-500", "Video only: audio is not included." }
        }
    }
}

/// What a stream's dashed line is: its own slice of the link.
const SLICE: &str = "slice of the link";
/// What the total's dashed line is: the whole connection's estimate.
const ESTIMATE: &str = "link estimate";

/// One chart: what was sent as a filled line, with the cap, the target and
/// the dashed `link` line (named by `link`) over it.
///
/// Every line follows its own value second by second, so a cap that changed
/// mid-minute (Sharp turned on, the camera reopening at another size) shows
/// where it changed.
#[component]
fn Chart(points: Vec<Point>, draw_cap: bool, link: &'static str) -> Element {
    // Averaged before it is cut to the minute shown, over the few older
    // seconds the history keeps for this, so the left edge is a full average
    // like every other point.
    let raw: Vec<Option<u32>> = points.iter().map(|point| point.sent).collect();
    let points: Vec<Point> = points
        .iter()
        .zip(rates::smoothed(&raw, rates::SMOOTH))
        .map(|(point, sent)| Point { sent, ..*point })
        .collect();
    let slots = rates::slots(&points);
    let of = |pick: fn(&Point) -> Option<u32>| -> Vec<Option<u32>> {
        slots.iter().map(|slot| slot.as_ref().and_then(pick)).collect()
    };
    let sent = of(|point| point.sent);
    // Scaled by what is drawn: the averaged line, not the raw spikes.
    let scaled: Vec<Point> = slots.iter().flatten().copied().collect();
    let top = rates::top(&scaled);
    let canvas = rates::Canvas {
        width: WIDTH,
        height: HEIGHT,
        top,
    };
    // A target sitting on the cap is the cap: drawn over it, it hid the cap
    // line entirely for a camera the link never holds back, which is most.
    let target = of(|point| point.target.filter(|target| point.cap != Some(*target)));
    // A value above the scale is not drawn pinned to the top edge, where it
    // reads as a value just over the scale; the numbers line says it.
    let dashed: Vec<Option<u32>> = of(|point| point.link)
        .into_iter()
        .map(|value| value.filter(|value| *value <= top))
        .collect();
    let cap = if draw_cap { of(|point| point.cap) } else { Vec::new() };

    let sent_runs = canvas.runs(&sent);
    let area = rates::area_d(&sent_runs, HEIGHT);
    let line = rates::line_d(&sent_runs, true);
    let cap_line = rates::line_d(&canvas.runs(&cap), false);
    let target_line = rates::line_d(&canvas.runs(&target), false);
    let link_line = rates::line_d(&canvas.runs(&dashed), false);
    let half = HEIGHT / 2.0;
    let grid = format!("M0,{half} L{WIDTH},{half}");

    // Which second the pointer is over, if any: its figures replace the
    // latest second's in the line under the chart. No tooltip, because blitz
    // has none.
    let mut hovered = use_signal(|| None::<usize>);
    let over = hovered();
    let numbers = match over.and_then(|index| slots.get(index).copied().map(|slot| (index, slot))) {
        Some((index, Some(point))) => numbers(&ago(index), point, draw_cap, link, top),
        Some((index, None)) => format!("{}  \u{b7}  nothing sent yet", ago(index)),
        None => numbers("now", points.last().copied().unwrap_or_default(), draw_cap, link, top),
    };
    let half_label = rate(top / 2);
    let top_label = rate(top);

    rsx! {
        div { class: "relative h-[120px] w-full overflow-hidden rounded-md bg-neutral-950",
            svg {
                // Needed: usvg only accepts a root in the SVG namespace.
                xmlns: "http://www.w3.org/2000/svg",
                width: "100%",
                height: "120",
                view_box: "0 0 600 120",
                // Stretched, not fitted: the chart fills whatever width the
                // pane has, and only heights carry meaning.
                preserve_aspect_ratio: "none",
                fill: "none",
                path { d: "{grid}", stroke: GRID, stroke_width: "1" }
                if !area.is_empty() {
                    path { d: "{area}", fill: SENT, fill_opacity: "0.22", stroke: "none" }
                    path {
                        d: "{line}",
                        stroke: SENT,
                        stroke_width: "2",
                        stroke_linejoin: "round",
                        stroke_linecap: "round",
                    }
                }
                if !target_line.is_empty() {
                    path { d: "{target_line}", stroke: TARGET, stroke_width: "1.5" }
                }
                if !cap_line.is_empty() {
                    path { d: "{cap_line}", stroke: CAP, stroke_width: "2" }
                }
                if !link_line.is_empty() {
                    path {
                        d: "{link_line}",
                        stroke: LINK,
                        stroke_width: "2",
                        stroke_dasharray: "10 8",
                    }
                }
            }
            // The scale: the top edge and the line halfway.
            span { class: "absolute left-2 top-1 text-xs text-neutral-500", "{top_label}" }
            span { class: "absolute left-2 top-[62px] text-xs text-neutral-600", "{half_label}" }
            // A column per second over everything else, to say which second
            // the pointer is on without having to know the chart's width.
            div {
                class: "absolute inset-0 flex flex-row",
                onmouseleave: move |_| {
                    let was = *hovered.peek();
                    if was.is_some() {
                        hovered.set(None);
                    }
                },
                for index in 0..rates::SECONDS {
                    div {
                        key: "{index}",
                        class: "relative h-[120px] min-w-0 flex-1",
                        // Written only on a change: a pointer moving inside
                        // one column must not repaint the window each time.
                        onmousemove: move |_| {
                            let was = *hovered.peek();
                            if was != Some(index) {
                                hovered.set(Some(index));
                            }
                        },
                        if over == Some(index) {
                            div { class: "absolute bottom-0 left-0 top-0 w-px bg-neutral-400" }
                        }
                    }
                }
            }
        }
        div { class: "mt-2 flex flex-row flex-wrap items-center gap-4 text-xs text-neutral-400",
            div { class: "flex flex-row items-center gap-1.5",
                div { class: "h-[2px] w-4 shrink-0 bg-blue-500" }
                span { "sent (5 s average)" }
            }
            if !cap_line.is_empty() {
                div { class: "flex flex-row items-center gap-1.5",
                    div { class: "h-[2px] w-4 shrink-0 bg-red-500" }
                    span { "cap" }
                }
            }
            if !target_line.is_empty() {
                div { class: "flex flex-row items-center gap-1.5",
                    div { class: "h-px w-4 shrink-0 bg-amber-400" }
                    span { "target" }
                }
            }
            if !link_line.is_empty() {
                div { class: "flex flex-row items-center gap-1.5",
                    div { class: "flex shrink-0 flex-row gap-[2px]",
                        div { class: "h-[2px] w-[6px] bg-emerald-400" }
                        div { class: "h-[2px] w-[6px] bg-emerald-400" }
                    }
                    span { "{link}" }
                }
            }
        }
        p { class: "mt-1 font-mono text-xs text-neutral-300", "{numbers}" }
    }
}

/// A rate as a person reads it: `345 kbps`, `39.0 Mbps`.
fn rate(kbps: u32) -> String {
    match kbps {
        0..10_000 => format!("{kbps} kbps"),
        _ => format!("{:.1} Mbps", kbps as f64 / 1000.0),
    }
}

/// How long ago the second in slot `index` was, the newest being the last.
fn ago(index: usize) -> String {
    match rates::SECONDS.saturating_sub(index + 1) {
        0 => "now".to_string(),
        seconds => format!("{seconds} s ago"),
    }
}

/// One second in words, e.g. `now · sent 272 kbps · cap 345 kbps · link
/// estimate 39.0 Mbps (off the chart)`, the dashed line named as `link` says.
/// Every figure carries its unit, and only what the chart or its legend also
/// shows is named: a target sitting on the cap is the cap. A dashed value above
/// the scale is not drawn, so here it says so.
fn numbers(when: &str, point: Point, with_cap: bool, link: &str, top: u32) -> String {
    let mut parts = vec![when.to_string()];
    match point.sent {
        Some(sent) => parts.push(format!("sent {}", rate(sent))),
        None => parts.push("nothing sent".to_string()),
    }
    if let (true, Some(cap)) = (with_cap, point.cap) {
        parts.push(format!("cap {}", rate(cap)));
    }
    if let Some(target) = point.target.filter(|target| point.cap != Some(*target)) {
        parts.push(format!("target {}", rate(target)));
    }
    match point.link {
        Some(value) if value > top => parts.push(format!("{link} {} (off the chart)", rate(value))),
        Some(value) => parts.push(format!("{link} {}", rate(value))),
        None => {}
    }
    parts.join("  \u{b7}  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_figure_carries_its_unit_and_a_target_on_the_cap_is_not_named() {
        let point = Point {
            sent: Some(272),
            cap: Some(345),
            target: Some(345),
            link: Some(38_961),
        };
        assert_eq!(
            numbers("now", point, true, ESTIMATE, 400),
            "now  \u{b7}  sent 272 kbps  \u{b7}  cap 345 kbps  \u{b7}  link estimate 39.0 Mbps (off the chart)"
        );
        let held = Point { target: Some(300), link: Some(310), ..point };
        assert_eq!(
            numbers("12 s ago", held, true, ESTIMATE, 400),
            "12 s ago  \u{b7}  sent 272 kbps  \u{b7}  cap 345 kbps  \u{b7}  target 300 kbps  \u{b7}  link estimate 310 kbps"
        );
        assert_eq!(
            numbers("now", held, true, SLICE, 400),
            "now  \u{b7}  sent 272 kbps  \u{b7}  cap 345 kbps  \u{b7}  target 300 kbps  \u{b7}  slice of the link 310 kbps"
        );
    }

    #[test]
    fn the_newest_slot_is_now() {
        assert_eq!(ago(rates::SECONDS - 1), "now");
        assert_eq!(ago(rates::SECONDS - 13), "12 s ago");
        assert_eq!(ago(0), "59 s ago");
    }
}
