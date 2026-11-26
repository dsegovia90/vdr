//! Where each node of the room's web goes on the Network tab, which lines
//! are drawn, and a made-up room to look at (`--fake-web`).
//!
//! **Balanced, not simulated.** Positions follow from how many relays and
//! people there are and the order they sort in, never from the edges, so the
//! picture does not drift or wobble, and a person joining re-spaces everyone
//! evenly in one step. No physics, no animation, nothing to settle.
//!
//! **Normalised coordinates**: everything is in a unit square, `(0, 0)` its
//! top left. The widget scales it to the pixels it is given and the names
//! and click targets over it are placed in percentages, so the two agree at
//! any width.
//!
//! The shape: relays in the middle (one at the centre, more on a ring of
//! their own), people around them on rings from the outside in, us at twelve
//! o'clock on the outer ring and everyone else clockwise in id order. Each
//! ring is evenly spaced, and alternate rings are turned half a step so that
//! nobody sits directly behind someone on the ring outside.

use std::f64::consts::{PI, TAU};

use crate::net::arrival::Sending;
use crate::net::web::{self, Kind, Person, Relay, State, Web};

/// At or under this many nodes everyone is named; over it only we and the
/// selected node are, since a hundred names on a 400 px square are noise.
pub const NAMED_MAX: usize = 24;

/// Over this many could-flow lines, only the focused node's are drawn: a
/// hundred people holding four sessions each is a grey fog, not a picture.
/// Lines that carry media now are always drawn.
pub const HELD_DRAWN_MAX: usize = 300;

/// The middle of the square.
const CENTRE: f64 = 0.5;
/// Kept clear at the square's edge, for the names under the outer ring.
const EDGE_ROOM: f64 = 0.06;
/// A person's circle at up to [`PEOPLE_LARGE`] people, and at [`PEOPLE_SMALL`];
/// in between it shrinks in step.
const PERSON_LARGE: f64 = 0.075;
const PERSON_SMALL: f64 = 0.03;
const PEOPLE_LARGE: usize = 20;
const PEOPLE_SMALL: usize = 100;
/// Never smaller than this, however many people: below it a circle is a dot.
const PERSON_FLOOR: f64 = 0.012;
/// Centre to centre along a ring and between rings, in circles: half a circle
/// of air between neighbours.
const SPACING: f64 = 1.5;
/// A relay alone, and the largest a relay on the ring may be.
const RELAY_ALONE: f64 = 0.09;
const RELAY_LARGE: f64 = 0.07;
/// The relays' ring: never tighter than this, never wider than that, so it
/// leaves the people room.
const RELAY_RING_MIN: f64 = 0.1;
const RELAY_RING_MAX: f64 = 0.2;
/// Air between the relays and the innermost ring of people.
const RELAY_GAP: f64 = 0.08;
/// How wide a name may be, as part of the square.
const LABEL_MIN: f64 = 0.12;
const LABEL_MAX: f64 = 0.3;

/// Where one node goes. `size` is a circle's diameter or a square's side.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Place {
    pub x: f64,
    pub y: f64,
    pub size: f64,
    /// Whether its name is shown.
    pub named: bool,
    /// How wide its name may be, centred under it: about the room to its
    /// neighbours on the same ring.
    pub label: f64,
}

impl Place {
    /// Half the widest extent of its shape: a square's corner reaches
    /// further than a circle's edge.
    pub fn reach(&self, kind: Kind) -> f64 {
        match kind {
            Kind::Relay => self.size * std::f64::consts::SQRT_2 / 2.0,
            Kind::Participant => self.size / 2.0,
        }
    }
}

/// The places of `web`'s nodes, in its order. `selected` is named whatever
/// the count.
pub fn layout(web: &Web, selected: Option<&str>) -> Vec<Place> {
    let mut places = vec![Place::default(); web.nodes.len()];
    // Already sorted by id, which is the relays' order.
    let relays: Vec<usize> = (0..web.nodes.len()).filter(|&at| web.nodes[at].kind == Kind::Relay).collect();
    let mut people: Vec<usize> = (0..web.nodes.len()).filter(|&at| web.nodes[at].kind == Kind::Participant).collect();
    // Us first, then by id: the sort is stable and the nodes are in id order.
    people.sort_by_key(|&at| !web.nodes[at].me);

    let reach = place_relays(&relays, &mut places);
    place_people(&people, reach, &mut places);

    let everyone = web.nodes.len() <= NAMED_MAX;
    for (node, place) in web.nodes.iter().zip(places.iter_mut()) {
        place.named = everyone || node.me || selected == Some(node.id.as_str());
        if !everyone {
            // Few names, so each may take its full width.
            place.label = LABEL_MAX;
        }
    }
    places
}

/// Puts the relays in the middle; returns how far from the centre they reach.
fn place_relays(relays: &[usize], places: &mut [Place]) -> f64 {
    match relays.len() {
        0 => 0.0,
        1 => {
            places[relays[0]] = Place { x: CENTRE, y: CENTRE, size: RELAY_ALONE, named: false, label: LABEL_MAX };
            RELAY_ALONE * std::f64::consts::SQRT_2 / 2.0
        }
        count => {
            let n = count as f64;
            // As wide as the largest squares need, within the ring's bounds;
            // the squares then shrink to what the ring has room for. A
            // square's diagonal is what must clear its neighbour.
            let wanted = RELAY_LARGE * std::f64::consts::SQRT_2 * SPACING * n / TAU;
            let radius = wanted.clamp(RELAY_RING_MIN, RELAY_RING_MAX);
            let chord = 2.0 * radius * (PI / n).sin();
            let size = (chord / (std::f64::consts::SQRT_2 * SPACING)).min(RELAY_LARGE);
            let label = chord.clamp(LABEL_MIN, LABEL_MAX);
            for (index, &at) in relays.iter().enumerate() {
                let angle = -PI / 2.0 + TAU * index as f64 / n;
                places[at] = Place {
                    x: CENTRE + radius * angle.cos(),
                    y: CENTRE + radius * angle.sin(),
                    size,
                    named: false,
                    label,
                };
            }
            radius + size * std::f64::consts::SQRT_2 / 2.0
        }
    }
}

/// A person's circle for `count` people, before any shrinking to fit.
fn person_size(count: usize) -> f64 {
    if count <= PEOPLE_LARGE {
        return PERSON_LARGE;
    }
    let along = (count.min(PEOPLE_SMALL) - PEOPLE_LARGE) as f64 / (PEOPLE_SMALL - PEOPLE_LARGE) as f64;
    PERSON_LARGE + (PERSON_SMALL - PERSON_LARGE) * along
}

/// The radii of the rings that fit outside `inner` for circles of `size`,
/// outermost first, each with how many it holds.
fn rings(size: f64, inner: f64) -> Vec<(f64, usize)> {
    let outer = CENTRE - EDGE_ROOM - size / 2.0;
    let step = size * SPACING;
    let mut rings = Vec::new();
    let mut radius = outer;
    // A ring at the very centre holds one, which is what a ring of radius
    // zero would compute as none.
    while radius >= inner.max(0.0) && rings.len() < 64 {
        let holds = if radius < step / 2.0 { 1 } else { (TAU * radius / step).floor() as usize };
        rings.push((radius, holds.max(1)));
        radius -= step;
    }
    rings
}

/// Puts people on rings outside the relays' `reach`.
fn place_people(people: &[usize], reach: f64, places: &mut [Place]) {
    if people.is_empty() {
        return;
    }
    let count = people.len();
    let mut size = person_size(count);
    let (chosen, size) = loop {
        let inner = if reach > 0.0 { reach + RELAY_GAP + size / 2.0 } else { 0.0 };
        let available = rings(size, inner);
        // The fewest rings that hold everyone.
        let mut held = 0;
        let fits = available.iter().position(|(_, holds)| {
            held += holds;
            held >= count
        });
        match fits {
            Some(last) => break (available[..=last].to_vec(), size),
            None if size > PERSON_FLOOR => size = (size * 0.9).max(PERSON_FLOOR),
            // More people than even the smallest circles fit: everyone still
            // gets a place, evenly spread over what there is, touching.
            None => {
                let rings = if available.is_empty() { vec![(CENTRE - EDGE_ROOM - size / 2.0, count)] } else { available };
                let total: usize = rings.iter().map(|(_, holds)| holds).sum();
                let scaled = rings.into_iter().map(|(radius, holds)| (radius, (holds * count).div_ceil(total.max(1)))).collect();
                break (scaled, size);
            }
        }
    };

    // Shared out so every ring is about as full as every other, which keeps
    // the spacing even from ring to ring; ties go to the outer ring.
    let mut on = vec![0usize; chosen.len()];
    for _ in 0..count {
        let ring = (0..chosen.len())
            .filter(|&ring| on[ring] < chosen[ring].1)
            .min_by(|&a, &b| {
                let fill = |ring: usize| on[ring] as f64 / chosen[ring].1 as f64;
                fill(a).total_cmp(&fill(b)).then(a.cmp(&b))
            })
            .unwrap_or(0);
        on[ring] += 1;
    }

    let mut next = people.iter();
    for (ring, (&(radius, _), &n)) in chosen.iter().zip(&on).enumerate() {
        if n == 0 {
            continue;
        }
        let step = TAU / n as f64;
        let turn = if ring % 2 == 1 { step / 2.0 } else { 0.0 };
        let label = (radius * step).clamp(LABEL_MIN, LABEL_MAX);
        for index in 0..n {
            let Some(&at) = next.next() else { return };
            let angle = -PI / 2.0 + turn + step * index as f64;
            places[at] = Place {
                x: CENTRE + radius * angle.cos(),
                y: CENTRE + radius * angle.sin(),
                size,
                named: false,
                label,
            };
        }
    }
}

/// The lines to draw, by index into `web.edges`, and how many were left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shown {
    pub edges: Vec<usize>,
    pub hidden: usize,
}

/// Which of `web`'s edges are drawn. Up to [`HELD_DRAWN_MAX`] could-flow
/// lines, all of them; over it, only the focus's (the selected node, or us),
/// and every flowing line whatever the count.
pub fn shown_edges(web: &Web, selected: Option<&str>) -> Shown {
    let quiet = web.edges.iter().filter(|edge| edge.state != State::Flowing).count();
    if quiet <= HELD_DRAWN_MAX {
        return Shown { edges: (0..web.edges.len()).collect(), hidden: 0 };
    }
    let me = web.nodes.iter().find(|node| node.me).map(|node| node.id.as_str());
    let focus = selected.or(me);
    let mut shown = Shown::default();
    for (at, edge) in web.edges.iter().enumerate() {
        let touches = focus.is_some_and(|focus| edge.from == focus || edge.to == focus);
        if edge.state == State::Flowing || touches {
            shown.edges.push(at);
        } else {
            shown.hidden += 1;
        }
    }
    shown
}

/// How opaque a could-flow line is, given how many are drawn: fainter as
/// they crowd, so the flowing ones still stand out. A line of the selected
/// node is drawn strong whatever the count.
pub fn quiet_alpha(drawn: usize, state: State, selected: bool) -> f32 {
    let base = match drawn {
        0..=30 => 0.55,
        31..=120 => 0.4,
        _ => 0.25,
    };
    let base = if selected { 0.85 } else { base };
    match state {
        State::Assumed => base * 0.7,
        State::Held | State::Flowing => base,
    }
}

/// A room to look at without one (`--fake-web`): `participants` people (us
/// among them), `relays` relay servers, some hosts, and media that moves
/// every few seconds of `tick`, so that a change and its repaint are seen.
/// The same arguments always give the same web.
pub fn fake(participants: usize, relays: usize, tick: u64) -> Web {
    // Ids that look like endpoint ids but do not sort in the order made, so
    // us being first is the layout's doing, not the ids'.
    let id = |n: usize| {
        let mixed = (n as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        format!("{mixed:016x}").repeat(4)
    };
    let relay_ids: Vec<String> = (0..relays).map(|n| format!("relay-{n:02}")).collect();
    let is_host = |n: usize| n % 5 == 1;
    let hosts: Vec<String> = (0..participants).filter(|&n| is_host(n)).map(id).collect();
    let phase = (tick / 4) as usize;

    let people: Vec<Person> = (0..participants)
        .map(|n| {
            let me = n == 0;
            let mut held: Vec<String> = Vec::new();
            if relays > 0 {
                held.push(web::HELD_RELAY.to_string());
            }
            let via = if relays == 0 && me {
                // A room without a relay meets at us.
                Some(Sending::Guests)
            } else if is_host(n) {
                Some(Sending::Relay)
            } else if !hosts.is_empty() && (n + phase).is_multiple_of(3) {
                let host = hosts[(n / 3 + phase) % hosts.len()].clone();
                Some(Sending::Host(host))
            } else if relays > 1 && (n + phase) % 5 == 1 {
                Some(Sending::Host(relay_ids[1 + (n + phase) % (relays - 1)].clone()))
            } else {
                Some(Sending::Relay)
            };
            if !is_host(n) {
                for k in 0..hosts.len().min(3) {
                    held.push(hosts[(n + k) % hosts.len()].clone());
                }
            }
            if relays > 1 {
                held.push(relay_ids[1 + n % (relays - 1)].clone());
            }
            // Someone on an older build says nothing.
            let (via, held) = if n % 11 == 7 { (None, Vec::new()) } else { (via, held) };
            Person {
                id: id(n),
                name: if me { None } else { Some(format!("Person {n}")) },
                can_host: is_host(n),
                me,
                via,
                held,
            }
        })
        .collect();
    let servers: Vec<Relay> = relay_ids
        .iter()
        .enumerate()
        .map(|(n, id)| Relay { id: id.clone(), name: Some(format!("Relay {}", n + 1)) })
        .collect();
    let venue = match (relay_ids.first(), people.first()) {
        (Some(relay), _) => Some(relay.as_str()),
        (None, Some(us)) => Some(us.id.as_str()),
        (None, None) => None,
    };
    web::build(&people, &servers, venue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::web::{Edge, Node};

    fn distance(a: &Place, b: &Place) -> f64 {
        ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
    }

    fn me_of(web: &Web) -> usize {
        web.nodes.iter().position(|node| node.me).unwrap()
    }

    #[test]
    fn the_same_web_lays_out_the_same() {
        let web = fake(30, 3, 0);
        assert_eq!(layout(&web, None), layout(&web, None));
        // Edges change nothing about where anyone is.
        let moved = fake(30, 3, 40);
        assert_ne!(web.edges, moved.edges, "the fake's media moves");
        assert_eq!(layout(&web, None), layout(&moved, None));
    }

    #[test]
    fn we_are_at_twelve_and_the_rest_follow_by_id_clockwise() {
        let web = fake(8, 1, 0);
        let places = layout(&web, None);
        let me = &places[me_of(&web)];
        assert!((me.x - CENTRE).abs() < 1e-9 && me.y < CENTRE, "{me:?}");
        // Everyone else by id, each a step further round.
        let angle = |place: &Place| {
            let turned = (place.y - CENTRE).atan2(place.x - CENTRE) + PI / 2.0;
            turned.rem_euclid(TAU)
        };
        let others: Vec<usize> =
            (0..web.nodes.len()).filter(|&at| web.nodes[at].kind == Kind::Participant && !web.nodes[at].me).collect();
        assert!(others.windows(2).all(|pair| web.nodes[pair[0]].id < web.nodes[pair[1]].id));
        let angles: Vec<f64> = others.iter().map(|&at| angle(&places[at])).collect();
        assert!(angles.windows(2).all(|pair| pair[0] < pair[1]), "{angles:?}");
        assert!(angles[0] > 0.0);
    }

    #[test]
    fn nothing_overlaps_everything_fits_and_people_are_outside_the_relays() {
        for participants in [1, 5, 20, 100] {
            for relays in [1, 5, 50] {
                let web = fake(participants, relays, 0);
                let places = layout(&web, None);
                let what = format!("{participants} people, {relays} relays");
                for (a, (node, place)) in web.nodes.iter().zip(&places).enumerate() {
                    let reach = place.reach(node.kind);
                    assert!(place.x - reach >= 0.0 && place.x + reach <= 1.0, "{what}: {place:?}");
                    assert!(place.y - reach >= 0.0 && place.y + reach <= 1.0, "{what}: {place:?}");
                    for (other, other_place) in web.nodes.iter().zip(&places).skip(a + 1) {
                        let room = distance(place, other_place) - reach - other_place.reach(other.kind);
                        assert!(room > 0.0, "{what}: {} and {} overlap by {}", node.id, other.id, -room);
                    }
                }
                let furthest_relay = web
                    .nodes
                    .iter()
                    .zip(&places)
                    .filter(|(node, _)| node.kind == Kind::Relay)
                    .map(|(node, place)| distance(place, &Place { x: CENTRE, y: CENTRE, ..*place }) + place.reach(node.kind))
                    .fold(0.0, f64::max);
                for (node, place) in web.nodes.iter().zip(&places).filter(|(node, _)| node.kind == Kind::Participant) {
                    let from_centre = distance(place, &Place { x: CENTRE, y: CENTRE, ..*place });
                    assert!(from_centre - place.reach(node.kind) > furthest_relay, "{what}: {place:?}");
                }
            }
        }
    }

    #[test]
    fn a_ring_is_evenly_spaced_before_and_after_someone_joins() {
        for count in [6, 7, 40, 41] {
            let web = fake(count, 1, 0);
            let places = layout(&web, None);
            // Group people by ring radius, then check each ring's gaps.
            let mut rings: Vec<(f64, Vec<f64>)> = Vec::new();
            for (node, place) in web.nodes.iter().zip(&places).filter(|(node, _)| node.kind == Kind::Participant) {
                let _ = node;
                let radius = ((place.x - CENTRE).powi(2) + (place.y - CENTRE).powi(2)).sqrt();
                let angle = (place.y - CENTRE).atan2(place.x - CENTRE).rem_euclid(TAU);
                match rings.iter_mut().find(|(r, _)| (r - radius).abs() < 1e-6) {
                    Some((_, angles)) => angles.push(angle),
                    None => rings.push((radius, vec![angle])),
                }
            }
            let placed: usize = rings.iter().map(|(_, angles)| angles.len()).sum();
            assert_eq!(placed, count);
            for (_, angles) in &mut rings {
                angles.sort_by(f64::total_cmp);
                let step = TAU / angles.len() as f64;
                for pair in angles.windows(2) {
                    assert!((pair[1] - pair[0] - step).abs() < 1e-6, "{count}: {angles:?}");
                }
            }
        }
    }

    #[test]
    fn sizes_stay_within_their_limits() {
        for participants in [1, 5, 20, 21, 60, 100] {
            for relays in [1, 2, 5, 50] {
                let web = fake(participants, relays, 0);
                for (node, place) in web.nodes.iter().zip(layout(&web, None)) {
                    match node.kind {
                        Kind::Participant => {
                            assert!((PERSON_FLOOR..=PERSON_LARGE).contains(&place.size), "{place:?}");
                            if participants <= PEOPLE_LARGE {
                                assert_eq!(place.size, PERSON_LARGE);
                            }
                        }
                        Kind::Relay => assert!(place.size > 0.01 && place.size <= RELAY_ALONE, "{place:?}"),
                    }
                    assert!((LABEL_MIN..=LABEL_MAX).contains(&place.label), "{place:?}");
                }
            }
        }
        // A hundred people are smaller than twenty.
        let small = layout(&fake(100, 1, 0), None);
        let large = layout(&fake(20, 1, 0), None);
        assert!(small.iter().map(|p| p.size).fold(1.0, f64::min) < large.iter().map(|p| p.size).fold(1.0, f64::min));
    }

    #[test]
    fn everyone_is_named_up_to_the_limit_and_then_only_us_and_the_selection() {
        let web = fake(NAMED_MAX - 1, 1, 0);
        assert_eq!(web.nodes.len(), NAMED_MAX);
        assert!(layout(&web, None).iter().all(|place| place.named));

        let web = fake(NAMED_MAX, 1, 0);
        let picked = web.nodes.iter().find(|node| !node.me && node.kind == Kind::Participant).unwrap().id.clone();
        let places = layout(&web, Some(&picked));
        for (node, place) in web.nodes.iter().zip(&places) {
            assert_eq!(place.named, node.me || node.id == picked, "{}", node.id);
        }
    }

    fn web_with(edges: Vec<Edge>, nodes: Vec<Node>) -> Web {
        Web { nodes, edges }
    }

    fn node(id: &str, me: bool) -> Node {
        Node { kind: Kind::Participant, id: id.to_string(), name: None, can_host: false, me }
    }

    #[test]
    fn under_the_cap_every_line_is_drawn() {
        let web = fake(20, 3, 0);
        let quiet = web.edges.iter().filter(|edge| edge.state != State::Flowing).count();
        assert!(quiet > 0 && quiet <= HELD_DRAWN_MAX);
        let shown = shown_edges(&web, None);
        assert_eq!(shown.edges, (0..web.edges.len()).collect::<Vec<_>>());
        assert_eq!(shown.hidden, 0);
    }

    #[test]
    fn over_the_cap_only_the_focus_and_whatever_flows() {
        // 400 held lines among 41 nodes, one flowing between two of them.
        let ids: Vec<String> = (0..41).map(|n| format!("{n:03}")).collect();
        let nodes: Vec<Node> = ids.iter().map(|id| node(id, id == "000")).collect();
        let mut edges = Vec::new();
        for a in 0..41 {
            for b in a + 1..41 {
                if edges.len() < 400 {
                    edges.push(Edge { from: ids[a].clone(), to: ids[b].clone(), state: State::Held });
                }
            }
        }
        let flowing = edges.iter().position(|edge| edge.from == "005" && edge.to == "006").unwrap();
        edges[flowing].state = State::Flowing;
        let web = web_with(edges, nodes);

        // Nothing selected: ours, which are the 40 from "000", and the one flowing.
        let shown = shown_edges(&web, None);
        assert_eq!(shown.edges.len(), 41);
        assert!(shown.edges.contains(&flowing));
        assert_eq!(shown.hidden, 399 - 40);
        assert!(shown.edges.iter().all(|&at| web.edges[at].state == State::Flowing || web.edges[at].from == "000"));

        // Someone selected: theirs instead.
        let shown = shown_edges(&web, Some("003"));
        let theirs = web.edges.iter().filter(|edge| edge.from == "003" || edge.to == "003").count();
        assert_eq!(shown.edges.len(), theirs + 1);
        assert_eq!(shown.hidden, 399 - theirs);
        assert!(shown.edges.contains(&flowing), "flowing is never culled");
        assert_eq!(shown.edges.len() + shown.hidden, web.edges.len());
    }

    #[test]
    fn a_big_fake_room_is_over_the_cap_and_still_draws_every_flowing_line() {
        let web = fake(100, 50, 0);
        let shown = shown_edges(&web, None);
        assert!(shown.hidden > 0);
        let flowing: Vec<usize> = (0..web.edges.len()).filter(|&at| web.edges[at].state == State::Flowing).collect();
        assert!(flowing.iter().all(|at| shown.edges.contains(at)));
    }

    #[test]
    fn the_fake_room_is_the_same_every_time_and_sorted() {
        let web = fake(100, 50, 7);
        assert_eq!(web, fake(100, 50, 7));
        assert_eq!(web.nodes.len(), 150);
        assert!(web.nodes.windows(2).all(|pair| pair[0].id < pair[1].id));
        assert_eq!(web.nodes.iter().filter(|node| node.me).count(), 1);
        assert!(web.nodes.iter().any(|node| node.can_host));
        assert!(web.edges.iter().any(|edge| edge.state == State::Flowing));
        assert!(web.edges.iter().any(|edge| edge.state == State::Held));
        assert!(web.edges.iter().any(|edge| edge.state == State::Assumed));
        // Media moves every few ticks, not every tick.
        assert_eq!(fake(10, 2, 0), fake(10, 2, 3));
        assert_ne!(fake(10, 2, 0).edges, fake(10, 2, 4).edges);
        // Without relays the room meets at us.
        let alone = fake(5, 0, 0);
        assert!(alone.nodes.iter().all(|node| node.kind == Kind::Participant));
        assert!(alone.edges.iter().any(|edge| edge.state == State::Flowing));
    }

    #[test]
    fn quiet_lines_fade_as_they_crowd_and_a_selected_one_does_not() {
        assert!(quiet_alpha(10, State::Held, false) > quiet_alpha(200, State::Held, false));
        assert!(quiet_alpha(10, State::Assumed, false) < quiet_alpha(10, State::Held, false));
        assert!(quiet_alpha(300, State::Held, true) > quiet_alpha(10, State::Held, false));
    }

    #[test]
    fn an_empty_or_lonely_web_lays_out() {
        assert!(layout(&Web::default(), None).is_empty());
        let web = web_with(Vec::new(), vec![node("a", true)]);
        let places = layout(&web, None);
        assert!(places[0].named && places[0].y < CENTRE);
    }
}
