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
//!
//! **Under the charts, the people in the call** (only while the connection
//! switch is on, and someone else is here): each person's own report on their
//! connection, in figures and, for others, as a chart of the last couple of
//! minutes built from their reports; then the grid of who gets what from
//! whom. The charts above stay outside the switch: they are ours, local, and
//! tell nobody anything.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dioxus_native::prelude::*;

use super::Heading;
use super::super::SignalBars;
use crate::channel::use_channel;
use crate::net::quality::{self, Blame, Link, Quality, Seen};
use crate::net::{Feed, Metrics, Participant, PeerStats};
use crate::rates::{self, Point, Second, kbps_words as rate};

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
pub fn NetworkTab(
    metrics: Arc<Metrics>,
    local: Arc<PeerStats>,
    participants: Signal<Vec<Participant>>,
    me: String,
    open: bool,
) -> Element {
    rsx! {
        div { class: "flex flex-col",
            div { class: "text-lg font-semibold text-neutral-100", "Network" }
            p { class: "mt-1 text-sm text-neutral-400",
                "What your call sent over the last minute, newest at the right. The link's estimate is divided between your microphone, your screen share and your camera, in that order; each video stream follows its slice, and never goes over its cap."
            }
            if open {
                Charts { metrics }
                People { local, participants, me }
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
    let forwarded: Vec<Point> = seconds.iter().map(Point::forwarded).collect();
    let hosting_now = hosting_words(seconds.last());

    // "Sent" counts what the encoder handed to the transport, and a stream
    // nobody subscribed to goes no further: without these words the chart
    // shows an upload that is not happening.
    let unwatched = |alone: bool| if alone { UNWATCHED.to_string() } else { String::new() };
    let camera_notes: Vec<String> = seconds.iter().map(|second| unwatched(second.camera_unwatched())).collect();
    let share_notes: Vec<String> = seconds.iter().map(|second| unwatched(second.share_unwatched())).collect();

    let has_camera = camera.iter().any(|point| point.cap.is_some());
    // "Shared within the minute", not "sharing now": a share that just ended
    // keeps its chart, and the total, until its seconds have scrolled away.
    let has_share = share.iter().any(|point| point.cap.is_some() || point.sent.is_some());
    // Hosting within the minute shown, as for the share: a non-host never
    // sees the section.
    let has_hosting = seconds.iter().any(|second| second.hosting.is_some());

    if !has_camera && !has_share && !has_hosting {
        return rsx! {
            p { class: "mt-6 text-sm text-neutral-300",
                "Nothing to show yet. The charts fill in once a call is sending video."
            }
        };
    }

    // The whole link, in words: with only the camera sending no chart draws
    // it, and it is the figure everything else on this tab is a part of.
    let whole = match seconds.last().and_then(|second| second.estimate) {
        Some(estimate) => format!("Link estimate now: {}", rate(estimate)),
        None => "No link estimate from the transport: every stream holds its own cap.".to_string(),
    };

    rsx! {
        p { class: "mt-4 font-mono text-xs text-neutral-300", "{whole}" }
        Heading { text: "Camera" }
        if has_camera {
            Chart { points: camera, draw_cap: true, link: SLICE, notes: camera_notes }
        } else {
            p { class: "text-sm text-neutral-300", "The camera is not sending." }
        }
        if has_share {
            Heading { text: "Screen share" }
            Chart { points: share, draw_cap: true, link: SLICE, notes: share_notes }
            Heading { text: "Everything sent" }
            Chart { points: total, draw_cap: false, link: ESTIMATE }
            p { class: "mt-1 text-xs text-neutral-500", "Video only: audio is not included." }
        }
        if has_hosting {
            Heading { text: "Hosting" }
            // Sentences, so not in the monospace the charts' figures use.
            p { class: "text-xs text-neutral-300", "{hosting_now}" }
            Chart { points: forwarded, draw_cap: false, link: ESTIMATE, sent_label: "forwarded" }
            p { class: "mt-1 text-xs text-neutral-500",
                "The line is other people's media that you pass on, to your guests and to the relay. Each guest's connection shares your upload, but its speed is estimated on its own."
            }
        }
    }
}

/// The hosting section's words for the latest second: who is connected, who
/// receives from us, what we pass on, and what it comes to against the link.
///
/// **Plain sentences, not figures in brackets**: "0 kbps forwarded for 1
/// guest (1 pulling)" was read as a fault, when with one guest there is
/// simply nobody else's media to pass on.
fn hosting_words(second: Option<&Second>) -> String {
    let Some((hosting, estimate)) = second.and_then(|second| second.hosting.map(|hosting| (hosting, second.estimate))) else {
        return "Not hosting now.".to_string();
    };
    let guests = hosting.guests;
    // A guest that left this second can still have been sent something.
    let receiving = hosting.pulling.min(guests);
    let connected = match (guests, receiving) {
        (0, _) => "No guests are connected to you.".to_string(),
        (1, 0) => "1 guest is connected, receiving nothing from you yet.".to_string(),
        (1, _) => "1 guest is connected and receiving from you.".to_string(),
        (n, 0) => format!("{n} guests are connected, none receiving from you yet."),
        (n, r) if r == n => format!("{n} guests are connected, all receiving from you."),
        (n, r) => format!("{n} guests are connected, {r} of them receiving from you."),
    };
    let passed = match (hosting.forwarded, guests) {
        (0, 0) => "There is nobody else's media to pass on.".to_string(),
        (0, 1) => "With one guest, there is nobody else's media to pass on.".to_string(),
        (0, _) => "You are not passing on anyone else's media right now.".to_string(),
        (kbps, _) => format!("You pass on {} of other people's media.", rate(kbps)),
    };
    let own = match hosting.own {
        0 => "You send nothing of your own".to_string(),
        kbps => format!("You send {} of your own", rate(kbps)),
    };
    // The total only when it differs from our own, which it is otherwise.
    let total = hosting.forwarded.saturating_add(hosting.own);
    let all = if hosting.forwarded == 0 { String::new() } else { format!("; {} in all", rate(total)) };
    let link = match estimate {
        Some(estimate) => format!(", of a {} link estimate", rate(estimate)),
        None => ", with no link estimate yet".to_string(),
    };
    format!("{connected} {passed} {own}{all}{link}.")
}

/// What a chart's filled line is, unless it says otherwise.
const SENT_LABEL: &str = "sent";

/// Said after a second's figures when nobody was subscribed to the stream.
const UNWATCHED: &str = "no one is watching: encoded, not uploaded";
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
///
/// `step` is the seconds between points and `smooth` how many the sent line
/// averages over: our own charts are a point a second, averaged over five; a
/// peer's are a point per report, each already an average, so not again.
/// `notes`, one per point when given, is said after a point's figures.
#[component]
fn Chart(
    points: Vec<Point>,
    draw_cap: bool,
    link: &'static str,
    #[props(default = 1)] step: u32,
    #[props(default = rates::SMOOTH)] smooth: usize,
    #[props(default)] notes: Vec<String>,
    #[props(default = SENT_LABEL)] sent_label: &'static str,
) -> Element {
    // Averaged before it is cut to the minute shown, over the few older
    // seconds the history keeps for this, so the left edge is a full average
    // like every other point.
    let raw: Vec<Option<u32>> = points.iter().map(|point| point.sent).collect();
    let points: Vec<Point> = points
        .iter()
        .zip(rates::smoothed(&raw, smooth))
        .map(|(point, sent)| Point { sent, ..*point })
        .collect();
    let slots = rates::slots(&points);
    let note_slots = rates::slots(&notes);
    let note_at = |index: usize| note_slots.get(index).cloned().flatten().unwrap_or_default();
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
    //
    // Nor is a slice sitting on the cap, for the same reason as the target: a
    // stream is never granted more than it asked for, so on a link with room
    // its slice *is* its cap and the two would draw as one striped line.
    let caps = of(|point| point.cap);
    let dashed: Vec<Option<u32>> = of(|point| point.link)
        .into_iter()
        .zip(&caps)
        .map(|(value, cap)| value.filter(|value| *value <= top && !(draw_cap && *cap == Some(*value))))
        .collect();
    let cap = if draw_cap { caps } else { Vec::new() };

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
        Some((index, Some(point))) => with_note(
            numbers_as(sent_label, &ago(index, step), point, draw_cap, link, top),
            &note_at(index),
        ),
        Some((index, None)) => format!("{}  \u{b7}  nothing {sent_label} yet", ago(index, step)),
        None => with_note(
            numbers_as(sent_label, "now", points.last().copied().unwrap_or_default(), draw_cap, link, top),
            &notes.last().cloned().unwrap_or_default(),
        ),
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
                if smooth > 1 {
                    span { "{sent_label} (5 s average)" }
                } else {
                    span { "{sent_label}" }
                }
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

/// How long ago the point in slot `index` was, the newest being the last,
/// with `step` seconds between points.
fn ago(index: usize, step: u32) -> String {
    match rates::SECONDS.saturating_sub(index + 1) as u32 * step {
        0 => "now".to_string(),
        seconds => format!("{seconds} s ago"),
    }
}

/// A point's figures, then its note if it has one.
fn with_note(figures: String, note: &str) -> String {
    if note.is_empty() { figures } else { format!("{figures}  \u{b7}  {note}") }
}

/// One second in words, e.g. `now · sent 272 kbps · cap 345 kbps · link
/// estimate 39.0 Mbps (off the chart)`, the dashed line named as `link` says.
/// Every figure carries its unit, and only what the chart or its legend also
/// shows is named: a target sitting on the cap is the cap. A dashed value above
/// the scale is not drawn, so here it says so. The filled line is named
/// `label`: "sent" for our own streams, "forwarded" while hosting.
fn numbers_as(label: &str, when: &str, point: Point, with_cap: bool, link: &str, top: u32) -> String {
    let mut parts = vec![when.to_string()];
    match point.sent {
        Some(sent) => parts.push(format!("{label} {}", rate(sent))),
        None => parts.push(format!("nothing {label}")),
    }
    if let (true, Some(cap)) = (with_cap, point.cap) {
        parts.push(format!("cap {}", rate(cap)));
    }
    if let Some(target) = point.target.filter(|target| point.cap != Some(*target)) {
        parts.push(format!("target {}", rate(target)));
    }
    match point.link {
        // Granted everything it asked for: the figure would only repeat the cap.
        Some(value) if with_cap && point.cap == Some(value) => parts.push("the link has room for all of it".to_string()),
        Some(value) if value > top => parts.push(format!("{link} {} (off the chart)", rate(value))),
        Some(value) => parts.push(format!("{link} {}", rate(value))),
        None => {}
    }
    parts.join("  \u{b7}  ")
}

/// One person in the People section: ours first.
#[derive(Clone, Debug, PartialEq)]
struct Person {
    id: String,
    /// Their name, or their short id; "You" for us.
    label: String,
    you: bool,
    /// Their own report; `None` is not reporting.
    link: Option<Link>,
    /// What they get from each other person, by id.
    seen: std::collections::BTreeMap<String, Seen>,
    /// Their reports over the last couple of minutes; empty for us.
    history: Vec<Option<Link>>,
}

/// One cell of the grid: what the column's person gets from the row's.
#[derive(Clone, Debug, PartialEq)]
enum Cell {
    /// A person against themselves: their own report's quality.
    Own(Option<Quality>),
    /// What the receiver said they get.
    Seen(Seen),
    /// The receiver is not reporting, or did not include the sender.
    Missing,
}

impl Cell {
    fn quality(&self) -> Option<Quality> {
        match self {
            Cell::Seen(seen) => Some(seen.q),
            _ => None,
        }
    }
}

/// Everything the section draws, sampled off the interface.
#[derive(Clone, Debug, Default, PartialEq)]
struct View {
    people: Vec<Person>,
    /// `grid[sender][receiver]`, both in `people`'s order.
    grid: Vec<Vec<Cell>>,
    blame: Blame,
}

/// The grid for `people`: rows send, columns receive.
fn grid_of(people: &[Person]) -> Vec<Vec<Cell>> {
    people
        .iter()
        .enumerate()
        .map(|(row, sender)| {
            people
                .iter()
                .enumerate()
                .map(|(column, receiver)| {
                    if row == column {
                        Cell::Own(sender.link.as_ref().map(|link| link.q))
                    } else {
                        receiver.seen.get(&sender.id).cloned().map_or(Cell::Missing, Cell::Seen)
                    }
                })
                .collect()
        })
        .collect()
}

/// The section's view, or `None` when it is not shown: the switch is off, or
/// nobody else is here.
fn gather(me: &str, local: &PeerStats, roster: &[(String, Arc<PeerStats>)]) -> Option<View> {
    if !local.connection_report() || roster.is_empty() {
        return None;
    }
    let mut people = vec![Person {
        id: me.to_string(),
        label: "You".to_string(),
        you: true,
        link: local.link(),
        seen: local.seen(),
        history: Vec::new(),
    }];
    for (id, stats) in roster {
        people.push(Person {
            id: id.clone(),
            label: crate::name::label(stats.name().as_deref(), id),
            you: false,
            link: stats.link(),
            seen: stats.seen(),
            history: stats.reports(),
        });
    }
    let grid = grid_of(&people);
    let qualities: Vec<Vec<Option<Quality>>> =
        grid.iter().map(|row| row.iter().map(Cell::quality).collect()).collect();
    let blame = quality::blame(&qualities);
    Some(View { people, grid, blame })
}

/// Everyone else in the call, once each, by their camera's entry: a share
/// carries its sharer's, so it would only be them again.
pub(super) fn roster_of(participants: &[Participant]) -> Vec<(String, Arc<PeerStats>)> {
    let mut roster: Vec<(String, Arc<PeerStats>)> = Vec::new();
    for participant in participants.iter().filter(|participant| participant.feed == Feed::Camera) {
        if !roster.iter().any(|(id, _)| *id == participant.id) {
            roster.push((participant.id.clone(), participant.stats.clone()));
        }
    }
    roster
}

/// The People section: each person's report, and the grid.
///
/// Sampled by a thread of its own once a second, as the charts are, and sent
/// to the page only when what it draws changed. Who is here comes from the
/// page's participants, handed to the thread through a lock each render.
#[component]
fn People(local: Arc<PeerStats>, participants: Signal<Vec<Participant>>, me: String) -> Element {
    let roster = roster_of(&participants.read());
    let shared = use_hook(|| Arc::new(std::sync::Mutex::new((String::new(), Vec::<(String, Arc<PeerStats>)>::new()))));
    if let Ok(mut slot) = shared.lock() {
        *slot = (me.clone(), roster.clone());
    }
    let mut view = use_signal({
        let local = local.clone();
        let me = me.clone();
        move || gather(&me, &local, &roster)
    });

    let stop = use_hook(|| Arc::new(AtomicBool::new(false)));
    use_drop({
        let stop = stop.clone();
        move || stop.store(true, Ordering::Relaxed)
    });
    let tx = use_channel(move |next: Option<View>| view.set(next));
    use_hook({
        let local = local.clone();
        let shared = shared.clone();
        let stop = stop.clone();
        let first = view.peek().clone();
        move || {
            let spawned = std::thread::Builder::new()
                .name("network-people".into())
                .spawn(move || {
                    let mut sent = first;
                    loop {
                        std::thread::sleep(POLL);
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        let (me, roster) = shared.lock().map(|slot| slot.clone()).unwrap_or_default();
                        let next = gather(&me, &local, &roster);
                        // Only a change is sent: any message wakes the page.
                        if next != sent {
                            if tx.unbounded_send(next.clone()).is_err() {
                                return;
                            }
                            sent = next;
                        }
                    }
                });
            if let Err(error) = spawned {
                tracing::warn!("could not spawn the people poller: {error}");
            }
        }
    });

    let Some(now) = view() else { return rsx! {} };
    let View { people, grid, blame } = now;
    let shown_grid = people.len() >= 2;
    let download: Vec<String> = blame
        .columns
        .iter()
        .enumerate()
        .filter(|(_, flagged)| **flagged)
        .map(|(column, _)| format!("{} ({}): their download?", column + 1, people[column].label))
        .collect();

    rsx! {
        Heading { text: "People" }
        p { class: "text-xs text-neutral-500",
            "What each person says about their own connection, rounded. Others' charts are built from their reports: the last 2 minutes, one point every 2 seconds."
        }
        for person in people.iter().cloned() {
            PersonRow { key: "{person.id}", person }
        }
        if shown_grid {
            Heading { text: "Who gets what" }
            p { class: "mb-3 text-xs text-neutral-500",
                "Rows send, columns receive, numbered as the rows. Each cell is what that column's person gets from that row's: frames a second, and how much later than its best their video arrives. The diagonal is each person's own connection."
            }
            div { class: "flex flex-row items-end gap-[2px]",
                div { class: "w-[120px] shrink-0" }
                for column in 0..people.len() {
                    div {
                        key: "{column}",
                        class: if blame.columns.get(column).copied().unwrap_or(false) {
                            "w-[40px] shrink-0 text-center text-xs font-semibold text-red-400"
                        } else {
                            "w-[40px] shrink-0 text-center text-xs text-neutral-400"
                        },
                        "{column + 1}"
                    }
                }
            }
            for (row, cells) in grid.iter().cloned().enumerate() {
                div { key: "{row}", class: "mt-[2px] flex flex-row items-center gap-[2px]",
                    div { class: "flex h-[34px] w-[120px] shrink-0 flex-col justify-center pr-2",
                        span { class: "truncate text-xs text-neutral-200", "{row + 1}  {people[row].label}" }
                        if blame.rows.get(row).copied().unwrap_or(false) {
                            span { class: "text-[10px] text-red-400", "their upload?" }
                        }
                    }
                    for (column, cell) in cells.into_iter().enumerate() {
                        GridCell { key: "{column}", cell }
                    }
                }
            }
            for line in download {
                p { class: "mt-2 text-xs text-red-400", "{line}" }
            }
        }
    }
}

/// One person: their bars and name, their figures, and for others a chart.
#[component]
fn PersonRow(person: Person) -> Element {
    let figures = person.link.as_ref().map(|link| quality::figures(link).join("  \u{b7}  "));
    let points: Vec<Point> = person.history.iter().map(|link| quality::chart_point(link.as_ref())).collect();
    let notes: Vec<String> = person.history.iter().map(|link| link.as_ref().map(quality::note).unwrap_or_default()).collect();
    let charted = !person.you && points.iter().any(|point| point.sent.is_some() || point.cap.is_some());
    rsx! {
        div { class: "mt-4 flex flex-col",
            div { class: "flex flex-row items-center gap-2",
                if let Some(link) = person.link.as_ref() {
                    SignalBars { quality: link.q }
                }
                span { class: "truncate text-sm text-neutral-100", "{person.label}" }
                if let Some(link) = person.link.as_ref() {
                    span { class: "text-xs text-neutral-400", {link.q.words()} }
                }
            }
            match figures {
                Some(figures) => rsx! { p { class: "mt-1 font-mono text-xs text-neutral-300", "{figures}" } },
                None => rsx! { p { class: "mt-1 text-xs text-neutral-500", "not reporting (older version or turned off)" } },
            }
            if charted {
                div { class: "mt-2",
                    Chart { points, draw_cap: true, link: ESTIMATE, step: 2, smooth: 1, notes }
                }
            }
        }
    }
}

/// One grid cell, fixed in size so twelve fit the pane.
#[component]
fn GridCell(cell: Cell) -> Element {
    match cell {
        Cell::Own(quality) => rsx! {
            div { class: "flex h-[34px] w-[40px] shrink-0 items-center justify-center rounded border border-neutral-600 bg-neutral-900",
                if let Some(quality) = quality {
                    SignalBars { quality }
                }
            }
        },
        Cell::Seen(seen) => {
            let fps = seen.fps.map_or("\u{2013}".to_string(), |fps| format!("{fps} fps"));
            let late = seen.ms.map_or(String::new(), |ms| format!("{ms} ms"));
            rsx! {
                div { class: cell_class(seen.q),
                    span { "{fps}" }
                    span { "{late}" }
                }
            }
        }
        Cell::Missing => rsx! {
            div { class: "flex h-[34px] w-[40px] shrink-0 items-center justify-center rounded bg-neutral-900 text-[10px] text-neutral-600",
                "\u{2013}"
            }
        },
    }
}

/// A received cell's look, by its quality. Whole literals, as everywhere.
fn cell_class(quality: Quality) -> &'static str {
    match quality {
        Quality::Good => "flex h-[34px] w-[40px] shrink-0 flex-col items-center justify-center rounded bg-emerald-900 text-[10px] leading-tight text-neutral-100",
        Quality::Held => "flex h-[34px] w-[40px] shrink-0 flex-col items-center justify-center rounded bg-amber-900 text-[10px] leading-tight text-neutral-100",
        Quality::Poor => "flex h-[34px] w-[40px] shrink-0 flex-col items-center justify-center rounded bg-red-900 text-[10px] leading-tight text-neutral-100",
        Quality::Unknown => "flex h-[34px] w-[40px] shrink-0 flex-col items-center justify-center rounded bg-neutral-800 text-[10px] leading-tight text-neutral-300",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Our own streams' words, which every chart but hosting's uses.
    fn numbers(when: &str, point: Point, with_cap: bool, link: &str, top: u32) -> String {
        numbers_as(SENT_LABEL, when, point, with_cap, link, top)
    }

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
    fn the_forwarded_chart_names_its_line() {
        let point = Point { sent: Some(340), cap: None, target: None, link: Some(3000) };
        assert_eq!(
            numbers_as("forwarded", "now", point, false, ESTIMATE, 400),
            "now  \u{b7}  forwarded 340 kbps  \u{b7}  link estimate 3000 kbps (off the chart)"
        );
        assert_eq!(
            numbers_as("forwarded", "now", Point::default(), false, ESTIMATE, 400),
            "now  \u{b7}  nothing forwarded"
        );
    }

    #[test]
    fn hosting_is_said_in_plain_sentences() {
        use crate::rates::Hosting;
        let said = |hosting: Hosting, estimate: Option<u32>| {
            hosting_words(Some(&Second { hosting: Some(hosting), estimate, ..Second::default() }))
        };
        let of = |forwarded, own, guests, pulling| Hosting { forwarded, own, guests, pulling, forwarding: false };
        assert_eq!(
            said(of(340, 60, 2, 1), Some(3000)),
            "2 guests are connected, 1 of them receiving from you. You pass on 340 kbps of other people's media. \
             You send 60 kbps of your own; 400 kbps in all, of a 3000 kbps link estimate."
        );
        assert_eq!(
            said(of(0, 215, 1, 1), Some(58_700)),
            "1 guest is connected and receiving from you. With one guest, there is nobody else's media to pass on. \
             You send 215 kbps of your own, of a 58.7 Mbps link estimate."
        );
        assert_eq!(
            said(of(0, 215, 0, 0), Some(58_700)),
            "No guests are connected to you. There is nobody else's media to pass on. \
             You send 215 kbps of your own, of a 58.7 Mbps link estimate."
        );
        assert_eq!(
            said(of(0, 0, 1, 0), None),
            "1 guest is connected, receiving nothing from you yet. With one guest, there is nobody else's media to pass on. \
             You send nothing of your own, with no link estimate yet."
        );
        assert_eq!(
            said(of(500, 120, 3, 3), Some(3000)),
            "3 guests are connected, all receiving from you. You pass on 500 kbps of other people's media. \
             You send 120 kbps of your own; 620 kbps in all, of a 3000 kbps link estimate."
        );
        assert_eq!(
            said(of(0, 120, 2, 0), Some(3000)),
            "2 guests are connected, none receiving from you yet. You are not passing on anyone else's media right now. \
             You send 120 kbps of your own, of a 3000 kbps link estimate."
        );
        assert_eq!(hosting_words(Some(&Second::default())), "Not hosting now.");
    }

    #[test]
    fn a_slice_that_is_the_whole_cap_is_said_in_words() {
        let point = Point {
            sent: Some(299),
            cap: Some(345),
            target: Some(345),
            link: Some(345),
        };
        assert_eq!(
            numbers("now", point, true, SLICE, 400),
            "now  \u{b7}  sent 299 kbps  \u{b7}  cap 345 kbps  \u{b7}  the link has room for all of it"
        );
    }

    fn person(id: &str, link: Option<Link>, seen: &[(&str, Quality)]) -> Person {
        Person {
            id: id.to_string(),
            label: id.to_string(),
            you: false,
            link,
            seen: seen.iter().map(|(id, q)| (id.to_string(), Seen { q: *q, ..Seen::default() })).collect(),
            history: Vec::new(),
        }
    }

    #[test]
    fn rows_send_columns_receive_and_the_diagonal_is_their_own() {
        let held = Link { q: Quality::Held, ..Link::default() };
        let people = [
            person("a", Some(held), &[("b", Quality::Poor)]),
            person("b", None, &[]),
            person("c", Some(Link::default()), &[("a", Quality::Good), ("b", Quality::Held)]),
        ];
        let grid = grid_of(&people);
        assert_eq!(grid[0][0], Cell::Own(Some(Quality::Held)));
        assert_eq!(grid[1][1], Cell::Own(None), "not reporting");
        // What c gets from a, and what a gets from b.
        assert_eq!(grid[0][2].quality(), Some(Quality::Good));
        assert_eq!(grid[1][0].quality(), Some(Quality::Poor));
        assert_eq!(grid[1][2].quality(), Some(Quality::Held));
        // b says nothing about anyone; a says nothing about c.
        assert_eq!(grid[0][1], Cell::Missing);
        assert_eq!(grid[2][0], Cell::Missing);
    }

    #[test]
    fn nothing_is_gathered_with_the_switch_off_or_nobody_else_here() {
        let local = PeerStats::default();
        let peer = Arc::new(PeerStats::default());
        let roster = vec![("b".to_string(), peer)];
        assert!(gather("a", &local, &[]).is_none());
        let view = gather("a", &local, &roster).expect("on by default");
        assert_eq!(view.people.len(), 2);
        assert!(view.people[0].you);
        local.set_connection_report(false);
        assert!(gather("a", &local, &roster).is_none());
    }

    #[test]
    fn the_newest_slot_is_now() {
        assert_eq!(ago(rates::SECONDS - 1, 1), "now");
        assert_eq!(ago(rates::SECONDS - 13, 1), "12 s ago");
        assert_eq!(ago(0, 1), "59 s ago");
        assert_eq!(ago(rates::SECONDS - 13, 2), "24 s ago", "a peer's chart is a point per report");
    }
}
