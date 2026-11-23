//! The Network tab: the last minute of what each outgoing stream sent, against
//! its cap and the link's estimate.
//!
//! **There is no shared budget to draw.** Each stream has its own cap, and the
//! transport's estimate for the whole connection is the one number they all
//! answer to. So each stream gets a chart against its own cap, and a third adds
//! them up against the estimate: that is the one where an overshoot shows,
//! because nothing divides the link between them.
//!
//! **Drawn with plain `div`s, not an SVG.** A slot per second, each a fixed
//! height box with its bar and its line segments positioned absolutely inside
//! it, the way the video sits in its tile. Every construct here is one the app
//! already relies on (absolute insets in a relative box, a dynamic size in an
//! inline `style`), whereas an inline SVG is handed to usvg as a re-serialised
//! subtree and re-parsed on each change, which nothing here does once a second
//! yet.
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

/// Each chart's height. Fixed pixels, because a percentage height has not
/// resolved reliably in blitz (see the app's root), and every position inside
/// is computed against it.
const CHART_PX: u32 = 120;

/// The lines' thicknesses, which tell them apart without colour.
const CAP_PX: u32 = 2;
const TARGET_PX: u32 = 1;
const ESTIMATE_PX: u32 = 2;

#[component]
pub fn NetworkTab(metrics: Arc<Metrics>, open: bool) -> Element {
    rsx! {
        div { class: "flex flex-col",
            div { class: "text-lg font-semibold text-neutral-100", "Network" }
            p { class: "mt-1 text-sm text-neutral-400",
                "What your call sends, one bar a second for the last minute, newest at the right. Each stream has its own cap; nothing divides the link between them, so the link's estimate is the one number they share."
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
    let latest = seconds.last().copied().unwrap_or_default();
    let camera: Vec<Point> = seconds.iter().map(Point::camera).collect();
    let share: Vec<Point> = seconds.iter().map(Point::share).collect();
    let total: Vec<Point> = seconds.iter().map(Point::total).collect();

    let has_camera = camera.iter().any(|point| point.cap.is_some());
    // While sharing, not "if it shared this minute": a share that ended is
    // still in the total.
    let has_share = latest.share_cap.is_some();
    let has_total = total.iter().any(|point| point.sent.is_some());

    if !has_camera && !has_share && !has_total {
        return rsx! {
            p { class: "mt-6 text-sm text-neutral-300",
                "Nothing to show yet. The charts fill in once a call is sending video."
            }
        };
    }

    rsx! {
        Heading { text: "Camera" }
        if has_camera {
            Chart { points: camera, draw_cap: true }
        } else {
            p { class: "text-sm text-neutral-300", "The camera is not sending." }
        }
        if has_share {
            Heading { text: "Screen share" }
            Chart { points: share, draw_cap: true }
        }
        Heading { text: "Everything sent" }
        Chart { points: total, draw_cap: false }
        p { class: "mt-1 text-xs text-neutral-500", "Video only: audio is not included." }
    }
}

/// One second's marks, in pixels from the chart's bottom.
#[derive(Clone, Copy, Default, PartialEq)]
struct Marks {
    bar: Option<u32>,
    cap: Option<u32>,
    target: Option<u32>,
    estimate: Option<u32>,
}

/// One chart: a slot per second, each with its bar and line segments.
///
/// The lines are drawn per second rather than as one line across, so a cap
/// that changed mid-minute (Sharp turned on, the camera reopening at another
/// size) shows where it changed instead of pretending it was always the
/// latest. Segments in adjacent slots touch, so a steady value reads as one
/// line; the estimate's are inset to read as dashes.
#[component]
fn Chart(points: Vec<Point>, draw_cap: bool) -> Element {
    let top = rates::top(&points);
    let has_target = points.iter().any(|point| point.target.is_some());
    let has_estimate = points.iter().any(|point| point.estimate.is_some());
    let slots: Vec<Option<Marks>> = rates::slots(&points)
        .into_iter()
        .map(|slot| {
            slot.map(|point| Marks {
                bar: point.sent.map(|kbps| rates::bar_px(kbps, top, CHART_PX)),
                cap: point.cap.filter(|_| draw_cap).map(|kbps| rates::line_px(kbps, top, CHART_PX, CAP_PX)),
                target: point.target.map(|kbps| rates::line_px(kbps, top, CHART_PX, TARGET_PX)),
                estimate: point.estimate.map(|kbps| rates::line_px(kbps, top, CHART_PX, ESTIMATE_PX)),
            })
        })
        .collect();
    let numbers = numbers(points.last().copied().unwrap_or_default(), draw_cap, top);

    rsx! {
        div { class: "relative flex h-[120px] flex-row overflow-hidden rounded-md bg-neutral-950",
            for (index, slot) in slots.into_iter().enumerate() {
                div { key: "{index}", class: "relative h-[120px] min-w-0 flex-1",
                    if let Some(marks) = slot {
                        if let Some(height) = marks.bar {
                            div {
                                class: "absolute bottom-0 left-px right-px bg-blue-500",
                                style: "height: {height}px",
                            }
                        }
                        if let Some(bottom) = marks.cap {
                            div {
                                class: "absolute left-0 right-0 h-[2px] bg-red-500",
                                style: "bottom: {bottom}px",
                            }
                        }
                        if let Some(bottom) = marks.target {
                            div {
                                class: "absolute left-0 right-0 h-px bg-amber-400",
                                style: "bottom: {bottom}px",
                            }
                        }
                        if let Some(bottom) = marks.estimate {
                            div {
                                class: "absolute left-[2px] right-[2px] h-[2px] bg-emerald-400",
                                style: "bottom: {bottom}px",
                            }
                        }
                    }
                }
            }
            // The scale, over the slots, so it is after them.
            span { class: "absolute left-2 top-1 text-xs text-neutral-500", "{top} kbps" }
        }
        div { class: "mt-2 flex flex-row flex-wrap items-center gap-4 text-xs text-neutral-400",
            div { class: "flex flex-row items-center gap-1.5",
                div { class: "h-2.5 w-2.5 shrink-0 bg-blue-500" }
                span { "sent" }
            }
            if draw_cap {
                div { class: "flex flex-row items-center gap-1.5",
                    div { class: "h-[2px] w-4 shrink-0 bg-red-500" }
                    span { "cap" }
                }
            }
            if has_target {
                div { class: "flex flex-row items-center gap-1.5",
                    div { class: "h-px w-4 shrink-0 bg-amber-400" }
                    span { "target" }
                }
            }
            if has_estimate {
                div { class: "flex flex-row items-center gap-1.5",
                    div { class: "flex shrink-0 flex-row gap-[2px]",
                        div { class: "h-[2px] w-[6px] bg-emerald-400" }
                        div { class: "h-[2px] w-[6px] bg-emerald-400" }
                    }
                    span { "link estimate" }
                }
            }
        }
        p { class: "mt-1 font-mono text-xs text-neutral-300", "{numbers}" }
    }
}

/// The latest second in words, e.g. `sent 1840 kbps · cap 2500 · link 5200`.
/// An estimate above the scale is drawn at the top, so here it says so.
fn numbers(point: Point, with_cap: bool, top: u32) -> String {
    let mut parts = Vec::new();
    match point.sent {
        Some(sent) => parts.push(format!("sent {sent} kbps")),
        None => parts.push("sent ?".to_string()),
    }
    if let (true, Some(cap)) = (with_cap, point.cap) {
        parts.push(format!("cap {cap}"));
    }
    if let Some(target) = point.target {
        parts.push(format!("target {target}"));
    }
    match point.estimate {
        Some(estimate) if estimate > top => parts.push(format!("link {estimate}, above the chart")),
        Some(estimate) => parts.push(format!("link {estimate}")),
        None => {}
    }
    parts.join("  \u{b7}  ")
}
