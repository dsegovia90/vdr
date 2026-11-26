//! The room as a picture, at the top of the Network tab: relays as squares,
//! people as circles (a host-coloured ring on those who can host), and a
//! line for every link, split at its middle: the half at a node is white
//! while that node sends media along it, grey while it does not. A link
//! white all along carries both ways; white at one end only, one way, from
//! the white end. A line all grey is a session held with nothing on it.
//!
//! **Painted by a widget, not an inline SVG.** Blitz re-serialises an inline
//! SVG's subtree and hands it to usvg again on any change, then replays it as
//! vectors on every paint, and nothing inside it takes a click. A widget
//! returns a recorded `Scene`, which is cached here and rebuilt only when the
//! picture or the size it is painted at changes. Names and click targets are
//! ordinary elements over it, placed in percentages of the same square.
//!
//! **No work while it cannot be seen.** It is mounted only while the window
//! is open on this tab; its poller stops when it is dropped, builds the model
//! once a second, and sends only when it differs, because any wake of the
//! page repaints the whole window. Blitz skips a widget that measures 0×0,
//! which is what a closed (`hidden`) window's widget does. And it is what
//! asks the room for its routes: mounting sets `map_open` on our entry and
//! dropping clears it, which is all the call reads (`call.rs`).
//!
//! **What people say is drawn, not what we infer** (see `net::web`). A direct
//! call meets nowhere, so it has no relay and no assumed lines; its one line
//! is the session between the two, drawn once the other's media is heard.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyrender::{PaintScene, RenderContext, Scene};
use blitz_dom::node::ComputedStyles;
use dioxus_native::prelude::*;
use dioxus_native::winit::window::Window;
use dioxus_native::{CustomWidgetAttr, Widget};
use peniko::kurbo::{Affine, BezPath, Circle, Point, Rect, Stroke};
use peniko::{Color, Fill};

use super::super::colour;
use super::network::{POLL, roster_of};
use crate::channel::use_channel;
use crate::net::arrival::Arrival;
use crate::net::web::{self, Kind, State, Web};
use crate::net::{Participant, PeerStats};
use crate::web_layout::{self, Place, Side};

/// What the widget paints, worked out on the interface's side so the paint
/// only turns it into shapes.
#[derive(Clone, Debug, Default, PartialEq)]
struct Drawing {
    dots: Vec<Dot>,
    lines: Vec<Line>,
    /// Which dot is selected, by index.
    selected: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct Dot {
    kind: Kind,
    can_host: bool,
    me: bool,
    place: Place,
    /// Off the selected node's paths: drawn faint.
    dim: bool,
    /// A relay with no link at all: nobody reaches it.
    lonely: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Line {
    a: usize,
    b: usize,
    state: State,
    /// How far it bows off straight, from [`web_layout::bows`].
    bow: f64,
    /// Streams from `a` to `b`, and from `b` to `a`.
    forth: u32,
    back: u32,
    /// Too short for halves: one colour all along ([`web_layout::halves_fit`]).
    whole: bool,
    /// Off the selected node's paths: drawn faint.
    dim: bool,
}

/// The drawing and a count that moves whenever it changes, shared between
/// the component, which writes it while rendering, and the widget, which
/// reads it while painting. Both run on the interface's thread, never at
/// once, so a `RefCell`; the paint still only tries to borrow it.
#[derive(Default)]
struct Canvas {
    generation: u64,
    drawing: Drawing,
}

/// Plain colours: nothing here goes through CSS. The grey of a link with
/// nothing on it is well below the white of one carrying, so the two are
/// told apart by brightness and not only by thickness.
const PERSON: Color = Color::from_rgb8(82, 82, 82);
const PERSON_EDGE: Color = Color::from_rgb8(163, 163, 163);
const ME: Color = Color::from_rgb8(229, 229, 229);
const RELAY: Color = Color::from_rgb8(64, 64, 64);
const RELAY_EDGE: Color = Color::from_rgb8(212, 212, 212);
const QUIET: Color = Color::from_rgb8(120, 120, 120);
const FLOWING: Color = Color::from_rgb8(245, 245, 245);
const SELECTED: Color = Color::from_rgb8(255, 255, 255);
/// How much of its strength anything off the selected node's paths keeps.
const DIMMED: f32 = 0.25;

/// How a piece of line is drawn: one path for each, rather than one per
/// line, which keeps a full room's scene small.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Look {
    /// A link with a session and nothing on it.
    Held { strong: bool },
    /// Someone who says nothing, guessed onto the meeting place.
    Assumed { strong: bool },
    /// The half of a carrying link whose node does not send along it.
    Idle,
    /// A half whose node sends along it, by [`web_layout::stream_width`].
    Carrying { width: u8 },
}

/// Paints a [`Drawing`] into whatever square fits the box it is given.
struct MapPaint {
    canvas: Rc<RefCell<Canvas>>,
    /// The last scene, and what it was painted for: generation, width,
    /// height, scale. Blitz asks every widget to paint on every window
    /// paint, so this is what keeps an unchanged map from being redrawn.
    cached: Option<((u64, u32, u32, u64), Scene)>,
}

impl Widget for MapPaint {
    /// `false`: nothing moves between changes, and each change comes with a
    /// render of the component, which is itself a repaint.
    fn requires_redraw(&self) -> bool {
        false
    }

    fn paint(&mut self, _render_ctx: &mut dyn RenderContext, _styles: &ComputedStyles, width: u32, height: u32, scale: f64) -> Scene {
        let Ok(canvas) = self.canvas.try_borrow() else {
            return self.cached.as_ref().map(|(_, scene)| scene.clone()).unwrap_or_default();
        };
        let key = (canvas.generation, width, height, scale.to_bits());
        if let Some((cached, scene)) = &self.cached {
            if *cached == key {
                return scene.clone();
            }
        }
        let scene = draw(&canvas.drawing, width, height, scale);
        self.cached = Some((key, scene.clone()));
        scene
    }
}

/// The shapes of `drawing` in a `width` × `height` box of device pixels, the
/// unit square centred in it at its shorter side.
fn draw(drawing: &Drawing, width: u32, height: u32, scale: f64) -> Scene {
    let mut scene = Scene::new();
    let side = f64::from(width.min(height));
    let left = (f64::from(width) - side) / 2.0;
    let top = (f64::from(height) - side) / 2.0;
    let at = |place: &Place| Point::new(left + place.x * side, top + place.y * side);

    // Lines first, under the nodes: a path per look and strength.
    let quiet = drawing.lines.iter().filter(|line| line.state != State::Flowing).count();
    let mut paths: Vec<((Look, bool), BezPath)> = Vec::new();
    let mut path_for = |look: Look, dim: bool| -> usize {
        match paths.iter().position(|(seen, _)| *seen == (look, dim)) {
            Some(at) => at,
            None => {
                paths.push(((look, dim), BezPath::new()));
                paths.len() - 1
            }
        }
    };
    let mut pieces: Vec<(usize, Point, Point, Point)> = Vec::new();
    for line in &drawing.lines {
        let (Some(a), Some(b)) = (drawing.dots.get(line.a), drawing.dots.get(line.b)) else { continue };
        let (from, to) = (at(&a.place), at(&b.place));
        // The control point, off the middle along the normal
        // `web_layout::bows` measures from; the middle itself when straight.
        let along = to - from;
        let normal = peniko::kurbo::Vec2::new(-along.y, along.x) / along.hypot().max(f64::MIN_POSITIVE);
        let control = from.midpoint(to) + normal * (line.bow * side);
        let touches = drawing.selected.is_some_and(|selected| selected == line.a || selected == line.b);
        let carrying = |streams: u32| Look::Carrying { width: (web_layout::stream_width(streams) * 2.0) as u8 };
        let (first, second) = match line.state {
            State::Held => (Look::Held { strong: touches }, Look::Held { strong: touches }),
            State::Assumed => (Look::Assumed { strong: touches }, Look::Assumed { strong: touches }),
            State::Flowing if line.whole => {
                let look = carrying(line.forth.max(line.back));
                (look, look)
            }
            State::Flowing => (
                if line.forth > 0 { carrying(line.forth) } else { Look::Idle },
                if line.back > 0 { carrying(line.back) } else { Look::Idle },
            ),
        };
        // Split at the curve's own middle (de Casteljau at a half), so a
        // bowed line's halves meet where it is furthest out.
        let middle = Point::new(
            0.25 * from.x + 0.5 * control.x + 0.25 * to.x,
            0.25 * from.y + 0.5 * control.y + 0.25 * to.y,
        );
        pieces.push((path_for(first, line.dim), from, from.midpoint(control), middle));
        pieces.push((path_for(second, line.dim), middle, control.midpoint(to), to));
    }
    for (at, start, control, end) in pieces {
        let path = &mut paths[at].1;
        path.move_to(start);
        path.quad_to(control, end);
    }
    // Grey under white, and the faint under the strong.
    paths.sort_by_key(|((look, dim), _)| (!*dim, *look));
    for ((look, dim), path) in &paths {
        let faded = |alpha: f32| if *dim { alpha * DIMMED } else { alpha };
        let (stroke, colour) = match *look {
            Look::Carrying { width } => (Stroke::new(f64::from(width) / 2.0 * scale), FLOWING.with_alpha(faded(0.95))),
            Look::Idle => (Stroke::new(scale), QUIET.with_alpha(faded(0.8))),
            Look::Held { strong } => {
                (Stroke::new(scale), QUIET.with_alpha(faded(web_layout::quiet_alpha(quiet, State::Held, strong))))
            }
            Look::Assumed { strong } => (
                Stroke::new(scale).with_dashes(0.0, [4.0 * scale, 3.0 * scale]),
                QUIET.with_alpha(faded(web_layout::quiet_alpha(quiet, State::Assumed, strong))),
            ),
        };
        scene.stroke(&stroke, Affine::IDENTITY, colour, None, path);
    }

    let host = colour::rgb(colour::HOST).map_or(SELECTED, |[r, g, b]| Color::from_rgb8(r, g, b));
    for (index, dot) in drawing.dots.iter().enumerate() {
        let centre = at(&dot.place);
        let size = dot.place.size * side;
        let faded = |colour: Color, alpha: f32| colour.with_alpha(if dot.dim { alpha * DIMMED } else { alpha });
        match dot.kind {
            Kind::Relay => {
                let square = Rect::from_center_size(centre, (size, size));
                let edge = square.inset(-0.5 * scale);
                if dot.lonely {
                    // Nobody reaches it: hollow-looking and dashed, not merely
                    // idle.
                    scene.fill(Fill::NonZero, Affine::IDENTITY, faded(RELAY, 0.45), None, &square);
                    let dashed = Stroke::new(scale).with_dashes(0.0, [3.0 * scale, 2.0 * scale]);
                    scene.stroke(&dashed, Affine::IDENTITY, faded(RELAY_EDGE, 0.5), None, &edge);
                } else {
                    scene.fill(Fill::NonZero, Affine::IDENTITY, faded(RELAY, 1.0), None, &square);
                    scene.stroke(&Stroke::new(scale), Affine::IDENTITY, faded(RELAY_EDGE, 1.0), None, &edge);
                }
            }
            Kind::Participant => {
                let circle = Circle::new(centre, size / 2.0);
                scene.fill(Fill::NonZero, Affine::IDENTITY, faded(if dot.me { ME } else { PERSON }, 1.0), None, &circle);
                if dot.can_host {
                    // Inside the circle's edge, so a host is no larger.
                    let ring = Circle::new(centre, (size / 2.0 - scale).max(scale));
                    scene.stroke(&Stroke::new(2.0 * scale), Affine::IDENTITY, faded(host, 1.0), None, &ring);
                } else {
                    scene.stroke(&Stroke::new(scale), Affine::IDENTITY, faded(PERSON_EDGE, 0.6), None, &circle);
                }
            }
        }
        if drawing.selected == Some(index) {
            let around = Circle::new(centre, dot.place.reach(dot.kind) * side + 3.0 * scale);
            scene.stroke(&Stroke::new(1.5 * scale), Affine::IDENTITY, SELECTED, None, &around);
        }
    }
    scene
}

/// The room as we see it: us from `local`, everyone else from their entry,
/// and where the room meets. Outside a room there is no meeting place to
/// draw, only people, and a line to whoever's media reaches us straight.
fn gather(me: &str, local: &PeerStats, roster: &[(String, Arc<PeerStats>)]) -> Web {
    let mut people = vec![web::person(me, true, local)];
    people.extend(roster.iter().map(|(id, stats)| web::person(id, false, stats)));
    // In a room we send somewhere, or meet at an endpoint, or hold a
    // session; a direct call does none of these. Inferred from what the
    // call thread sets, not from a flag of its own: none exists.
    let in_room = local.sending().is_some() || local.meets_at().is_some() || !local.held().is_empty();
    if !in_room {
        // Outside a room the call thread marks a peer's arrival direct only
        // once their broadcast is heard over the one session between us,
        // which carries ours too: media flows both ways on it. A peer not
        // yet heard has no line, as in a room.
        let straight: Vec<&str> = roster
            .iter()
            .filter(|(_, stats)| stats.arrival() == Some(Arrival::Direct))
            .map(|(id, _)| id.as_str())
            .collect();
        return web::direct(web::build(&people, &[], None), me, &straight);
    }
    // What each receiver says, ours from the document we would send.
    let mut reported: BTreeMap<String, web::Reported> = BTreeMap::new();
    if let Some(ours) = local.routes_out().and_then(|document| web::read_routes(&document, me)) {
        reported.insert(me.to_string(), ours);
    }
    for (id, stats) in roster {
        if let Some(theirs) = stats.reported() {
            reported.insert(id.clone(), (*theirs).clone());
        }
    }
    let at = web::meeting(local.meets_at().as_ref(), me, &people);
    let relays: Vec<web::Relay> = at.relay.into_iter().collect();
    web::build_reported(&people, &relays, Some(&at.venue), &reported)
}

/// The model for this tick: the made-up room of `--fake-web`, or the real one.
fn model(fake: Option<(usize, usize)>, tick: u64, me: &str, local: &PeerStats, roster: &[(String, Arc<PeerStats>)]) -> Web {
    match fake {
        Some((participants, relays)) => web_layout::fake(participants, relays, tick),
        None => gather(me, local, roster),
    }
}

/// What a node is called on the map.
fn label(node: &web::Node) -> String {
    match node.kind {
        _ if node.me => "You".to_string(),
        Kind::Participant => crate::name::label(node.name.as_deref(), &node.id),
        Kind::Relay => match &node.name {
            Some(name) => name.clone(),
            None if node.id == web::RELAY_ID => "Relay".to_string(),
            None if web::is_unnamed(&node.id) => "Unnamed relay".to_string(),
            None => format!("Relay {}", crate::name::short(&node.id)),
        },
    }
}

/// The lines under the map about the selected node: what it is, then each
/// line with whether it is flagged.
struct Details {
    first: String,
    lines: Vec<(String, bool)>,
}

/// How a set of flows travels, in words: "via Relay", "directly", "via Host
/// A then Relay", each with how many when they differ.
fn ways(now: &Web, flows: &[&web::Flow]) -> String {
    let mut ways: BTreeMap<String, usize> = BTreeMap::new();
    for flow in flows {
        let between: Vec<String> = flow.between().iter().map(|&at| label(&now.nodes[at])).collect();
        let way = if between.is_empty() { "directly".to_string() } else { format!("via {}", between.join(" then ")) };
        *ways.entry(way).or_default() += 1;
    }
    let mut ways: Vec<(String, usize)> = ways.into_iter().collect();
    ways.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    match ways.as_slice() {
        [(way, _)] => way.clone(),
        _ => ways.iter().map(|(way, count)| format!("{way} ({count})")).collect::<Vec<_>>().join(", "),
    }
}

fn details(now: &Web, at: usize) -> Details {
    let node = &now.nodes[at];
    let what = match node.kind {
        Kind::Relay => "Relay",
        Kind::Participant => "Participant",
    };
    let mut first = format!("{what}: {} \u{b7} {}", label(node), crate::name::short(&node.id));
    if node.kind == Kind::Participant {
        first.push_str(if node.can_host { " \u{b7} can host" } else { " \u{b7} does not host" });
    }
    let mut lines = Vec::new();
    let out: Vec<&web::Flow> = now.flows.iter().filter(|flow| flow.sender() == at).collect();
    let into: Vec<&web::Flow> = now.flows.iter().filter(|flow| flow.receiver() == at).collect();
    let through = now.flows.iter().filter(|flow| flow.between().contains(&at)).count();
    if !out.is_empty() {
        lines.push((format!("sends to {}: {}", out.len(), ways(now, &out)), false));
    }
    if !into.is_empty() {
        lines.push((format!("receives from {}: {}", into.len(), ways(now, &into)), false));
    }
    if through > 0 {
        lines.push((format!("carries {through} of the room's streams"), false));
    }
    for disagreement in web::disagreements(now, &node.id) {
        let says = &now.nodes[disagreement.says];
        let says = if says.kind == Kind::Relay { "the relay".to_string() } else { label(says) };
        let count = disagreement.count;
        let seen = disagreement.seen;
        // A first hop that is the receiver itself is no hop at all.
        let arrive = match out.iter().filter(|flow| flow.path[1] == seen).all(|flow| flow.path.len() == 2) {
            true => "arrive directly".to_string(),
            false => format!("arrive first through {}", label(&now.nodes[seen])),
        };
        lines.push((format!("says it sends via {says}; {count} see it {arrive}"), true));
    }
    if lines.is_empty() {
        // Nobody has reported a path that touches it: say what presence
        // says, as before reports.
        fn other<'e>(edge: &'e web::Edge, id: &str) -> &'e str {
            if edge.from == id { &edge.to } else { &edge.from }
        }
        let theirs: Vec<&web::Edge> = now.edges.iter().filter(|edge| edge.from == node.id || edge.to == node.id).collect();
        let flowing: Vec<String> = theirs
            .iter()
            .filter(|edge| edge.state == State::Flowing)
            .filter_map(|edge| now.index_of(other(edge, &node.id)))
            .map(|at| label(&now.nodes[at]))
            .collect();
        let links = match theirs.len() {
            1 => "1 link".to_string(),
            n => format!("{n} links"),
        };
        let mut second = match flowing.len() {
            0 => format!("{links}, no media on any"),
            _ => format!("{links}; media travels with {}", flowing.join(", ")),
        };
        if theirs.iter().any(|edge| edge.state == State::Assumed) {
            second.push_str(" (one assumed: they say nothing about where they send)");
        }
        lines.push((second, false));
    }
    Details { first, lines }
}

/// The map's drawing for one render: dots and the shown lines, dimmed for a
/// selection, and whether any line was too short for halves while carrying
/// one way only (the legend says so then).
fn drawing_of(now: &Web, places: &[Place], shown: &web_layout::Shown, chosen: Option<usize>) -> (Drawing, bool) {
    let focus = chosen.map(|at| web_layout::focus(now, at));
    let kinds: Vec<Kind> = now.nodes.iter().map(|node| node.kind).collect();
    let ends: Vec<(usize, usize, usize)> = shown
        .edges
        .iter()
        .filter_map(|&at| {
            let edge = &now.edges[at];
            Some((at, now.index_of(&edge.from)?, now.index_of(&edge.to)?))
        })
        .collect();
    let pairs: Vec<(usize, usize)> = ends.iter().map(|&(_, a, b)| (a, b)).collect();
    let bent = web_layout::bows(&kinds, places, &pairs);
    let mut short = false;
    let lines: Vec<Line> = ends
        .iter()
        .zip(&bent)
        .map(|(&(at, a, b), &bow)| {
            let edge = &now.edges[at];
            let whole = !web_layout::halves_fit(&kinds, places, a, b);
            short |= whole && edge.carries_forth() != edge.carries_back();
            Line {
                a,
                b,
                state: edge.state,
                bow,
                forth: edge.forth,
                back: edge.back,
                whole,
                dim: focus.as_ref().is_some_and(|focus| !focus.edges[at]),
            }
        })
        .collect();
    let linked: Vec<bool> = {
        let mut linked = vec![false; now.nodes.len()];
        for edge in &now.edges {
            for end in [&edge.from, &edge.to] {
                if let Some(at) = now.index_of(end) {
                    linked[at] = true;
                }
            }
        }
        linked
    };
    let dots = now
        .nodes
        .iter()
        .zip(places)
        .enumerate()
        .map(|(at, (node, place))| Dot {
            kind: node.kind,
            can_host: node.can_host,
            me: node.me,
            place: *place,
            dim: focus.as_ref().is_some_and(|focus| !focus.nodes[at]),
            lonely: node.kind == Kind::Relay && !linked[at],
        })
        .collect();
    (Drawing { dots, lines, selected: chosen }, short)
}

#[component]
pub(super) fn RoomMap(local: Arc<PeerStats>, participants: Signal<Vec<Participant>>, me: String) -> Element {
    let fake = crate::args::Args::get().fake_web;
    let roster = roster_of(&participants.read());
    let shared = use_hook(|| Arc::new(std::sync::Mutex::new((String::new(), Vec::<(String, Arc<PeerStats>)>::new()))));
    if let Ok(mut slot) = shared.lock() {
        *slot = (me.clone(), roster.clone());
    }
    // While mounted, the call follows everyone's routes for us; the drop
    // below lets them go.
    use_hook({
        let local = local.clone();
        move || local.set_map_open(true)
    });
    let mut room = use_signal({
        let local = local.clone();
        let me = me.clone();
        move || model(fake, 0, &me, &local, &roster)
    });

    let stop = use_hook(|| Arc::new(AtomicBool::new(false)));
    use_drop({
        let stop = stop.clone();
        let local = local.clone();
        move || {
            stop.store(true, Ordering::Relaxed);
            local.set_map_open(false);
        }
    });
    let tx = use_channel(move |next: Web| room.set(next));
    use_hook({
        let local = local.clone();
        let shared = shared.clone();
        let stop = stop.clone();
        let first = room.peek().clone();
        move || {
            let spawned = std::thread::Builder::new()
                .name("network-map".into())
                .spawn(move || {
                    let mut sent = first;
                    let mut tick = 0u64;
                    loop {
                        std::thread::sleep(POLL);
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        tick += 1;
                        let (me, roster) = shared.lock().map(|slot| slot.clone()).unwrap_or_default();
                        let next = model(fake, tick, &me, &local, &roster);
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
                tracing::warn!("could not spawn the room map's poller: {error}");
            }
        }
    });

    let mut selected = use_signal(|| None::<String>);
    let now = room();
    // Someone selected who has since left is nobody selected. Not written
    // back from here: a render does not write the signals it reads.
    let chosen = selected().filter(|id| now.index_of(id).is_some());
    let places = web_layout::layout(&now, chosen.as_deref());
    let shown = web_layout::shown_edges(&now, chosen.as_deref());
    let chosen_at = chosen.as_deref().and_then(|id| now.index_of(id));
    let (drawing, short) = drawing_of(&now, &places, &shown, chosen_at);
    let lonely = drawing.dots.iter().any(|dot| dot.lonely);

    let canvas = use_hook(|| Rc::new(RefCell::new(Canvas::default())));
    let widget = use_hook({
        let canvas = canvas.clone();
        move || CustomWidgetAttr::new(MapPaint { canvas, cached: None })
    });
    if let Ok(mut slot) = canvas.try_borrow_mut() {
        if slot.drawing != drawing {
            slot.drawing = drawing;
            slot.generation += 1;
        }
    }
    // This render is the change; ask for the paint that shows it, as the
    // video does, rather than trust that a render with no DOM change has one.
    let window = use_hook(try_consume_context::<Arc<dyn Window>>);
    if let Some(window) = &window {
        window.request_redraw();
    }

    // Names: outside each node, away from the middle where the lines are:
    // above the top, below the bottom, beside the sides. Each class a whole
    // literal, so Tailwind finds it.
    let labels: Vec<(String, &'static str, String, String)> = now
        .nodes
        .iter()
        .zip(&places)
        .filter(|(_, place)| place.named)
        .map(|(node, place)| {
            let name = web_layout::name_box(place, node.kind);
            let (class, vertical) = match name.side {
                Side::Above => ("absolute truncate text-center text-[10px] leading-tight text-neutral-300", format!("bottom:{:.2}%", (1.0 - name.edge) * 100.0)),
                Side::Below => ("absolute truncate text-center text-[10px] leading-tight text-neutral-300", format!("top:{:.2}%", name.edge * 100.0)),
                Side::Left => ("absolute truncate text-right text-[10px] leading-tight text-neutral-300", format!("top:{:.2}%", name.edge * 100.0)),
                Side::Right => ("absolute truncate text-left text-[10px] leading-tight text-neutral-300", format!("top:{:.2}%", name.edge * 100.0)),
            };
            let style = format!("left:{:.2}%;width:{:.2}%;{vertical}", name.left * 100.0, name.width * 100.0);
            (node.id.clone(), class, style, label(node))
        })
        .collect();
    // Click targets: larger than the dot, as the layout sized them.
    let targets: Vec<(String, String)> = now
        .nodes
        .iter()
        .zip(&places)
        .map(|(node, place)| {
            let across = place.hit;
            let style = format!(
                "left:{:.2}%;top:{:.2}%;width:{:.2}%;height:{:.2}%",
                (place.x - across / 2.0) * 100.0,
                (place.y - across / 2.0) * 100.0,
                across * 100.0,
                across * 100.0,
            );
            (node.id.clone(), style)
        })
        .collect();

    let about = chosen_at.map(|at| details(&now, at));
    let focus = match (&chosen_at, now.nodes.iter().position(|node| node.me)) {
        (Some(at), _) => label(&now.nodes[*at]),
        (None, Some(_)) => "your".to_string(),
        (None, None) => "nobody's".to_string(),
    };
    let hidden = match shown.hidden {
        0 => None,
        n if chosen_at.is_some() => Some(format!("{n} idle lines hidden: showing {focus}'s and every one carrying media.")),
        n => Some(format!("{n} idle lines hidden: showing {focus} own and every one carrying media. Click someone to see theirs.")),
    };
    let faked = fake.map(|(participants, relays)| format!("A made-up room (--fake-web): {participants} people, {relays} relays."));

    rsx! {
        // Square by its width, up to 400 px: the widget paints whatever box
        // it gets, but the names over it assume the same square.
        //
        // **Two boxes, deliberately.** As a flex item, taffy turns `w-full`
        // into a height through the aspect ratio *before* clamping the width
        // to its maximum, so in a pane wider than 400 px one box would be
        // 400 wide and as tall as the pane (read in taffy's source, not
        // seen). The outer box takes the clamped width; the square is a
        // block child of it, where taffy applies the ratio to the maximum
        // too. A box with no height would be a widget measuring 0x0, which
        // blitz skips without a word.
        div { class: "mt-4 w-full max-w-[400px] shrink-0",
            div { class: "relative aspect-square w-full overflow-hidden rounded-md bg-neutral-950",
                // Sized by `inset-0` and, in case the renderer treats it as a
                // replaced element sized by its content, by `h-full w-full` too.
                object { class: "absolute inset-0 h-full w-full", "data": widget.clone() }
                for (id, class, style, text) in labels {
                    div {
                        key: "name-{id}",
                        class: "{class}",
                        style: "{style}",
                        "{text}"
                    }
                }
                // After the names, so they are on top and take the clicks.
                for (id, style) in targets {
                    div {
                        key: "hit-{id}",
                        class: "absolute cursor-pointer rounded-full",
                        style: "{style}",
                        onclick: move |_| {
                            // Read into a local before writing: a borrow held
                            // across the write panics.
                            let was = selected.peek().clone();
                            let next = if was.as_deref() == Some(id.as_str()) { None } else { Some(id.clone()) };
                            if next != was {
                                selected.set(next);
                            }
                        },
                    }
                }
            }
        }
        if let Some(faked) = faked {
            p { class: "mt-1 text-xs text-amber-400", "{faked}" }
        }
        if let Some(hidden) = hidden {
            p { class: "mt-1 text-xs text-neutral-500", "{hidden}" }
        }
        match about {
            Some(Details { first, lines }) => rsx! {
                div { class: "mt-2 flex flex-col gap-0.5",
                    p { class: "text-xs text-neutral-200", "{first}" }
                    for (line, flagged) in lines {
                        if flagged {
                            p { class: "text-xs text-amber-400", "\u{26a0} {line}" }
                        } else {
                            p { class: "text-xs text-neutral-400", "{line}" }
                        }
                    }
                    p { class: "text-xs text-neutral-500", "Click it again to clear." }
                }
            },
            None => rsx! {
                p { class: "mt-2 text-xs text-neutral-500", "Click a node to see where its media goes and comes from." }
            },
        }
        div { class: "mt-2 flex flex-row flex-wrap items-center gap-4 text-xs text-neutral-400",
            div { class: "flex flex-row items-center gap-1.5",
                div { class: "h-2.5 w-2.5 shrink-0 border border-neutral-300 bg-neutral-700" }
                span { "relay" }
            }
            div { class: "flex flex-row items-center gap-1.5",
                div { class: "h-2.5 w-2.5 shrink-0 rounded-full bg-neutral-600" }
                span { "person" }
            }
            div { class: "flex flex-row items-center gap-1.5",
                div { class: "h-2.5 w-2.5 shrink-0 rounded-full border-2 border-host bg-neutral-600" }
                span { "can host" }
            }
            div { class: "flex flex-row items-center gap-1.5",
                div { class: "h-px w-4 shrink-0 bg-neutral-600" }
                span { "held, nothing on it" }
            }
            div { class: "flex flex-row items-center gap-1.5",
                div { class: "flex h-[2px] w-6 shrink-0 flex-row",
                    div { class: "h-[2px] w-3 bg-neutral-100" }
                    div { class: "mt-[0.5px] h-px w-3 bg-neutral-600" }
                }
                span { "one way, from the white end" }
            }
            div { class: "flex flex-row items-center gap-1.5",
                div { class: "h-[2px] w-6 shrink-0 bg-neutral-100" }
                span { "both ways" }
            }
            if lonely {
                div { class: "flex flex-row items-center gap-1.5",
                    div { class: "h-2.5 w-2.5 shrink-0 border border-dashed border-neutral-500 bg-neutral-800" }
                    span { "relay nobody reaches" }
                }
            }
        }
        if short {
            p { class: "mt-1 text-xs text-neutral-500",
                "Lines too short to split are white all along when either end sends."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_person_outside_a_room_has_no_relay() {
        let local = PeerStats::default();
        let web = gather(&"a".repeat(64), &local, &[]);
        assert_eq!(web.nodes.len(), 1);
        assert!(web.nodes[0].me && web.edges.is_empty());
    }

    #[test]
    fn a_direct_call_has_no_line_until_their_media_arrives() {
        let (me, them) = ("b".repeat(64), "a".repeat(64));
        let roster = vec![(them, Arc::new(PeerStats::default()))];
        let web = gather(&me, &PeerStats::default(), &roster);
        assert_eq!(web.nodes.len(), 2);
        assert!(web.edges.is_empty());
    }

    #[test]
    fn the_details_say_where_media_goes_and_comes_from() {
        let web = web_layout::fake(6, 1, 0);
        let me = web.nodes.iter().position(|node| node.me).unwrap();
        let about = details(&web, me);
        assert!(about.first.starts_with("Participant: You"), "{}", about.first);
        assert!(about.lines.iter().any(|(line, _)| line.starts_with("sends to ")), "{:?}", about.lines);
        assert!(about.lines.iter().any(|(line, _)| line.starts_with("receives from ")), "{:?}", about.lines);
        let relay = web.index_of("relay-00").unwrap();
        let about = details(&web, relay);
        assert!(about.first.starts_with("Relay: Relay 1"));
        assert!(about.lines.iter().any(|(line, _)| line.starts_with("carries ")), "{:?}", about.lines);
    }

    #[test]
    fn a_disagreement_is_a_flagged_line() {
        let web = web_layout::fake(12, 2, 0);
        let flagged: Vec<String> = (0..web.nodes.len())
            .flat_map(|at| details(&web, at).lines)
            .filter(|(_, flagged)| *flagged)
            .map(|(line, _)| line)
            .collect();
        assert!(flagged.iter().any(|line| line.starts_with("says it sends via the relay; ") && line.contains("arrive first through Person 1")), "{flagged:?}");
    }

    #[test]
    fn selecting_dims_what_is_off_their_paths() {
        let web = web_layout::fake(20, 3, 0);
        let places = web_layout::layout(&web, None);
        let shown = web_layout::shown_edges(&web, None);
        let (plain, _) = drawing_of(&web, &places, &shown, None);
        assert!(plain.dots.iter().all(|dot| !dot.dim) && plain.lines.iter().all(|line| !line.dim));
        let me = web.nodes.iter().position(|node| node.me).unwrap();
        let (chosen, _) = drawing_of(&web, &places, &shown, Some(me));
        assert!(!chosen.dots[me].dim);
        assert!(chosen.dots.iter().any(|dot| dot.dim), "someone is off our paths");
        assert!(chosen.lines.iter().any(|line| line.dim) && chosen.lines.iter().any(|line| !line.dim));
        // Every line of ours stays strong.
        assert!(chosen.lines.iter().filter(|line| line.a == me || line.b == me).all(|line| !line.dim));
    }

    #[test]
    fn a_relay_nobody_reaches_is_lonely() {
        let people = vec![web::Person { id: "a".repeat(64), me: true, via: Some(crate::net::arrival::Sending::Relay), ..web::Person::default() }];
        let relays = vec![
            web::Relay { id: web::RELAY_ID.to_string(), name: None },
            web::Relay { id: "spare".to_string(), name: None },
        ];
        let web = web::build(&people, &relays, Some(web::RELAY_ID));
        let places = web_layout::layout(&web, None);
        let (drawing, _) = drawing_of(&web, &places, &web_layout::shown_edges(&web, None), None);
        let lonely: Vec<&str> = web.nodes.iter().zip(&drawing.dots).filter(|(_, dot)| dot.lonely).map(|(node, _)| node.id.as_str()).collect();
        assert_eq!(lonely, ["spare"]);
    }

    #[test]
    fn a_drawing_paints_once_per_change() {
        let web = web_layout::fake(30, 3, 0);
        let places = web_layout::layout(&web, None);
        let dot = |(node, place): (&web::Node, &Place)| Dot {
            kind: node.kind,
            can_host: node.can_host,
            me: node.me,
            place: *place,
            dim: false,
            lonely: false,
        };
        let line = |a, b, state, forth, back| Line { a, b, state, bow: 0.05, forth, back, whole: false, dim: false };
        let drawing = Drawing {
            dots: web.nodes.iter().zip(&places).map(dot).collect(),
            lines: vec![
                // One way: a white half and an idle one.
                line(0, 1, State::Flowing, 1, 0),
                // Both ways, busier one way: two widths of white.
                line(1, 2, State::Flowing, 1, 30),
                line(1, 3, State::Assumed, 0, 0),
                line(2, 3, State::Held, 0, 0),
            ],
            selected: Some(1),
        };
        let scene = draw(&drawing, 400, 300, 2.0);
        // Five looks (held, assumed, idle, two widths), every node filled
        // and outlined, and the selection.
        assert_eq!(scene.commands.len(), 5 + 2 * drawing.dots.len() + 1);
        // A degenerate box paints nothing that could go wrong.
        let _ = draw(&drawing, 0, 0, 1.0);
    }

    #[test]
    fn a_line_too_short_for_halves_is_white_all_along_and_said() {
        // Two people a hair apart, one sending to the other.
        let at = |x: f64| Place { x, y: 0.5, size: 0.02, named: false, label: 0.12, hit: 0.02 };
        let kinds = [Kind::Participant; 2];
        assert!(!web_layout::halves_fit(&kinds, &[at(0.5), at(0.53)], 0, 1));
        assert!(web_layout::halves_fit(&kinds, &[at(0.2), at(0.8)], 0, 1));
        let mut web = web::direct(
            web::build(
                &[
                    web::Person { id: "a".repeat(64), me: true, ..web::Person::default() },
                    web::Person { id: "b".repeat(64), ..web::Person::default() },
                ],
                &[],
                None,
            ),
            &"a".repeat(64),
            &[&"b".repeat(64)],
        );
        web.edges[0].back = 0;
        let shown = web_layout::shown_edges(&web, None);
        let (drawing, short) = drawing_of(&web, &[at(0.5), at(0.53)], &shown, None);
        assert!(drawing.lines[0].whole && short);
        let (drawing, short) = drawing_of(&web, &[at(0.2), at(0.8)], &shown, None);
        assert!(!drawing.lines[0].whole && !short);
    }
}
