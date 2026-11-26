//! The room as a picture, at the top of the Network tab: relays as squares,
//! people as circles (a host-coloured ring on those who can host), a grey
//! line wherever media could flow and a white one wherever it does.
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
//! which is what a closed (`hidden`) window's widget does.
//!
//! **What people say is drawn, not what we infer** (see `net::web`). A direct
//! call meets nowhere, so it has no relay and no assumed lines.

use std::cell::RefCell;
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
use crate::net::web::{self, Kind, State, Web};
use crate::net::{Participant, PeerStats};
use crate::web_layout::{self, Place};

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
}

#[derive(Clone, Debug, PartialEq)]
struct Line {
    a: usize,
    b: usize,
    state: State,
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

/// Plain colours: nothing here goes through CSS.
const PERSON: Color = Color::from_rgb8(82, 82, 82);
const PERSON_EDGE: Color = Color::from_rgb8(163, 163, 163);
const ME: Color = Color::from_rgb8(229, 229, 229);
const RELAY: Color = Color::from_rgb8(64, 64, 64);
const RELAY_EDGE: Color = Color::from_rgb8(212, 212, 212);
const QUIET: Color = Color::from_rgb8(163, 163, 163);
const FLOWING: Color = Color::from_rgb8(245, 245, 245);
const SELECTED: Color = Color::from_rgb8(255, 255, 255);

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

    // Lines first, under the nodes, a path per look: one stroke for each
    // rather than one per line, which keeps a full room's scene small.
    let quiet = drawing.lines.iter().filter(|line| line.state != State::Flowing).count();
    let mut paths: Vec<((State, bool), BezPath)> = Vec::new();
    for line in &drawing.lines {
        let (Some(a), Some(b)) = (drawing.dots.get(line.a), drawing.dots.get(line.b)) else { continue };
        let touches = drawing.selected.is_some_and(|selected| selected == line.a || selected == line.b);
        let look = (line.state, touches && line.state != State::Flowing);
        let path = match paths.iter_mut().find(|(seen, _)| *seen == look) {
            Some((_, path)) => path,
            None => {
                paths.push((look, BezPath::new()));
                &mut paths.last_mut().expect("just pushed").1
            }
        };
        path.move_to(at(&a.place));
        path.line_to(at(&b.place));
    }
    // Flowing last, so it lies over the grey.
    paths.sort_by_key(|((state, selected), _)| (*state == State::Flowing, *selected));
    for ((state, selected), path) in &paths {
        let (stroke, colour) = match state {
            State::Flowing => (Stroke::new(1.5 * scale), FLOWING.with_alpha(0.95)),
            State::Held => (Stroke::new(scale), QUIET.with_alpha(web_layout::quiet_alpha(quiet, *state, *selected))),
            State::Assumed => (
                Stroke::new(scale).with_dashes(0.0, [4.0 * scale, 3.0 * scale]),
                QUIET.with_alpha(web_layout::quiet_alpha(quiet, *state, *selected)),
            ),
        };
        scene.stroke(&stroke, Affine::IDENTITY, colour, None, path);
    }

    let host = colour::rgb(colour::HOST).map_or(SELECTED, |[r, g, b]| Color::from_rgb8(r, g, b));
    for (index, dot) in drawing.dots.iter().enumerate() {
        let centre = at(&dot.place);
        let size = dot.place.size * side;
        match dot.kind {
            Kind::Relay => {
                let square = Rect::from_center_size(centre, (size, size));
                scene.fill(Fill::NonZero, Affine::IDENTITY, RELAY, None, &square);
                scene.stroke(&Stroke::new(scale), Affine::IDENTITY, RELAY_EDGE, None, &square.inset(-0.5 * scale));
            }
            Kind::Participant => {
                let circle = Circle::new(centre, size / 2.0);
                scene.fill(Fill::NonZero, Affine::IDENTITY, if dot.me { ME } else { PERSON }, None, &circle);
                if dot.can_host {
                    // Inside the circle's edge, so a host is no larger.
                    let ring = Circle::new(centre, (size / 2.0 - scale).max(scale));
                    scene.stroke(&Stroke::new(2.0 * scale), Affine::IDENTITY, host, None, &ring);
                } else {
                    scene.stroke(&Stroke::new(scale), Affine::IDENTITY, PERSON_EDGE.with_alpha(0.6), None, &circle);
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
/// draw, only people.
fn gather(me: &str, local: &PeerStats, roster: &[(String, Arc<PeerStats>)]) -> Web {
    let mut people = vec![web::person(me, true, local)];
    people.extend(roster.iter().map(|(id, stats)| web::person(id, false, stats)));
    // In a room we send somewhere, or meet at an endpoint, or hold a
    // session; a direct call does none of these. Inferred from what the
    // call thread sets, not from a flag of its own: none exists.
    let in_room = local.sending().is_some() || local.meets_at().is_some() || !local.held().is_empty();
    if !in_room {
        return web::build(&people, &[], None);
    }
    let at = web::meeting(local.meets_at().as_ref(), me, &people);
    let relays: Vec<web::Relay> = at.relay.into_iter().collect();
    web::build(&people, &relays, Some(&at.venue))
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
            None => format!("Relay {}", crate::name::short(&node.id)),
        },
    }
}

/// The lines under the map about the selected node.
fn details(now: &Web, at: usize) -> (String, String) {
    let node = &now.nodes[at];
    let what = match node.kind {
        Kind::Relay => "Relay",
        Kind::Participant => "Participant",
    };
    let mut first = format!("{what}: {} \u{b7} {}", label(node), crate::name::short(&node.id));
    if node.kind == Kind::Participant {
        first.push_str(if node.can_host { " \u{b7} can host" } else { " \u{b7} does not host" });
    }
    fn other<'e>(edge: &'e web::Edge, id: &str) -> &'e str {
        if edge.from == id { &edge.to } else { &edge.from }
    }
    let theirs: Vec<&web::Edge> = now.edges.iter().filter(|edge| edge.from == node.id || edge.to == node.id).collect();
    let flowing: Vec<String> = theirs
        .iter()
        .filter(|edge| edge.state == State::Flowing)
        .filter_map(|edge| now.nodes.iter().find(|them| them.id == other(edge, &node.id)))
        .map(label)
        .collect();
    let assumed = theirs.iter().any(|edge| edge.state == State::Assumed);
    let links = match theirs.len() {
        1 => "1 link".to_string(),
        n => format!("{n} links"),
    };
    let mut second = match flowing.len() {
        0 => format!("{links}, no media flowing on any"),
        _ => format!("{links}; media flowing with {}", flowing.join(", ")),
    };
    if assumed {
        second.push_str(" (one assumed: they say nothing about where they send)");
    }
    (first, second)
}

#[component]
pub(super) fn RoomMap(local: Arc<PeerStats>, participants: Signal<Vec<Participant>>, me: String) -> Element {
    let fake = crate::args::Args::get().fake_web;
    let roster = roster_of(&participants.read());
    let shared = use_hook(|| Arc::new(std::sync::Mutex::new((String::new(), Vec::<(String, Arc<PeerStats>)>::new()))));
    if let Ok(mut slot) = shared.lock() {
        *slot = (me.clone(), roster.clone());
    }
    let mut room = use_signal({
        let local = local.clone();
        let me = me.clone();
        move || model(fake, 0, &me, &local, &roster)
    });

    let stop = use_hook(|| Arc::new(AtomicBool::new(false)));
    use_drop({
        let stop = stop.clone();
        move || stop.store(true, Ordering::Relaxed)
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
    let chosen = selected().filter(|id| now.nodes.iter().any(|node| node.id == *id));
    let places = web_layout::layout(&now, chosen.as_deref());
    let shown = web_layout::shown_edges(&now, chosen.as_deref());
    let index_of = |id: &str| now.nodes.iter().position(|node| node.id == id);
    let chosen_at = chosen.as_deref().and_then(index_of);
    let drawing = Drawing {
        dots: now
            .nodes
            .iter()
            .zip(&places)
            .map(|(node, place)| Dot { kind: node.kind, can_host: node.can_host, me: node.me, place: *place })
            .collect(),
        lines: shown
            .edges
            .iter()
            .filter_map(|&at| {
                let edge = &now.edges[at];
                Some(Line { a: index_of(&edge.from)?, b: index_of(&edge.to)?, state: edge.state })
            })
            .collect(),
        selected: chosen_at,
    };

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

    // Names: under a node, or over it near the bottom edge, where under it
    // would leave the square.
    let names: Vec<(String, String)> = now
        .nodes
        .iter()
        .zip(&places)
        .filter(|(_, place)| place.named)
        .map(|(node, place)| {
            let width = place.label;
            let left = (place.x - width / 2.0).clamp(0.0, 1.0 - width) * 100.0;
            let reach = place.reach(node.kind);
            let vertical = if place.y < 0.8 {
                format!("top:{:.2}%", (place.y + reach + 0.008) * 100.0)
            } else {
                format!("bottom:{:.2}%", (1.0 - place.y + reach + 0.008) * 100.0)
            };
            (node.id.clone(), format!("left:{left:.2}%;width:{:.2}%;{vertical}", width * 100.0))
        })
        .collect();
    let labels: Vec<(String, String, String)> = names
        .into_iter()
        .filter_map(|(id, style)| now.nodes.iter().find(|node| node.id == id).map(|node| (id, style, label(node))))
        .collect();
    // Click targets: at least a few pixels across even for the smallest dot.
    let targets: Vec<(String, String)> = now
        .nodes
        .iter()
        .zip(&places)
        .map(|(node, place)| {
            let across = (place.reach(node.kind) * 2.0 * 1.3).max(0.05);
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
        n if chosen_at.is_some() => Some(format!("{n} could-flow lines hidden: showing {focus}'s and every flowing one.")),
        n => Some(format!("{n} could-flow lines hidden: showing {focus} own and every flowing one. Click someone to see theirs.")),
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
                for (id, style, text) in labels {
                    div {
                        key: "name-{id}",
                        class: "absolute truncate text-center text-[10px] leading-tight text-neutral-300",
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
            Some((first, second)) => rsx! {
                div { class: "mt-2 flex flex-col gap-0.5",
                    p { class: "text-xs text-neutral-200", "{first}" }
                    p { class: "text-xs text-neutral-400", "{second}" }
                }
            },
            None => rsx! {
                p { class: "mt-2 text-xs text-neutral-500", "Click a node to see its links." }
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
                div { class: "h-px w-4 shrink-0 bg-neutral-500" }
                span { "could flow" }
            }
            div { class: "flex flex-row items-center gap-1.5",
                div { class: "h-[2px] w-4 shrink-0 bg-neutral-100" }
                span { "flowing" }
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
    fn the_details_name_who_media_flows_with() {
        let web = web_layout::fake(6, 1, 0);
        let me = web.nodes.iter().position(|node| node.me).unwrap();
        let (first, second) = details(&web, me);
        assert!(first.starts_with("Participant: You"), "{first}");
        assert!(second.contains("media flowing with "), "{second}");
        let relay = web.nodes.iter().position(|node| node.kind == Kind::Relay).unwrap();
        assert!(details(&web, relay).0.starts_with("Relay: Relay 1"));
    }

    #[test]
    fn a_drawing_paints_once_per_change() {
        let web = web_layout::fake(30, 3, 0);
        let places = web_layout::layout(&web, None);
        let drawing = Drawing {
            dots: web
                .nodes
                .iter()
                .zip(&places)
                .map(|(node, place)| Dot { kind: node.kind, can_host: node.can_host, me: node.me, place: *place })
                .collect(),
            lines: vec![Line { a: 0, b: 1, state: State::Flowing }, Line { a: 1, b: 2, state: State::Assumed }],
            selected: Some(1),
        };
        let scene = draw(&drawing, 400, 300, 2.0);
        // Two line looks, every node filled and outlined, and the selection.
        assert_eq!(scene.commands.len(), 2 + 2 * drawing.dots.len() + 1);
        // A degenerate box paints nothing that could go wrong.
        let _ = draw(&drawing, 0, 0, 1.0);
    }
}
