//! Where each node of the room's web goes on the Topology tab, which lines
//! are drawn and how, and a made-up room to look at (`--fake-web`).
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
//! **Everyone on one ring, relays included.** A relay in the middle sat on
//! the straight line between any two people across from each other, so
//! someone sending straight to a host on the far side was drawn exactly like
//! someone sending through the relay. Points on one circle are in convex
//! position: no straight line between two of them passes through the middle
//! of a third. A relay outside the circle was rejected for the same reason:
//! a line from it to a far node crosses the circle at a nearer point, where
//! another node can sit. So: us at twelve o'clock, the relays together as
//! one arc centred at six, sorted by id, and the people clockwise by id
//! filling the rest, an equal share on each side.
//!
//! **A second ring only when one would make dots too small to click**, and
//! then it holds people, never us or the relays (see [`rings`]). The relays
//! stay at six o'clock however the room grows, which matters because a real
//! room has one, and it is where most lines go: on an inner ring it would
//! sit back in the middle, the very thing this layout is for. The people who
//! overflow are spread thin on the inner ring, so the long lines that cross
//! it pass between them. Tried and dropped: a zigzag, every other node a
//! step inward. Each dot then had a neighbour just inside it in the way of
//! most of its lines, and over half the lines of a hundred people bowed,
//! some by half their length.
//!
//! **Names point away from the middle**: above a node at the top, below one
//! at the bottom, and to the side of one at three or nine o'clock, where a
//! name above or below sat on the lines running down to the relay. The ring
//! is pulled in far enough for a side name to fit inside the square.
//!
//! **A line is white while media travels on it, either way**, and which way
//! is shown by motion: short bright dashes drift along it from sender to
//! receiver ([`dashes`]), in one lane down the middle for a one-way link and
//! two side by side, each drifting its own way, for a two-way one
//! ([`lanes`]). A line split into a white and a grey half was tried first
//! and read as a broken or fading line, not as a direction. Selecting a node
//! dims whatever is not on its media's paths ([`focus`]), and only what stays
//! lit moves; in a big room only the focus's paths move ([`moving`]).
//!
//! **A line never crosses a node it does not end at.** Even on one ring a
//! node has a size, so a line between two nodes not far apart grazes the
//! neighbours of either end: in a room of twenty, about one line in five.
//! Such a line, and any that crosses the inner ring at a dot, is drawn as a
//! curve bowed to the clear side ([`bows`]), as little as clears.

use std::f64::consts::{FRAC_PI_2, PI, SQRT_2, TAU};

use crate::net::arrival::Sending;
use std::collections::BTreeMap;

use crate::net::web::{self, Hop, Kind, Person, Relay, State, Web};

/// At or under this many nodes everyone is named; over it only we and the
/// selected node are, since a hundred names on the square are noise. Not
/// raised with the Topology tab's larger square: names have more room on it,
/// but a small window still lays the map out at about 280 px, where this is
/// what was judged, and no test checks one name against another.
pub const NAMED_MAX: usize = 24;

/// Over this many could-flow lines, only the focused node's are drawn: a
/// hundred people holding four sessions each is a grey fog, not a picture.
/// Lines that carry media now are always drawn.
pub const HELD_DRAWN_MAX: usize = 300;

/// The middle of the square.
const CENTRE: f64 = 0.5;
/// The largest a person's circle and a relay's square get, however few.
const PERSON_LARGE: f64 = 0.075;
const RELAY_LARGE: f64 = 0.07;
/// The smallest circle one ring may give before the second ring is used:
/// eight pixels on a 400 px square, about ninety nodes. One ring of 150
/// gives under five, a dot to aim at rather than a node; only a room too
/// big for a thin inner ring comes to that. Click targets are larger than
/// the dot where the neighbours leave room (see [`Place::hit`]).
const RING_FLOOR: f64 = 0.02;
/// How far out the inner ring sits, as part of the outer ring's radius,
/// when it has room to spare.
const INNER_SHARE: f64 = 0.6;
/// Centre to centre between neighbours, in node sizes: half a size of air.
const SPACING: f64 = 1.5;
/// How wide a name may be, as part of the square.
const LABEL_MIN: f64 = 0.12;
const LABEL_MAX: f64 = 0.3;
/// Between a node's edge and its name.
pub const NAME_GAP: f64 = 0.008;
/// The least room a name beside a node gets: what the ring is pulled in to
/// leave at three and nine o'clock.
const SIDE_LABEL: f64 = LABEL_MIN;
/// How tall a name is: a line of 10 px text, leading-tight, on a square of
/// about 280 px, about the narrowest the Topology tab is laid out at; on the
/// full 560 px it is half of this.
pub const NAME_TALL: f64 = 0.045;
/// Kept clear at the square's edge beyond the names.
const EDGE_PAD: f64 = 0.004;
/// The largest a click target gets, and how much bigger than the node it
/// tries to be: twenty pixels on a 400 px square is easy to hit.
const HIT_MIN: f64 = 0.05;
const HIT_OVER: f64 = 1.3;

/// Where one node goes. `size` is a circle's diameter or a square's side.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Place {
    pub x: f64,
    pub y: f64,
    pub size: f64,
    /// Whether its name is shown.
    pub named: bool,
    /// How wide its name may be: about the room to its neighbours.
    pub label: f64,
    /// The side of its square click target. At least [`HIT_MIN`] where the
    /// neighbours leave room, never so large that two targets overlap: a
    /// target over a neighbour's dot would take its clicks.
    pub hit: f64,
}

impl Place {
    /// Half the widest extent of its shape: a square's corner reaches
    /// further than a circle's edge.
    pub fn reach(&self, kind: Kind) -> f64 {
        match kind {
            Kind::Relay => self.size * SQRT_2 / 2.0,
            Kind::Participant => self.size / 2.0,
        }
    }
}

/// Which side of its node a name sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Above,
    Below,
    Left,
    Right,
}

/// Where a node's name goes: outside it, away from the middle of the
/// square, where the lines are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NameBox {
    pub side: Side,
    pub left: f64,
    pub width: f64,
    /// Above: its bottom edge. Below, left and right: its top edge.
    pub edge: f64,
}

/// Which way from the middle a node at `x`, `y` lies: to a side when it is
/// nearer level with the middle than above or below it.
fn side_of(x: f64, y: f64) -> Side {
    let (dx, dy) = (x - CENTRE, y - CENTRE);
    if dx.abs() > dy.abs() {
        if dx < 0.0 { Side::Left } else { Side::Right }
    } else if dy < -1e-9 {
        Side::Above
    } else {
        Side::Below
    }
}

/// Where `place`'s name goes. Above and below it is centred on the node and
/// pulled in from the square's sides rather than cut off by them; beside it,
/// it is as wide as the room to the edge allows, which the ring leaves.
pub fn name_box(place: &Place, kind: Kind) -> NameBox {
    let reach = place.reach(kind);
    let beside = match side_of(place.x, place.y) {
        Side::Left => place.x - reach - NAME_GAP - EDGE_PAD,
        Side::Right => 1.0 - EDGE_PAD - (place.x + reach + NAME_GAP),
        Side::Above | Side::Below => 0.0,
    };
    let side = match side_of(place.x, place.y) {
        Side::Left | Side::Right if beside < SIDE_LABEL - 1e-9 => {
            if place.y < CENTRE - 1e-9 { Side::Above } else { Side::Below }
        }
        side => side,
    };
    match side {
        side @ (Side::Above | Side::Below) => {
            let width = place.label.min(1.0);
            let left = (place.x - width / 2.0).clamp(0.0, 1.0 - width);
            let edge = if side == Side::Above { place.y - reach - NAME_GAP } else { place.y + reach + NAME_GAP };
            NameBox { side, left, width, edge }
        }
        Side::Left => {
            let right = place.x - reach - NAME_GAP;
            let width = place.label.min(right - EDGE_PAD).max(0.0);
            NameBox { side: Side::Left, left: right - width, width, edge: place.y - NAME_TALL / 2.0 }
        }
        Side::Right => {
            let left = place.x + reach + NAME_GAP;
            let width = place.label.min(1.0 - EDGE_PAD - left).max(0.0);
            NameBox { side: Side::Right, left, width, edge: place.y - NAME_TALL / 2.0 }
        }
    }
}

/// The places of `web`'s nodes, in its order. `selected` is named whatever
/// the count.
pub fn layout(web: &Web, selected: Option<&str>) -> Vec<Place> {
    let mut places = vec![Place::default(); web.nodes.len()];
    if web.nodes.is_empty() {
        return places;
    }
    // Already sorted by id.
    let relays: Vec<usize> = (0..web.nodes.len()).filter(|&at| web.nodes[at].kind == Kind::Relay).collect();
    let mut people: Vec<usize> = (0..web.nodes.len()).filter(|&at| web.nodes[at].kind == Kind::Participant).collect();
    // Us first, then by id: the sort is stable and the nodes are in id order.
    people.sort_by_key(|&at| !web.nodes[at].me);

    let (sequence, angles, radius, sizes, inner, inner_radius) = rings(&people, &relays);

    let size_of = |at: usize| match web.nodes[at].kind {
        Kind::Relay => sizes.relay,
        Kind::Participant => sizes.person,
    };
    let around = |angle: f64, ring: f64| (CENTRE + ring * angle.cos(), CENTRE + ring * angle.sin());
    for (&at, &angle) in sequence.iter().zip(&angles) {
        let (x, y) = around(angle, radius);
        places[at] = Place { x, y, size: size_of(at), named: false, label: sizes.label, hit: 0.0 };
    }
    for (index, &at) in inner.iter().enumerate() {
        let angle = -FRAC_PI_2 + TAU * (index as f64 + 0.5) / inner.len() as f64;
        let (x, y) = around(angle, inner_radius);
        places[at] = Place { x, y, size: size_of(at), named: false, label: sizes.label, hit: 0.0 };
    }

    // Click targets: as large as the nearest neighbour allows. Squares of
    // side d/√2 around points d apart never overlap.
    for at in 0..places.len() {
        let nearest = (0..places.len())
            .filter(|&other| other != at)
            .map(|other| distance(&places[at], &places[other]))
            .fold(f64::INFINITY, f64::min);
        let place = &mut places[at];
        let wanted = (place.reach(web.nodes[at].kind) * 2.0 * HIT_OVER).max(HIT_MIN);
        place.hit = wanted.min(nearest / SQRT_2);
    }

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

/// Who goes where: the outer ring in order with its angles, its radius, the
/// sizes, and who is on the inner ring, at what radius.
type Rings<'p> = (Vec<usize>, Vec<f64>, f64, Sizes, &'p [usize], f64);

/// One ring if its dots stay [`RING_FLOOR`] or larger. Otherwise us, the
/// relays and as many of the others, by id, as keep that size, on the outer
/// ring, and the rest on an inner one, as long as it can be spread thin
/// enough at that size for a line to pass between two of its dots: the
/// room a line keeps from each and a dot's width more. Where it cannot, an
/// inner ring would be a wall no bow gets round, and everyone goes back on
/// one ring with smaller dots.
///
/// The inner ring sits well inside the outer one, where only long lines
/// cross it, pushed out only as far as its count needs.
fn rings<'p>(people: &'p [usize], relays: &[usize]) -> Rings<'p> {
    let one = |count: usize| {
        let sequence = sequence(&people[..count], relays);
        let angles = angles_of(sequence.len(), relays, sequence.iter().position(|at| relays.contains(at)));
        let (radius, sizes) = fit(&angles, relays);
        (sequence, angles, radius, sizes)
    };
    let all = one(people.len());
    if all.3.person >= RING_FLOOR || people.len() <= 1 {
        let (sequence, angles, radius, sizes) = all;
        return (sequence, angles, radius, sizes, &[], 0.0);
    }
    let Some(outer) = (1..people.len()).rev().find(|&count| one(count).3.person >= RING_FLOOR) else {
        let (sequence, angles, radius, sizes) = all;
        return (sequence, angles, radius, sizes, &[], 0.0);
    };
    let (sequence, angles, radius, sizes) = one(outer);
    let inner = &people[outer..];
    let apart = 2.0 * (sizes.person / 2.0 + LINE_CLEAR) + sizes.person;
    let needed = if inner.len() == 1 { 0.0 } else { apart / (2.0 * (PI / inner.len() as f64).sin()) };
    let widest = radius - SPACING * (reach_of(sizes, relays) + sizes.person / 2.0) - sizes.person;
    let inner_radius = (radius * INNER_SHARE).max(needed);
    if inner_radius > widest {
        let (sequence, angles, radius, sizes) = all;
        return (sequence, angles, radius, sizes, &[], 0.0);
    }
    (sequence, angles, radius, sizes, inner, inner_radius)
}

fn distance(a: &Place, b: &Place) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// The outer ring in order from twelve o'clock: us, the first half of the
/// others, the relays, the second half.
fn sequence(people: &[usize], relays: &[usize]) -> Vec<usize> {
    let (us, others) = people.split_first().map_or((None, &[][..]), |(us, others)| (Some(*us), others));
    let right = others.len().div_ceil(2);
    let mut sequence: Vec<usize> = Vec::new();
    sequence.extend(us);
    sequence.extend(&others[..right]);
    sequence.extend(relays);
    sequence.extend(&others[right..]);
    sequence
}

/// The angle of each of `n` places round the outer ring, clockwise from
/// three o'clock as the screen's `y` runs down, so twelve o'clock is `-π/2`.
/// `first_relay` is where the relays start in the order.
///
/// Without relays it is even all round. With them, the relays are a step
/// apart centred on six o'clock, and each side's people share what is left of
/// their half evenly: with an even count of people one side has one more,
/// and its spacing is that much tighter. Keeping us at twelve and the relays
/// centred at six is worth that small unevenness.
fn angles_of(n: usize, relays: &[usize], first_relay: Option<usize>) -> Vec<f64> {
    let step = TAU / n as f64;
    let Some(first_relay) = first_relay else {
        return (0..n).map(|index| -FRAC_PI_2 + step * index as f64).collect();
    };
    let r = relays.len();
    let block = (r - 1) as f64 * step;
    let last_relay = first_relay + r - 1;
    if first_relay == 0 {
        // Only relays (a made-up room): the block, spread all round.
        return (0..n).map(|index| FRAC_PI_2 + (index as f64 - (r - 1) as f64 / 2.0) * step).collect();
    }
    // Places strictly between us and the block on each side.
    let side = PI - block / 2.0;
    let right_step = side / first_relay as f64;
    let left_step = side / (n - last_relay) as f64;
    (0..n)
        .map(|index| {
            if index < first_relay {
                -FRAC_PI_2 + right_step * index as f64
            } else if index <= last_relay {
                FRAC_PI_2 + (index - first_relay) as f64 * step - block / 2.0
            } else {
                FRAC_PI_2 + block / 2.0 + left_step * (index - last_relay) as f64
            }
        })
        .collect()
}

/// What the nodes measure.
#[derive(Clone, Copy, Debug)]
struct Sizes {
    person: f64,
    relay: f64,
    label: f64,
}

/// The outer ring's radius for these angles and the sizes on it: as large as
/// the edge allows, then pulled in by the largest reach and sized again for
/// that. Sizes only shrink as the ring does, so the second pass still fits
/// inside the first one's margin.
fn fit(angles: &[f64], relays: &[usize]) -> (f64, Sizes) {
    // Room at the square's edge for a node and a name above or below it.
    let room = CENTRE - NAME_GAP - NAME_TALL - EDGE_PAD;
    let first = ring_sizes(angles, room);
    let reach = reach_of(first, relays);
    // And beside the nodes whose names go to the side: the one furthest
    // out sets how far the ring may reach.
    // Only while everyone is named: in a larger room only we and the
    // selected node are, and the room would cost every dot a sixth of its
    // size, so a name that finds no room beside goes above or below.
    let beside = CENTRE - NAME_GAP - SIDE_LABEL - EDGE_PAD - reach;
    let named = angles.len() <= NAMED_MAX;
    let widest = angles
        .iter()
        .filter(|angle| matches!(side_of(CENTRE + angle.cos(), CENTRE + angle.sin()), Side::Left | Side::Right))
        .map(|angle| angle.cos().abs())
        .fold(0.0, f64::max);
    let radius = if named && widest > 0.0 { (room - reach).min(beside / widest) } else { room - reach };
    (radius, ring_sizes(angles, radius))
}

/// The largest sizes that keep [`SPACING`] between the closest two
/// neighbours on a ring of `radius`.
fn ring_sizes(angles: &[f64], radius: f64) -> Sizes {
    let n = angles.len();
    let nearest = if n < 2 {
        PI
    } else {
        (0..n)
            .map(|index| match angles.get(index + 1) {
                Some(next) => next - angles[index],
                None => angles[0] + TAU - angles[index],
            })
            .fold(f64::INFINITY, f64::min)
            .min(PI)
    };
    let chord = 2.0 * radius * (nearest / 2.0).sin();
    let unit = chord / SPACING;
    Sizes {
        person: unit.min(PERSON_LARGE),
        // A square's diagonal is what must clear its neighbour.
        relay: (unit / SQRT_2).min(RELAY_LARGE),
        label: chord.clamp(LABEL_MIN, LABEL_MAX),
    }
}

/// The furthest any node reaches past its centre, at these sizes.
fn reach_of(sizes: Sizes, relays: &[usize]) -> f64 {
    let relay = if relays.is_empty() { 0.0 } else { sizes.relay * SQRT_2 / 2.0 };
    relay.max(sizes.person / 2.0)
}

/// Kept between a line and a node it passes, beyond the node's reach.
const LINE_CLEAR: f64 = 0.006;
/// The bows tried, a step at a time, up to the line's length.
const BOW_STEP: f64 = 0.003;
/// Straight pieces a bowed line is checked as. A quadratic strays from them
/// by at most a 2048th of its bow, far inside [`LINE_CLEAR`].
const BOW_PIECES: usize = 32;

/// How far each of `lines` (pairs of indices into `places`) bows: the
/// control point of a quadratic Bézier sits this far off the middle of the
/// straight line, along its normal turned a quarter clockwise on screen
/// (`(-dy, dx)` with `y` down); `0.0` is straight.
///
/// A line that passes within a node's reach plus [`LINE_CLEAR`] of a node it
/// does not end at is bowed, as little as clears every such node, in steps
/// of [`BOW_STEP`]: first on the side away from most of the nodes it would
/// cross (towards the middle of the square if they are even), then the
/// other, a step further each time. The same input always gives the same
/// bows.
///
/// In chord coordinates the curve is `y = 2k·t(1 − t)` over `x = t·len`,
/// so a node only meets the part of it within its own reach along the
/// chord, and only those few pieces are checked. The cost is every node
/// against every line once (150 × 450 for the largest room drawn), then, for
/// a line that needs a bow, a check of each bow tried against the nodes it
/// could reach, nearest first, which stops at the first that fails. Every
/// edge of the 150-node fake room, 425 lines, takes under 10 ms in a release
/// build.
pub fn bows(kinds: &[Kind], places: &[Place], lines: &[(usize, usize)]) -> Vec<f64> {
    lines.iter().map(|&(a, b)| bow(kinds, places, a, b)).collect()
}

fn bow(kinds: &[Kind], places: &[Place], a: usize, b: usize) -> f64 {
    let (Some(from), Some(to)) = (places.get(a), places.get(b)) else { return 0.0 };
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let len = dx.hypot(dy);
    if len < 1e-9 {
        return 0.0;
    }
    let (ux, uy) = (dx / len, dy / len);
    let (nx, ny) = (-uy, ux);
    // Each other node in chord coordinates, with how far the line must keep.
    // Only those a bow up to the line's length could reach: such a curve
    // strays at most half its bow from the chord.
    let most = len;
    let mut near: Vec<(f64, f64, f64)> = places
        .iter()
        .zip(kinds)
        .enumerate()
        .filter(|&(at, _)| at != a && at != b)
        .filter_map(|(_, (place, &kind))| {
            let (px, py) = (place.x - from.x, place.y - from.y);
            let along = px * ux + py * uy;
            let off = px * nx + py * ny;
            let clear = place.reach(kind) + LINE_CLEAR;
            let inside = along > -clear && along < len + clear && off.abs() < clear + most / 2.0;
            inside.then_some((along, off, clear))
        })
        .collect();
    // Nearest the line first, so a bow that fails finds out at once.
    near.sort_by(|a, b| (a.1.abs() - a.2).total_cmp(&(b.1.abs() - b.2)));
    let clears = |k: f64| near.iter().all(|&(along, off, clear)| curve_keeps(k, len, along, off, clear));
    if clears(0.0) {
        return 0.0;
    }
    // Away from the side most of the crossed nodes are on; if even, towards
    // the middle of the square.
    let crossed: f64 = near
        .iter()
        .filter(|&&(along, off, clear)| !curve_keeps(0.0, len, along, off, clear))
        .map(|&(_, off, _)| off.signum())
        .sum();
    let side = if crossed != 0.0 {
        -crossed.signum()
    } else {
        let (cx, cy) = (CENTRE - (from.x + to.x) / 2.0, CENTRE - (from.y + to.y) / 2.0);
        if cx * nx + cy * ny >= 0.0 { 1.0 } else { -1.0 }
    };
    // Kept inside the square, which hides whatever leaves it.
    let inside = |k: f64| {
        let (x, y) = ((from.x + to.x) / 2.0 + nx * k / 2.0, (from.y + to.y) / 2.0 + ny * k / 2.0);
        (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)
    };
    let tried = || {
        (1..)
            .map(|step| step as f64 * BOW_STEP)
            .take_while(|&size| size <= most)
            .flat_map(|size| [side * size, -side * size])
            .filter(|&k| inside(k))
    };
    if let Some(k) = tried().find(|&k| clears(k)) {
        return k;
    }
    // Nothing clears (not seen for any edge of the rooms the tests lay
    // out): the least bad bow.
    let slack = |k: f64| {
        near.iter().map(|&(along, off, clear)| curve_distance(k, len, along, off, clear) - clear).fold(f64::INFINITY, f64::min)
    };
    tried().max_by(|&a, &b| slack(a).total_cmp(&slack(b))).unwrap_or(0.0)
}

/// Whether the curve of bow `k` over a chord of `len` keeps `clear` from a
/// point at `along`, `off` in chord coordinates.
fn curve_keeps(k: f64, len: f64, along: f64, off: f64, clear: f64) -> bool {
    curve_distance(k, len, along, off, clear) > clear
}

/// The distance from a point to the curve, checked only on the pieces within
/// `clear` of it along the chord; further than `clear` wherever none is.
fn curve_distance(k: f64, len: f64, along: f64, off: f64, clear: f64) -> f64 {
    let point = |t: f64| (t * len, 2.0 * k * t * (1.0 - t));
    let first = (((along - clear) / len * BOW_PIECES as f64).floor().max(0.0)) as usize;
    let last = (((along + clear) / len * BOW_PIECES as f64).ceil().min(BOW_PIECES as f64)) as usize;
    let mut nearest = f64::INFINITY;
    for piece in first..last {
        let (ax, ay) = point(piece as f64 / BOW_PIECES as f64);
        let (bx, by) = point((piece + 1) as f64 / BOW_PIECES as f64);
        nearest = nearest.min(to_segment(along, off, ax, ay, bx, by));
    }
    nearest
}

fn to_segment(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let (dx, dy) = (bx - ax, by - ay);
    let length = dx * dx + dy * dy;
    let t = if length > 0.0 { (((px - ax) * dx + (py - ay) * dy) / length).clamp(0.0, 1.0) } else { 0.0 };
    (px - ax - t * dx).hypot(py - ay - t * dy)
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

/// What stays at full strength while a node is selected, by index into a
/// web's nodes and edges: everything on a path of media that passes it
/// (theirs, out and in, or carried by them), and every link of theirs with
/// the node at its far end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Focus {
    pub nodes: Vec<bool>,
    pub edges: Vec<bool>,
}

pub fn focus(web: &Web, selected: usize) -> Focus {
    let mut nodes = vec![false; web.nodes.len()];
    let mut edges = vec![false; web.edges.len()];
    let ends: Vec<(usize, usize)> = web
        .edges
        .iter()
        .map(|edge| (web.index_of(&edge.from).unwrap_or(usize::MAX), web.index_of(&edge.to).unwrap_or(usize::MAX)))
        .collect();
    let edge_of = |a: usize, b: usize| ends.binary_search(&(a.min(b), a.max(b))).ok();
    if let Some(node) = nodes.get_mut(selected) {
        *node = true;
    }
    for flow in web.flows.iter().filter(|flow| flow.path.contains(&selected)) {
        for &at in &flow.path {
            nodes[at] = true;
        }
        for pair in flow.path.windows(2) {
            if let Some(at) = edge_of(pair[0], pair[1]) {
                edges[at] = true;
            }
        }
    }
    for (at, &(a, b)) in ends.iter().enumerate() {
        if a == selected || b == selected {
            edges[at] = true;
            for end in [a, b] {
                if let Some(node) = nodes.get_mut(end) {
                    *node = true;
                }
            }
        }
    }
    Focus { nodes, edges }
}

/// How thick a carrying line is for `streams` people's media, both ways: 1.5 for one
/// to 3 for twenty and more, in steps by order of magnitude, so a busy
/// link reads at a glance and a count need not be read.
pub fn stream_width(streams: u32) -> f64 {
    match streams {
        0 | 1 => 1.5,
        2..=4 => 2.0,
        5..=19 => 2.5,
        _ => 3.0,
    }
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

/// How fast a dash drifts, in logical pixels a second, the same on every
/// line and at every map size, so a long line is not a fast one: brisk, a
/// line across the map in about six seconds, and still slow enough to follow.
///
/// **It never seems to step backwards** while a frame moves a dash less than
/// half of [`DASH_PERIOD`] (past half, the nearest dash in the next frame is
/// the one behind, and the eye takes that one), and it reads as one thing
/// gliding while a frame moves it less than its own length, so where it was
/// and where it is overlap. At 50 px/s that is under 1 px a frame at 60 fps and
/// 2.5 px at a degraded 20, against 8 and 6
/// (`dashes_cannot_seem_to_step_backwards`).
pub const DASH_SPEED: f64 = 50.0;
/// A dash, and the gap after it, in logical pixels. A dash shorter than its
/// gap reads as a thing moving rather than as a dashed line; 6 px rather
/// than 5 keeps a dash longer than a 20 fps frame's step at this speed. At
/// 16 px from one to the next, any line with 22 px between its nodes' edges
/// always shows a whole dash, and one across a 560 px map about thirty-five.
pub const DASH_LONG: f64 = 6.0;
pub const DASH_GAP: f64 = 10.0;
pub const DASH_PERIOD: f64 = DASH_LONG + DASH_GAP;
/// How far a two-way link's lanes sit either side of its middle, in logical
/// pixels: as wide as the thickest line is, so the two read as two rails.
pub const LANE_APART: f64 = 1.5;
/// Kept between a dash and a node's edge, in logical pixels.
pub const DASH_CLEAR: f64 = 2.0;
/// Over this many carrying lines, only the focus's paths move (the selected
/// node's, or ours). For legibility, not cost: a hundred-odd lines all moving
/// at once is a shimmer, not a direction, and faster dashes shimmer sooner,
/// not later; in a room with a relay it is about a hundred people, where a
/// person looks for their own media anyway. Kept at 120 with the larger map:
/// the lines all meet at the relay and the same few hosts whatever the size,
/// so the crowding there is what decides it, and that does not scale with the
/// square. (What a frame costs is measured anyway: all 228 of the 150-person
/// fake room's carrying lines' dashes are worked out in a few milliseconds in
/// a debug build, `a_frame_of_the_largest_room_is_bounded`.)
pub const MOVING_MAX: usize = 120;

/// Which way along a link a lane's dashes drift: from its `from` end to its
/// `to` end, or back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    Forth,
    Back,
}

/// The lanes a link's dashes drift in, each with how far it sits to the
/// right of its own direction of travel, in logical pixels. A one-way link
/// has one, down the middle; a two-way one two, each to its own right, so
/// they sit either side of the middle; a link with nothing known to travel
/// on it none. Right of travel is the usual side to pass on, so the two
/// lanes are where a reader expects them.
pub fn lanes(forth: u32, back: u32) -> Vec<(Way, f64)> {
    match (forth > 0, back > 0) {
        (true, true) => vec![(Way::Forth, LANE_APART), (Way::Back, LANE_APART)],
        (true, false) => vec![(Way::Forth, 0.0)],
        (false, true) => vec![(Way::Back, 0.0)],
        (false, false) => Vec::new(),
    }
}

/// A quadratic curve in pixels, from the sender's centre to the receiver's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bend {
    pub from: (f64, f64),
    pub control: (f64, f64),
    pub to: (f64, f64),
}

/// A dash's two ends, from the sender's side.
pub type Dash = ((f64, f64), (f64, f64));

/// The dashes on one lane, as the two ends of each, in the same pixels as
/// the curve `bend`, with each dash kept `clear` (from, to) along the curve
/// from its two centres, `offset` to the right of travel, and
/// `travelled` logical pixels along since the drift began. `scale` is device
/// pixels to the logical pixel the dash sizes are in.
///
/// The dashes are spaced [`DASH_PERIOD`] along the curve, measured from the
/// sender's edge, and move with `travelled` towards the receiver; one a
/// period further on is the same picture, so the drift wraps without a jump.
/// A dash entering at the sender's edge or leaving at the receiver's is cut
/// there rather than drawn over the node. Each dash is drawn straight
/// between its ends, which on a bow at most a few pixels long is the curve.
pub fn dashes(bend: Bend, (clear_from, clear_to): (f64, f64), offset: f64, travelled: f64, scale: f64) -> Vec<Dash> {
    let Bend { from, control, to } = bend;
    /// Straight pieces the curve is measured in: a bowed line here strays
    /// from them by well under a pixel.
    const PIECES: usize = 32;
    let at = |t: f64| {
        let u = 1.0 - t;
        (
            u * u * from.0 + 2.0 * u * t * control.0 + t * t * to.0,
            u * u * from.1 + 2.0 * u * t * control.1 + t * t * to.1,
        )
    };
    let mut lengths = [0.0f64; PIECES + 1];
    let mut last = from;
    for (piece, length) in lengths.iter_mut().enumerate().skip(1) {
        let next = at(piece as f64 / PIECES as f64);
        *length = (next.0 - last.0).hypot(next.1 - last.1);
        last = next;
    }
    for piece in 1..=PIECES {
        lengths[piece] += lengths[piece - 1];
    }
    let total = lengths[PIECES];
    let usable = total - clear_from - clear_to;
    if !usable.is_finite() || usable <= 0.0 || scale <= 0.0 {
        return Vec::new();
    }
    // A point `s` along the curve, moved `offset` to the right of travel
    // (with `y` down, the right of `(dx, dy)` is `(-dy, dx)`).
    let point = |s: f64| {
        let piece = lengths.partition_point(|&length| length <= s).clamp(1, PIECES);
        let (start, end) = (lengths[piece - 1], lengths[piece]);
        let within = if end > start { (s - start) / (end - start) } else { 0.0 };
        let t = (piece as f64 - 1.0 + within.clamp(0.0, 1.0)) / PIECES as f64;
        let (x, y) = at(t);
        let dx = 2.0 * (1.0 - t) * (control.0 - from.0) + 2.0 * t * (to.0 - control.0);
        let dy = 2.0 * (1.0 - t) * (control.1 - from.1) + 2.0 * t * (to.1 - control.1);
        let along = dx.hypot(dy).max(f64::MIN_POSITIVE);
        (x - dy / along * offset, y + dx / along * offset)
    };
    let period = DASH_PERIOD * scale;
    let long = DASH_LONG * scale;
    let mut out = Vec::new();
    // The first dash starts up to a period before the sender's edge, so one
    // is always on its way in.
    let mut start = (travelled * scale).rem_euclid(period) - period;
    while start < usable {
        let (a, b) = (start.max(0.0), (start + long).min(usable));
        if b > a {
            out.push((point(clear_from + a), point(clear_from + b)));
        }
        start += period;
    }
    out
}

/// Which of the drawn lines (`shown.edges`, in order) have drifting dashes:
/// none while the switch is off (`on`); otherwise each carrying line with a
/// direction known, unless a node is selected and it is off that node's
/// paths (it is dimmed, and dimmed lines keep still), or the room has over
/// [`MOVING_MAX`] such lines and it is off the paths of the selected node,
/// or ours with nothing selected.
pub fn moving(web: &Web, shown: &Shown, selected: Option<usize>, on: bool) -> Vec<bool> {
    if !on {
        return vec![false; shown.edges.len()];
    }
    let carrying = |at: usize| {
        let edge = &web.edges[at];
        edge.state == State::Flowing && (edge.carries_forth() || edge.carries_back())
    };
    let count = shown.edges.iter().filter(|&&at| carrying(at)).count();
    let me = web.nodes.iter().position(|node| node.me);
    let lit = match (selected, count > MOVING_MAX) {
        (Some(at), _) => Some(focus(web, at)),
        (None, true) => me.map(|at| focus(web, at)),
        (None, false) => None,
    };
    // Over the cap with nobody to focus on, nothing moves.
    let crowded_and_blind = lit.is_none() && count > MOVING_MAX;
    shown
        .edges
        .iter()
        .map(|&at| carrying(at) && !crowded_and_blind && lit.as_ref().is_none_or(|lit| lit.edges[at]))
        .collect()
}

/// A room to look at without one (`--fake-web`): `participants` people (us
/// among them), `relays` relay servers, some hosts, and media that moves
/// every few seconds of `tick`, so that a change and its repaint are seen.
/// The same arguments always give the same web.
///
/// Everyone but the older builds reports, through the same document a real
/// room sends ([`web::say_routes`] and back), so the fake also measures it.
/// It has two-way links (everyone and the relay), one-way ones (some take
/// hosts' media straight from them while sending elsewhere), a sender whose
/// receivers see its media arrive through a host while it says the relay,
/// a relay nobody names, and someone who receives one person not at all.
pub fn fake(participants: usize, relays: usize, tick: u64) -> Web {
    let (people, servers, venue, reported) = fake_room(participants, relays, tick);
    web::build_reported(&people, &servers, venue.as_deref(), &reported)
}

/// A hop nobody claims: the relay the fake's fourth person goes through.
const FAKE_UNNAMED: u64 = 9_999;
/// The meeting place's hop, which every reporter on it claims.
const FAKE_VENUE: u64 = 7;

/// The fake room's parts, before they are built into a web.
type FakeRoom = (Vec<Person>, Vec<Relay>, Option<String>, BTreeMap<String, web::Reported>);

fn fake_room(participants: usize, relays: usize, tick: u64) -> FakeRoom {
    // Ids that look like endpoint ids but do not sort in the order made, so
    // us being first is the layout's doing, not the ids'.
    let id = |n: usize| {
        let mixed = (n as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        format!("{mixed:016x}").repeat(4)
    };
    let relay_ids: Vec<String> = (0..relays).map(|n| format!("relay-{n:02}")).collect();
    let is_host = |n: usize| n % 5 == 1;
    let hosts: Vec<usize> = (0..participants).filter(|&n| is_host(n)).collect();
    let phase = (tick / 4) as usize;
    // Someone on an older build says nothing and reports nothing.
    let older = |n: usize| n % 11 == 7;

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
                Some(Sending::Host(id(hosts[(n / 3 + phase) % hosts.len()])))
            } else if relays > 1 && (n + phase) % 5 == 1 {
                Some(Sending::Host(relay_ids[1 + (n + phase) % (relays - 1)].clone()))
            } else {
                Some(Sending::Relay)
            };
            if !is_host(n) {
                for k in 0..hosts.len().min(4) {
                    held.push(id(hosts[(n + k) % hosts.len()]));
                }
            }
            if relays > 1 {
                held.push(relay_ids[1 + n % (relays - 1)].clone());
            }
            let (via, held) = if older(n) { (None, Vec::new()) } else { (via, held) };
            Person {
                id: id(n),
                name: if me { None } else { Some(format!("Person {n}")) },
                can_host: is_host(n),
                me,
                via,
                held,
                hop: is_host(n).then_some(1_000 + n as u64),
            }
        })
        .collect();
    let servers: Vec<Relay> = relay_ids
        .iter()
        .enumerate()
        .map(|(n, id)| Relay { id: id.clone(), name: Some(format!("Relay {}", n + 1)) })
        .collect();
    let venue = match (relay_ids.first(), people.first()) {
        (Some(relay), _) => Some(relay.clone()),
        (None, Some(us)) => Some(us.id.clone()),
        (None, None) => None,
    };

    let roster: Vec<&str> = people.iter().map(|person| person.id.as_str()).collect();
    let gate = |n: usize| 2_000 + n as u64;
    // Whose media a receiver takes straight from the hosts it holds, while
    // its own goes elsewhere: the one-way links.
    let straight_from_hosts = |n: usize| !is_host(n) && (n + phase) % 9 == 2;
    let mut reported = BTreeMap::new();
    for (r, receiver) in people.iter().enumerate().filter(|&(r, _)| !older(r)) {
        let mut paths = BTreeMap::new();
        for (s, sender) in people.iter().enumerate().filter(|&(s, _)| s != r) {
            let on_relay = |hops: Vec<Hop>| if relays > 0 { hops } else { Vec::new() };
            let path = match &sender.via {
                _ if s % 13 == 5 && r == (s + 1) % participants => None,
                _ if is_host(s) && receiver.held.contains(&sender.id) && straight_from_hosts(r) => Some(Vec::new()),
                Some(Sending::Host(host)) if receiver.via.as_ref() == Some(&Sending::Host(host.clone())) => {
                    Some(vec![Hop::Person(host.clone())])
                }
                Some(Sending::Host(host)) if host.starts_with("relay-") => Some(vec![Hop::Unnamed(gate(s)), Hop::Venue]),
                Some(Sending::Host(host)) => Some(on_relay(vec![Hop::Person(host.clone()), Hop::Unnamed(FAKE_VENUE)])),
                // Says the relay; arrives through the first host.
                Some(Sending::Relay) if s == 2 && !hosts.is_empty() => {
                    Some(on_relay(vec![Hop::Person(id(hosts[0])), Hop::Unnamed(FAKE_VENUE)]))
                }
                Some(Sending::Relay) if s == 4 => Some(on_relay(vec![Hop::Unnamed(gate(s)), Hop::Unnamed(FAKE_UNNAMED), Hop::Venue])),
                // A host has no gates; anyone else's first hop is theirs.
                Some(Sending::Relay) if is_host(s) => Some(on_relay(vec![Hop::Unnamed(FAKE_VENUE)])),
                Some(Sending::Relay) => Some(on_relay(vec![Hop::Unnamed(gate(s)), Hop::Unnamed(FAKE_VENUE)])),
                Some(Sending::Guests) => Some(Vec::new()),
                // Some older builds' media is reported, so their guessed
                // line is confirmed; the rest stay guessed.
                None if s % 22 == 7 => None,
                None => Some(on_relay(vec![Hop::Venue])),
            };
            paths.insert(sender.id.clone(), path);
        }
        let routes = web::Routes {
            origin: 1_000 + r as u64,
            gates: if is_host(r) { Vec::new() } else { vec![gate(r)] },
            venue: if relays > 0 { vec![FAKE_VENUE] } else { Vec::new() },
            paths,
        };
        let document = web::say_routes(&routes, &roster);
        if let Some(report) = web::read_routes(&document, &receiver.id) {
            reported.insert(receiver.id.clone(), report);
        }
    }
    (people, servers, venue, reported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::web::{Edge, Node};

    /// Every pairing the layout is held to.
    const PEOPLE: [usize; 5] = [1, 2, 5, 20, 100];
    const RELAYS: [usize; 4] = [0, 1, 5, 50];

    fn me_of(web: &Web) -> usize {
        web.nodes.iter().position(|node| node.me).unwrap()
    }

    /// Clockwise from twelve o'clock, in `[0, 2π)`.
    fn turn(place: &Place) -> f64 {
        ((place.y - CENTRE).atan2(place.x - CENTRE) + FRAC_PI_2).rem_euclid(TAU)
    }

    fn from_centre(place: &Place) -> f64 {
        (place.x - CENTRE).hypot(place.y - CENTRE)
    }

    fn kinds(web: &Web) -> Vec<Kind> {
        web.nodes.iter().map(|node| node.kind).collect()
    }

    /// The drawn lines as index pairs.
    fn drawn(web: &Web, selected: Option<&str>) -> Vec<(usize, usize)> {
        let index_of = |id: &str| web.nodes.iter().position(|node| node.id == id).unwrap();
        shown_edges(web, selected)
            .edges
            .iter()
            .map(|&at| (index_of(&web.edges[at].from), index_of(&web.edges[at].to)))
            .collect()
    }

    #[test]
    fn the_same_web_lays_out_the_same() {
        let web = fake(30, 3, 0);
        assert_eq!(layout(&web, None), layout(&web, None));
        // Edges change nothing about where anyone is.
        let moved = fake(30, 3, 40);
        assert_ne!(web.edges, moved.edges, "the fake's media moves");
        assert_eq!(layout(&web, None), layout(&moved, None));
        let lines = drawn(&web, None);
        let places = layout(&web, None);
        assert_eq!(bows(&kinds(&web), &places, &lines), bows(&kinds(&web), &places, &lines));
    }

    #[test]
    fn we_are_at_twelve_and_the_rest_follow_by_id_clockwise() {
        for relays in RELAYS {
            let web = fake(8, relays, 0);
            let places = layout(&web, None);
            let me = &places[me_of(&web)];
            assert!((me.x - CENTRE).abs() < 1e-9 && me.y < CENTRE, "{me:?}");
            let others: Vec<usize> =
                (0..web.nodes.len()).filter(|&at| web.nodes[at].kind == Kind::Participant && !web.nodes[at].me).collect();
            assert!(others.windows(2).all(|pair| web.nodes[pair[0]].id < web.nodes[pair[1]].id));
            let angles: Vec<f64> = others.iter().map(|&at| turn(&places[at])).collect();
            assert!(angles.windows(2).all(|pair| pair[0] < pair[1]), "{relays}: {angles:?}");
            assert!(angles[0] > 0.0);
        }
    }

    #[test]
    fn relays_are_one_arc_centred_at_six_in_id_order() {
        for participants in PEOPLE {
            for relays in RELAYS.into_iter().filter(|&relays| relays > 0) {
                let web = fake(participants, relays, 0);
                let places = layout(&web, None);
                let what = format!("{participants} people, {relays} relays");
                // Round the outer ring from twelve: who is where.
                let outer = places.iter().map(from_centre).fold(0.0, f64::max);
                let mut round: Vec<(f64, usize)> = (0..web.nodes.len())
                    .filter(|&at| (from_centre(&places[at]) - outer).abs() < 1e-9)
                    .map(|at| (turn(&places[at]), at))
                    .collect();
                round.sort_by(|a, b| a.0.total_cmp(&b.0));
                let is_relay: Vec<bool> = round.iter().map(|&(_, at)| web.nodes[at].kind == Kind::Relay).collect();
                let first = is_relay.iter().position(|&relay| relay).unwrap();
                let last = is_relay.iter().rposition(|&relay| relay).unwrap();
                assert!(is_relay[first..=last].iter().all(|&relay| relay), "{what}: not contiguous");
                // In id order, clockwise.
                let ids: Vec<&str> = round[first..=last].iter().map(|&(_, at)| web.nodes[at].id.as_str()).collect();
                assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{what}: {ids:?}");
                // Centred on six o'clock (half a turn from twelve).
                let middle = (round[first].0 + round[last].0) / 2.0;
                assert!((middle - PI).abs() < 1e-9, "{what}: centred at {middle}");
            }
        }
    }

    #[test]
    fn nothing_overlaps_and_everything_fits_names_included() {
        for participants in PEOPLE {
            for relays in RELAYS {
                let web = fake(participants, relays, 0);
                // Everyone named, as if each were selected in turn.
                let places: Vec<Place> =
                    web.nodes.iter().enumerate().map(|(at, node)| layout(&web, Some(&node.id))[at]).collect();
                let what = format!("{participants} people, {relays} relays");
                for (a, (node, place)) in web.nodes.iter().zip(&places).enumerate() {
                    let reach = place.reach(node.kind);
                    assert!(place.x - reach >= 0.0 && place.x + reach <= 1.0, "{what}: {place:?}");
                    assert!(place.y - reach >= 0.0 && place.y + reach <= 1.0, "{what}: {place:?}");
                    assert!(place.named);
                    let name = name_box(place, node.kind);
                    assert!(name.left >= 0.0 && name.left + name.width <= 1.0 + 1e-12, "{what}: {name:?}");
                    let (top, bottom) = match name.side {
                        Side::Above => (name.edge - NAME_TALL, name.edge),
                        Side::Below | Side::Left | Side::Right => (name.edge, name.edge + NAME_TALL),
                    };
                    assert!(top >= 0.0 && bottom <= 1.0, "{what}: {name:?}");
                    // Outside the node, on the side away from the middle.
                    match name.side {
                        Side::Above => assert!(name.edge <= place.y - reach && place.y < CENTRE, "{what}: {name:?}"),
                        Side::Below => assert!(name.edge >= place.y + reach && place.y >= CENTRE - 1e-9, "{what}: {name:?}"),
                        Side::Left => assert!(name.left + name.width <= place.x - reach && place.x < CENTRE, "{what}: {name:?}"),
                        Side::Right => assert!(name.left >= place.x + reach && place.x > CENTRE, "{what}: {name:?}"),
                    }
                    // A name beside an outer node has at least the room the
                    // ring was pulled in for.
                    if matches!(name.side, Side::Left | Side::Right) && rings_of(&places).len() == 1 {
                        assert!(name.width >= SIDE_LABEL - 1e-9, "{what}: {name:?}");
                    }
                    for (other, other_place) in web.nodes.iter().zip(&places).skip(a + 1) {
                        let room = distance(place, other_place) - reach - other_place.reach(other.kind);
                        assert!(room > 0.0, "{what}: {} and {} overlap by {}", node.id, other.id, -room);
                        // Click targets, squares, never overlap.
                        let apart = (place.x - other_place.x).abs().max((place.y - other_place.y).abs());
                        assert!(apart >= (place.hit + other_place.hit) / 2.0 - 1e-12, "{what}: targets overlap");
                    }
                    assert!(place.hit >= place.size, "{what}: a target smaller than its dot");
                }
            }
        }
    }

    /// The radii nodes sit at, each with the angles there, sorted.
    fn rings_of(places: &[Place]) -> Vec<(f64, Vec<f64>)> {
        let mut rings: Vec<(f64, Vec<f64>)> = Vec::new();
        for place in places {
            let radius = from_centre(place);
            match rings.iter_mut().find(|(r, _)| (r - radius).abs() < 1e-6) {
                Some((_, angles)) => angles.push(turn(place)),
                None => rings.push((radius, vec![turn(place)])),
            }
        }
        for (_, angles) in &mut rings {
            angles.sort_by(f64::total_cmp);
        }
        rings
    }

    #[test]
    fn one_ring_until_dots_would_be_too_small_then_two() {
        for participants in PEOPLE {
            for relays in RELAYS {
                let web = fake(participants, relays, 0);
                let places = layout(&web, None);
                let rings = rings_of(&places).len();
                let smallest = web
                    .nodes
                    .iter()
                    .zip(&places)
                    .filter(|(node, _)| node.kind == Kind::Participant)
                    .map(|(_, place)| place.size)
                    .fold(1.0, f64::min);
                let what = format!("{participants} people, {relays} relays");
                // A hundred people and up to a few relays overflow one ring;
                // with fifty relays as well, the inner ring would be too
                // crowded for lines to pass, so it is one ring of small dots.
                let two = participants == 100 && relays < 50;
                assert_eq!(rings, if two { 2 } else { 1 }, "{what}");
                if two || web.nodes.len() <= 80 {
                    assert!(smallest >= RING_FLOOR, "{what}: {smallest}");
                }
                if two {
                    // Us and the relays stay outside.
                    let outer = places.iter().map(from_centre).fold(0.0, f64::max);
                    for (node, place) in web.nodes.iter().zip(&places) {
                        if node.me || node.kind == Kind::Relay {
                            assert!((from_centre(place) - outer).abs() < 1e-9, "{what}: {}", node.id);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_ring_is_evenly_spaced_before_and_after_someone_joins() {
        // Without relays: exactly even, whatever the count.
        for count in [2, 6, 7, 40, 41] {
            let places = layout(&fake(count, 0, 0), None);
            let rings = rings_of(&places);
            assert_eq!(rings.len(), 1);
            let angles = &rings[0].1;
            let step = TAU / count as f64;
            for pair in angles.windows(2) {
                assert!((pair[1] - pair[0] - step).abs() < 1e-9, "{count}: {angles:?}");
            }
        }
        // With relays, people on each side of them are evenly spaced, and
        // with an odd count of people everyone is.
        for people in [5, 6, 21, 22] {
            for relays in [1, 3] {
                let web = fake(people, relays, 0);
                let places = layout(&web, None);
                let mut round: Vec<f64> = places.iter().map(turn).collect();
                round.sort_by(f64::total_cmp);
                round.push(TAU);
                let gaps: Vec<f64> = round.windows(2).map(|pair| pair[1] - pair[0]).collect();
                let even = TAU / web.nodes.len() as f64;
                if people % 2 == 1 {
                    assert!(gaps.iter().all(|gap| (gap - even).abs() < 1e-9), "{people}, {relays}: {gaps:?}");
                } else {
                    // The tighter side is within a sixth of the even step.
                    assert!(gaps.iter().all(|gap| *gap > even * 5.0 / 6.0), "{people}, {relays}: {gaps:?}");
                }
            }
        }
    }

    #[test]
    fn sizes_stay_within_their_limits() {
        for participants in PEOPLE {
            for relays in RELAYS {
                let web = fake(participants, relays, 0);
                for (node, place) in web.nodes.iter().zip(layout(&web, None)) {
                    match node.kind {
                        // Eight pixels on a 400 px square, or nearly five
                        // for the hundred and fifty of the largest fake room.
                        Kind::Participant => {
                            let least = if web.nodes.len() > 140 { 0.012 } else { RING_FLOOR };
                            assert!((least..=PERSON_LARGE).contains(&place.size), "{place:?}");
                        }
                        // A square's diagonal is a circle's diameter.
                        Kind::Relay => {
                            let least = if web.nodes.len() > 140 { 0.012 } else { RING_FLOOR } / SQRT_2;
                            assert!((least..=RELAY_LARGE).contains(&place.size), "{place:?}");
                        }
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

    /// The nearest a quadratic from `a` to `b` bowed by `k` comes to `point`,
    /// sampled along its length.
    fn nearest_on_curve(a: &Place, b: &Place, k: f64, point: &Place) -> f64 {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = dx.hypot(dy);
        let (nx, ny) = (-dy / len, dx / len);
        let control = ((a.x + b.x) / 2.0 + nx * k, (a.y + b.y) / 2.0 + ny * k);
        (0..=400)
            .map(|step| {
                let t = step as f64 / 400.0;
                let x = (1.0 - t).powi(2) * a.x + 2.0 * t * (1.0 - t) * control.0 + t * t * b.x;
                let y = (1.0 - t).powi(2) * a.y + 2.0 * t * (1.0 - t) * control.1 + t * t * b.y;
                (x - point.x).hypot(y - point.y)
            })
            .fold(f64::INFINITY, f64::min)
    }

    #[test]
    fn no_line_crosses_a_node_it_does_not_end_at() {
        let mut bowed = 0;
        let mut total = 0;
        for participants in PEOPLE {
            for relays in RELAYS {
                let web = fake(participants, relays, 0);
                let what = format!("{participants} people, {relays} relays");
                // Every edge, not only those drawn unselected: whoever is
                // selected, theirs are a subset.
                let index_of = |id: &str| web.nodes.iter().position(|node| node.id == id).unwrap();
                let every: Vec<(usize, usize)> =
                    web.edges.iter().map(|edge| (index_of(&edge.from), index_of(&edge.to))).collect();
                assert!(drawn(&web, None).iter().all(|line| every.contains(line)));
                {
                    let places = layout(&web, None);
                    let lines = every;
                    let bent = bows(&kinds(&web), &places, &lines);
                    for (&(a, b), &k) in lines.iter().zip(&bent) {
                        total += 1;
                        if k != 0.0 {
                            bowed += 1;
                        }
                        for (at, (node, place)) in web.nodes.iter().zip(&places).enumerate() {
                            if at == a || at == b {
                                continue;
                            }
                            let gap = nearest_on_curve(&places[a], &places[b], k, place);
                            assert!(
                                gap > place.reach(node.kind),
                                "{what}: {} to {} (bow {k}) passes {} at {gap}",
                                web.nodes[a].id,
                                web.nodes[b].id,
                                node.id
                            );
                        }
                    }
                }
            }
        }
        // The rule is exercised, and most lines stay straight.
        assert!(bowed > 0 && bowed * 2 < total, "{bowed} of {total} bowed");
    }

    #[test]
    fn a_line_with_nothing_in_its_way_is_straight_and_bows_go_the_clear_way() {
        // Three on one ring, nothing between any two.
        let web = fake(3, 0, 0);
        let places = layout(&web, None);
        assert_eq!(bows(&kinds(&web), &places, &[(0, 1), (1, 2), (0, 2)]), vec![0.0; 3]);
        // A node right on the line: the bow clears it, and is small.
        let at = |x: f64, y: f64| Place { x, y, size: 0.04, label: LABEL_MIN, ..Place::default() };
        let places = vec![at(0.2, 0.5), at(0.8, 0.5), at(0.5, 0.49)];
        let all = [Kind::Participant; 3];
        let k = bows(&all, &places, &[(0, 1)])[0];
        assert!(nearest_on_curve(&places[0], &places[1], k, &places[2]) > places[2].reach(Kind::Participant));
        // The node is a hair above the line (smaller y), so the bow goes
        // down: the normal of a line running right points down the screen.
        assert!(k > 0.0 && k < 0.1, "{k}");
    }

    #[test]
    fn bowing_the_largest_room_is_cheap() {
        let web = fake(100, 50, 0);
        let places = layout(&web, None);
        let index_of = |id: &str| web.nodes.iter().position(|node| node.id == id).unwrap();
        // Every edge, not only the drawn ones: well past the 450 at most drawn.
        let lines: Vec<(usize, usize)> = web.edges.iter().map(|edge| (index_of(&edge.from), index_of(&edge.to))).collect();
        let started = std::time::Instant::now();
        let _ = bows(&kinds(&web), &places, &lines);
        // Generous for a debug build on a busy machine; a release build is
        // a few milliseconds.
        assert!(started.elapsed() < std::time::Duration::from_millis(500), "{} lines: {:?}", lines.len(), started.elapsed());
    }

    #[test]
    fn everyone_is_named_up_to_the_limit_and_then_only_us_and_the_selection() {
        // With the relay nobody names that the fake's fifth person uses.
        let web = fake(NAMED_MAX - 2, 1, 0);
        assert_eq!(web.nodes.len(), NAMED_MAX);
        assert!(layout(&web, None).iter().all(|place| place.named));

        let web = fake(NAMED_MAX - 1, 1, 0);
        let picked = web.nodes.iter().find(|node| !node.me && node.kind == Kind::Participant).unwrap().id.clone();
        let places = layout(&web, Some(&picked));
        for (node, place) in web.nodes.iter().zip(&places) {
            assert_eq!(place.named, node.me || node.id == picked, "{}", node.id);
        }
    }

    fn web_with(edges: Vec<Edge>, nodes: Vec<Node>) -> Web {
        Web { nodes, edges, flows: Vec::new() }
    }

    fn node(id: &str, me: bool) -> Node {
        Node { kind: Kind::Participant, id: id.to_string(), name: None, can_host: false, me, says: None }
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
                    edges.push(Edge { from: ids[a].clone(), to: ids[b].clone(), state: State::Held, forth: 0, back: 0 });
                }
            }
        }
        let flowing = edges.iter().position(|edge| edge.from == "005" && edge.to == "006").unwrap();
        edges[flowing].state = State::Flowing;
        edges[flowing].forth = 1;
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
        // And the relay nobody names.
        assert_eq!(web.nodes.len(), 151);
        assert_eq!(web.nodes.iter().filter(|node| web::is_unnamed(&node.id)).count(), 1);
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

    /// Dashes on a straight line along `x`, 300 px between centres, each
    /// node 10 px in reach.
    fn straight(offset: f64, travelled: f64) -> Vec<Dash> {
        dashes(Bend { from: (0.0, 0.0), control: (150.0, 0.0), to: (300.0, 0.0) }, (10.0, 10.0), offset, travelled, 1.0)
    }

    fn close(a: &[Dash], b: &[Dash]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| {
                [(x.0.0, y.0.0), (x.0.1, y.0.1), (x.1.0, y.1.0), (x.1.1, y.1.1)].iter().all(|(p, q)| (p - q).abs() < 1e-6)
            })
    }

    #[test]
    fn dashes_are_evenly_spaced_and_stay_between_the_edges() {
        for travelled in [0.0, 3.3, 7.0, 13.99, 1_000.5] {
            let line = straight(0.0, travelled);
            assert!(!line.is_empty());
            for &((x0, y0), (x1, y1)) in &line {
                assert!((10.0..=290.0).contains(&x0) && (10.0..=290.0).contains(&x1), "{x0}..{x1}");
                assert!(x1 > x0, "drawn from the sender's side");
                assert!(y0.abs() < 1e-9 && y1.abs() < 1e-9);
                assert!(x1 - x0 <= DASH_LONG + 1e-9);
            }
            // Whole dashes (not cut at an edge) are a period apart.
            let whole: Vec<f64> = line.iter().filter(|(a, b)| (b.0 - a.0 - DASH_LONG).abs() < 1e-6).map(|(a, _)| a.0).collect();
            assert!(whole.len() >= 16, "{}", whole.len());
            for pair in whole.windows(2) {
                assert!((pair[1] - pair[0] - DASH_PERIOD).abs() < 1e-6, "{pair:?}");
            }
        }
    }

    #[test]
    fn dashes_cannot_seem_to_step_backwards() {
        // At a display's 60 fps and at a degraded 20, a frame's step is under
        // half the spacing (or the nearest dash next frame is the one behind)
        // and under a dash's own length (so where it was and is overlap).
        for fps in [60.0, 20.0] {
            let step = DASH_SPEED / fps;
            assert!(step < DASH_PERIOD / 2.0, "{fps} fps: {step} px a frame against a {DASH_PERIOD} px period");
            assert!(step < DASH_LONG, "{fps} fps: {step} px a frame against a {DASH_LONG} px dash");
        }
    }

    #[test]
    fn dashes_drift_towards_the_receiver_and_wrap_after_a_period() {
        let now = straight(0.0, 20.0);
        let later = straight(0.0, 21.0);
        // Every whole dash one pixel nearer the receiver.
        let starts = |line: &[Dash]| -> Vec<f64> {
            line.iter().filter(|(a, b)| (b.0 - a.0 - DASH_LONG).abs() < 1e-6).map(|(a, _)| a.0).collect()
        };
        let (now_starts, later_starts) = (starts(&now), starts(&later));
        for start in &now_starts {
            assert!(later_starts.iter().any(|later| (later - start - 1.0).abs() < 1e-6), "{start} moved one pixel on");
        }
        assert!(close(&straight(0.0, 20.0), &straight(0.0, 20.0 + DASH_PERIOD)));
        assert!(close(&straight(1.5, 3.0), &straight(1.5, 3.0 + 7.0 * DASH_PERIOD)));
        // Device pixels: twice the scale, twice the spacing.
        let doubled = dashes(Bend { from: (0.0, 0.0), control: (300.0, 0.0), to: (600.0, 0.0) }, (20.0, 20.0), 0.0, 20.0, 2.0);
        assert_eq!(doubled.len(), now.len());
    }

    #[test]
    fn a_two_way_link_has_two_lanes_moving_apart() {
        assert_eq!(lanes(0, 0), []);
        assert_eq!(lanes(3, 0), [(Way::Forth, 0.0)]);
        assert_eq!(lanes(0, 1), [(Way::Back, 0.0)]);
        let both = lanes(2, 5);
        assert_eq!(both.len(), 2);
        // Each lane is drawn from its own sender: forth from (0, 0), back
        // from (300, 0), each to its own right.
        let (a, b, middle) = ((0.0, 0.0), (300.0, 0.0), (150.0, 0.0));
        let lane = |way: Way, offset: f64, travelled: f64| match way {
            Way::Forth => dashes(Bend { from: a, control: middle, to: b }, (10.0, 10.0), offset, travelled, 1.0),
            Way::Back => dashes(Bend { from: b, control: middle, to: a }, (10.0, 10.0), offset, travelled, 1.0),
        };
        let ys: Vec<f64> = both.iter().map(|&(way, offset)| lane(way, offset, 2.0)[0].0.1).collect();
        assert!(ys[0] > 0.0 && ys[1] < 0.0, "either side of the middle: {ys:?}");
        assert!((ys[0] + ys[1]).abs() < 1e-9 && (ys[0] - LANE_APART).abs() < 1e-9);
        // A little later, the forth lane's whole dashes are further along x
        // and the back lane's further back: compare one dash each.
        for &(way, offset) in &both {
            let now = lane(way, offset, 2.0);
            let later = lane(way, offset, 3.0);
            let step = later[1].0.0 - now[1].0.0;
            match way {
                Way::Forth => assert!((step - 1.0).abs() < 1e-6, "{step}"),
                Way::Back => assert!((step + 1.0).abs() < 1e-6, "{step}"),
            }
        }
    }

    #[test]
    fn dashes_follow_a_bow_and_stay_off_the_nodes() {
        let (from, control, to) = ((0.0, 0.0), (100.0, 60.0), (200.0, 0.0));
        let line = dashes(Bend { from, control, to }, (8.0, 12.0), 0.0, 5.0, 1.0);
        assert!(line.len() > 10);
        let on_curve = |(x, y): (f64, f64)| {
            // y of the curve at this x: x = 200t, y = 60t(1 - t)·2.
            let t = x / 200.0;
            (y - 120.0 * t * (1.0 - t)).abs() < 0.5
        };
        for &(p, q) in &line {
            assert!(on_curve(p) && on_curve(q), "{p:?} {q:?}");
            // Measured along the curve, so straight across a hair less.
            assert!(p.0.hypot(p.1) >= 8.0 - 0.2 && (q.0 - 200.0).hypot(q.1) >= 12.0 - 0.2);
        }
        // Nothing to draw between nodes that touch, or for a degenerate one.
        assert!(dashes(Bend { from, control: from, to: (15.0, 0.0) }, (8.0, 8.0), 0.0, 0.0, 1.0).is_empty());
        assert!(dashes(Bend { from, control: from, to: from }, (0.0, 0.0), 0.0, 0.0, 1.0).is_empty());
        assert!(dashes(Bend { from, control, to }, (0.0, 0.0), 0.0, f64::NAN, 1.0).is_empty(), "a broken clock draws nothing");
    }

    #[test]
    fn only_carrying_lines_move_and_none_with_the_switch_off() {
        let web = fake(20, 3, 0);
        let shown = shown_edges(&web, None);
        let off = moving(&web, &shown, None, false);
        assert!(off.iter().all(|&moves| !moves));
        let on = moving(&web, &shown, None, true);
        assert!(on.iter().any(|&moves| moves));
        // The fake room has lines of one lane and of two.
        let lanes_of = |at: usize| lanes(web.edges[at].forth, web.edges[at].back).len();
        let moving_at: Vec<usize> = shown.edges.iter().zip(&on).filter(|(_, moves)| **moves).map(|(&at, _)| at).collect();
        assert!(moving_at.iter().any(|&at| lanes_of(at) == 1) && moving_at.iter().any(|&at| lanes_of(at) == 2));
        for (&at, &moves) in shown.edges.iter().zip(&on) {
            let edge = &web.edges[at];
            let carrying = edge.state == State::Flowing && (edge.forth > 0 || edge.back > 0);
            assert_eq!(moves, carrying, "{edge:?}");
        }
        assert!(shown.edges.iter().zip(&on).any(|(&at, &moves)| web.edges[at].state != State::Flowing && !moves));
    }

    #[test]
    fn a_selection_keeps_only_its_paths_moving() {
        let web = fake(20, 3, 0);
        let shown = shown_edges(&web, None);
        let me = me_of(&web);
        let lit = focus(&web, me);
        let moves = moving(&web, &shown, Some(me), true);
        for (&at, &moves) in shown.edges.iter().zip(&moves) {
            if moves {
                assert!(lit.edges[at], "a dimmed line keeps still");
            }
        }
        assert!(moves.iter().any(|&moves| moves));
        let all = moving(&web, &shown, None, true);
        assert!(all.iter().filter(|&&m| m).count() > moves.iter().filter(|&&m| m).count());
    }

    #[test]
    fn in_a_big_room_only_the_focus_moves() {
        let web = fake(150, 3, 0);
        let shown = shown_edges(&web, None);
        let carrying = shown
            .edges
            .iter()
            .filter(|&&at| web.edges[at].state == State::Flowing && (web.edges[at].forth > 0 || web.edges[at].back > 0))
            .count();
        assert!(carrying > MOVING_MAX, "{carrying}");
        let me = me_of(&web);
        let ours = focus(&web, me);
        let moves = moving(&web, &shown, None, true);
        let count = moves.iter().filter(|&&m| m).count();
        assert!(count > 0 && count < carrying, "{count} of {carrying}");
        for (&at, &moves) in shown.edges.iter().zip(&moves) {
            if moves {
                assert!(ours.edges[at]);
            }
        }
        // Selecting someone else moves theirs instead.
        let other = web.nodes.iter().position(|node| !node.me && node.kind == Kind::Participant).unwrap();
        let theirs = focus(&web, other);
        for (&at, &moves) in shown.edges.iter().zip(&moving(&web, &shown, Some(other), true)) {
            if moves {
                assert!(theirs.edges[at]);
            }
        }
    }

    #[test]
    fn a_frame_of_the_largest_room_is_bounded() {
        // Every carrying line of the 150-person fake room as if all moved,
        // on a 400 px square: what a frame costs at the worst the cap allows
        // and then some.
        let web = fake(150, 3, 0);
        let places = layout(&web, None);
        let index_of = |id: &str| web.index_of(id).unwrap();
        let started = std::time::Instant::now();
        let mut total = 0;
        let mut lines = 0;
        for edge in web.edges.iter().filter(|edge| edge.state == State::Flowing) {
            let (a, b) = (&places[index_of(&edge.from)], &places[index_of(&edge.to)]);
            let (from, to) = ((a.x * 400.0, a.y * 400.0), (b.x * 400.0, b.y * 400.0));
            let middle = ((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0);
            for (way, offset) in lanes(edge.forth, edge.back) {
                let (start, end) = if way == Way::Forth { (from, to) } else { (to, from) };
                total += dashes(Bend { from: start, control: middle, to: end }, (6.0, 6.0), offset, 0.0, 1.0).len();
            }
            lines += 1;
        }
        let took = started.elapsed();
        assert!(lines > MOVING_MAX);
        // Per line, at most a two-way line's worth across the square.
        assert!(total < lines * 2 * (400.0 / DASH_PERIOD) as usize, "{total} dashes on {lines} lines");
        eprintln!("{total} dashes on {lines} lines in {took:?}");
    }

    #[test]
    fn an_empty_or_lonely_web_lays_out() {
        assert!(layout(&Web::default(), None).is_empty());
        let web = web_with(Vec::new(), vec![node("a", true)]);
        let places = layout(&web, None);
        assert!(places[0].named && places[0].y < CENTRE);
    }
}
