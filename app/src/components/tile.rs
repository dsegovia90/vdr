//! One cell of the call grid: someone's video, and a line saying how it is.
//!
//! **The label is in normal flow under the video, not laid over it.** An
//! overlay would only need to paint, which absolutely-positioned content does
//! manage in this blitz revision -- but a row beneath costs nothing, cannot
//! cover the picture, and is one less thing that depends on the renderer.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use dioxus_native::prelude::*;

use super::device_menu::OpenMenu;
use super::tile_menu::{TileMenu, menu_name, move_item};
use super::{Invites, MicIcon, Names, OwnShare, ParticipantVideo, PersonIcon, Rooms, SignalBars, Through};
use crate::audio::Crosstalk;
use crate::camera::{FrameSlot, Mirror};
use crate::channel::use_channel;
use crate::net::arrival::{self, MeetsAt, Sending};
use crate::net::{Metrics, PeerStats};
use crate::net::quality::{self, Quality};

/// How often a tile re-reads its numbers. Matches the stats row.
const POLL: Duration = Duration::from_millis(500);

/// What a tile's label shows, sampled so rendering never touches the atomics.
#[derive(Clone, PartialEq)]
struct Reading {
    fps: Option<f32>,
    talking: bool,
    camera_on: bool,
    mic_on: bool,
    /// Their subroom. Not shown on the tile; passed on to the page's [`Rooms`].
    room: Option<String>,
    /// What they call themselves, already cleaned. Shown in place of the
    /// label on a peer's tile, and passed on to the page's [`Names`].
    name: Option<String>,
    /// Who muted us. Our own tile only; empty on a peer's.
    muted_by: Option<String>,
    /// Who invites us, to which room. Our own tile only; passed on to the
    /// page's [`Invites`].
    invites: BTreeMap<String, Option<String>>,
    /// Whether we share a screen, and why a share failed. Our own tile only;
    /// passed on to the page's [`OwnShare`].
    sharing: bool,
    share_problem: Option<String>,
    /// Their connection's quality as they report it, ours on our own tile.
    /// `None` when they are not reporting or our connection report is off:
    /// no mark at all, rather than an "unknown" one.
    mark: Option<Quality>,
    // **The two below are only read while the route switch is on**, and
    // stay at their defaults while it is off, so nothing changes and nothing
    // is sent for them then.
    /// What the host border is decided from, besides everyone's routes.
    seat: Seat,
    /// The hosts their media passes through to us, from their route. A
    /// peer's tile only; passed on to the page's [`Through`].
    through: Vec<String>,
}

/// What a tile's host border is read from, as sampled: each field changes
/// only when hosting does, never with the figures, so sampling it wakes the
/// page no more often than the border could change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Seat {
    /// Whether they could host: their presence says they host the room, or,
    /// on our own tile, we serve it.
    can_host: bool,
    /// Where our own media goes. Sampled on every peer's tile, because it is
    /// the tile of whoever carries it that shows it.
    sending: Option<Sending>,
    /// Where the room meets, when that is an endpoint rather than a relay
    /// server.
    meets_at: Option<MeetsAt>,
    /// Whether we are forwarding other people's media, by the same measure
    /// as the log's "[host] forwarding started". Our own tile only.
    forwarding: bool,
    /// Whether at least one guest is connected to us. Our own tile only.
    guests: bool,
}

impl Default for Reading {
    fn default() -> Self {
        Self {
            fps: None,
            talking: false,
            camera_on: true,
            mic_on: true,
            room: None,
            name: None,
            muted_by: None,
            invites: BTreeMap::new(),
            sharing: false,
            share_problem: None,
            mark: None,
            seat: Seat::default(),
            through: Vec::new(),
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct ParticipantTileProps {
    /// Names the stream in logs, and is the label's text unless they have
    /// a name: their short id, or "You".
    pub label: String,
    /// Frames to render.
    pub slot: FrameSlot,
    /// Self-view mirrors; everyone else must not.
    pub mirror: Mirror,
    /// Their numbers, or `None` for our own preview, which has none worth a
    /// label -- the camera's own rate is on the status line below the grid.
    pub stats: Option<Arc<PeerStats>>,
    /// Size and placement, `layout::TILE`: the box *is* the card.
    pub class: &'static str,
    /// The dark layer over someone outside our subroom, from [`dim_class`];
    /// [`UNDIMMED`] for everyone in it.
    pub dim: &'static str,
    /// Who this is, by endpoint id: picks their avatar's colour, so everyone
    /// -- them included -- sees the same colour for the same person. Not the
    /// label, which reads "You" on your own tile.
    pub who: String,
    /// Ourselves: what a peer's tile menu asks through, and moves.
    pub local: Arc<PeerStats>,
    /// Our key in [`Rooms`]. The tile whose `who` is this is ours, and has no
    /// menu.
    pub me: String,
    /// Our own call's numbers, on our own tile: whether we are forwarding
    /// for anyone. `None` on a peer's.
    #[props(default)]
    pub metrics: Option<Arc<Metrics>>,
}

#[component]
pub fn ParticipantTile(props: ParticipantTileProps) -> Element {
    let mut reading = use_signal(Reading::default);
    let Rooms(mut rooms) = use_context::<Rooms>();
    let Names(mut names) = use_context::<Names>();
    let Invites(mut invites) = use_context::<Invites>();
    let OwnShare(mut own_share) = use_context::<OwnShare>();
    let Through(mut through) = use_context::<Through>();
    let OpenMenu(mut current) = use_context::<OpenMenu>();
    // Both are "" before the endpoint binds, and a peer's id is never ours.
    let mine = props.who == props.me;
    // **Who, as of this render.** Our own tile's `who` changes once, when the
    // endpoint binds, and the callback below outlives that render; it reads
    // this rather than the props it was created with.
    let who = use_hook(|| Rc::new(RefCell::new(String::new())));
    who.replace(props.who.clone());
    // The key this tile last wrote, so a changed `who` or an unmount removes it:
    // a stale entry would keep a subroom open after its last member left.
    let written = use_hook(|| Rc::new(RefCell::new(None::<String>)));

    // A thread per tile, the stats row's pattern: it samples and sends, and it
    // ends on its own when the tile unmounts and the channel's receiver goes.
    // At most a handful of tiles, each asleep nearly all the time.
    let tx = use_channel({
        let who = who.clone();
        let written = written.clone();
        move |next: Reading| {
            let key = who.borrow().clone();
            let stale = written.borrow().as_ref().filter(|old| **old != key).cloned();
            if let Some(stale) = stale {
                rooms.write().remove(&stale);
                names.write().remove(&stale);
                through.write().remove(&stale);
            }
            if rooms.peek().get(&key) != Some(&next.room) {
                rooms.write().insert(key.clone(), next.room.clone());
            }
            // Peers only: ours is never shown by name on this screen, and
            // "you" is what the lists say of us.
            let name = if mine { None } else { next.name.clone() };
            // Compared into a local first, so no read guard is held while
            // the same signal is written.
            let same = names.peek().get(&key) == name.as_ref();
            if !same {
                match name {
                    Some(name) => names.write().insert(key.clone(), name),
                    None => names.write().remove(&key),
                };
            }
            // Peers only, and only an entry that says something, so a room
            // reached all through the relay leaves the map empty.
            let hops = if mine { Vec::new() } else { next.through.clone() };
            let same = through.peek().get(&key).map_or(hops.is_empty(), |old| *old == hops);
            if !same {
                if hops.is_empty() {
                    through.write().remove(&key);
                } else {
                    through.write().insert(key.clone(), hops);
                }
            }
            written.replace(Some(key));
            if mine && *invites.peek() != next.invites {
                invites.set(next.invites.clone());
            }
            let share = (next.sharing, next.share_problem.clone());
            if mine && *own_share.peek() != share {
                own_share.set(share);
            }
            if *reading.peek() != next {
                reading.set(next);
            }
        }
    });
    use_drop(move || {
        if let Some(key) = written.take() {
            // The page may be going too, taking the map with it.
            if let Ok(mut map) = rooms.try_write() {
                map.remove(&key);
            }
            if let Ok(mut map) = names.try_write() {
                map.remove(&key);
            }
            if let Ok(mut map) = through.try_write() {
                map.remove(&key);
            }
        }
    });
    use_hook({
        let stats = props.stats.clone();
        let local = props.local.clone();
        let metrics = props.metrics.clone();
        move || {
            let Some(stats) = stats else { return };
            // Our own entry is the one our tile is given.
            let own = Arc::ptr_eq(&stats, &local);
            let spawned = std::thread::Builder::new()
                .name("tile".into())
                .spawn(move || {
                    let mut sent: Option<Reading> = None;
                    loop {
                        // Off means neither publish nor show.
                        let shown = local.connection_report();
                        let link = if shown { stats.link() } else { None };
                        let routes = local.show_routes();
                        let through = match (routes, own, stats.arrival()) {
                            (true, false, Some(way)) => arrival::through(&way),
                            _ => Vec::new(),
                        };
                        let hosting = match (routes, own) {
                            (true, true) => metrics.as_ref().and_then(|metrics| metrics.hosting()),
                            _ => None,
                        };
                        let seat = if routes {
                            Seat {
                                // `hosting` is what our own entry knows,
                                // serving the room; `hosts_room` what a
                                // peer's says.
                                can_host: stats.hosting() || stats.hosts_room(),
                                sending: if own { None } else { local.sending() },
                                meets_at: local.meets_at(),
                                forwarding: hosting.is_some_and(|hosting| hosting.forwarding),
                                guests: hosting.is_some_and(|hosting| hosting.guests > 0),
                            }
                        } else {
                            Seat::default()
                        };
                        let next = Reading {
                            // Only the state, which has hysteresis: the
                            // figures move every report, and any change
                            // sent here wakes the page.
                            mark: link.as_ref().map(|link| link.q),
                            fps: stats.video_fps(),
                            talking: stats.talking(),
                            camera_on: stats.camera_on(),
                            mic_on: stats.mic_on(),
                            room: stats.room(),
                            name: stats.name(),
                            muted_by: stats.muted_by(),
                            invites: stats.invites(),
                            sharing: stats.sharing(),
                            share_problem: stats.share_problem(),
                            seat,
                            through,
                        };
                        // Only a change is sent. Any message wakes the page, and blitz repaints
                        // the whole window whenever the page wakes, changed or not.
                        if sent.as_ref() != Some(&next) {
                            if tx.unbounded_send(next.clone()).is_err() {
                                return;
                            }
                            sent = Some(next);
                        }
                        std::thread::sleep(POLL);
                    }
                });
            if let Err(error) = spawned {
                tracing::warn!("could not spawn a tile's stats thread: {error}");
            }
        }
    });

    let now = reading();
    // **A camera that is off is a choice, not a fault.** No frames arrive
    // either way, so without the sender's word this read as a stall: amber
    // "0 fps" over a frozen last frame.
    let fps = match now.fps {
        _ if !now.camera_on => " \u{b7} camera off".to_string(),
        Some(fps) => format!(" \u{b7} {fps:.0} fps"),
        None => String::new(),
    };
    // A stalled stream is the thing this label exists to show, so zero is
    // coloured rather than merely printed.
    let stalled = now.camera_on && now.fps.is_some_and(|fps| fps < 1.0);
    let avatar = avatar_class(&props.who);
    let text = if stalled {
        "whitespace-nowrap text-xs text-amber-400"
    } else {
        "whitespace-nowrap text-xs text-neutral-200"
    };
    // On our own tile only: a mic that went off by itself reads as a fault
    // unless it says who did it.
    let muted_by = now
        .muted_by
        .as_ref()
        .map(|by| format!("muted by {}", Names::of(&names.read(), by)));
    // **Their name if they have one, on a peer's tile only**: ours says
    // "You" whatever we are called.
    let shown = shown_label(mine, now.name.as_deref(), &props.label);
    let host = host_mark(&props.who, mine, &now.seat, &through.read());
    // Said once per change, so the log tells a developer what the border
    // shows; the routes behind it are logged where they are read.
    let shown_mark = use_hook(|| Rc::new(Cell::new(HostMark::None)));
    let was = shown_mark.replace(host);
    if (was == HostMark::Carrying) != (host == HostMark::Carrying) {
        let who = &props.who;
        match (mine, host == HostMark::Carrying) {
            (true, true) if now.seat.meets_at == Some(MeetsAt::Us) => {
                tracing::info!("[tile] we carry the call now: the room meets at us and a guest is here")
            }
            (true, true) => tracing::info!("[tile] we carry the call now: forwarding for others"),
            (true, false) => tracing::info!("[tile] we no longer carry the call"),
            (false, true) => tracing::info!("[tile] {who} carries the call for us now"),
            (false, false) => tracing::info!("[tile] {who} no longer carries the call for us"),
        }
    }
    let name = menu_name(&props.who);
    let open = !mine && current.read().as_deref() == Some(name.as_str());
    let our_room = rooms.read().get(&props.me).cloned().flatten();
    // **Read as the menu renders, not sampled**: the figures move with every
    // report, and sampling them would wake the page every couple of seconds
    // for a line that is mostly hidden. Opening the menu renders it fresh.
    let connection = match (&props.stats, open && props.local.connection_report()) {
        (Some(stats), true) => quality::connection_line(stats.link().as_ref(), props.local.seen().get(&props.who)),
        _ => None,
    };
    // Read the same way, as the menu opens: whether they host, and whether
    // the call is on them already.
    let moving = if open {
        move_item(
            props.local.move_calls(),
            props.stats.as_ref().is_some_and(|stats| stats.hosts_room()),
            props.local.call_host().as_deref(),
            &props.who,
        )
    } else {
        None
    };
    // **No z-index while closed**, so the dim layer still covers it; lifted
    // above its own click-away layer while open, so a second click closes it.
    let label = if mine {
        "absolute bottom-2 left-2 flex flex-row items-center gap-2 rounded-md bg-black/50 px-2 py-1"
    } else if open {
        "absolute bottom-2 left-2 z-[6] cursor-pointer flex flex-row items-center gap-2 rounded-md bg-black/80 px-2 py-1"
    } else {
        "absolute bottom-2 left-2 cursor-pointer flex flex-row items-center gap-2 rounded-md bg-black/50 px-2 py-1 hover:bg-black/70"
    };

    rsx! {
        // **The box is the card.** It is sized by the layout to the video's
        // shape, so the label can sit on the picture itself and moves with it,
        // rather than under a cell the card may not fill.
        div { class: props.class,
            ParticipantVideo {
                label: props.label.clone(),
                slot: props.slot.clone(),
                mirror: props.mirror,
                class: "absolute inset-0",
            }
            // Over the video, which keeps its place and shape underneath: the
            // last frame they sent is not who they are now.
            if !now.camera_on {
                div { class: "absolute inset-0 flex items-center justify-center rounded-[12px] bg-neutral-800",
                    div { class: avatar, PersonIcon {} }
                }
            }
            // The host border: over the picture rather than around the box,
            // so it never changes the tile's size, which the video's own
            // size would then chase. Mounted always and hidden by class, as
            // the dim layer is. Before the label, which it must not cover.
            div { class: host.class() }
            // Over the video's bottom-left corner. On a peer's tile it opens
            // their menu.
            div {
                class: label,
                onclick: move |_| {
                    if mine {
                        return;
                    }
                    let next = if open { None } else { Some(name.clone()) };
                    current.set(next);
                },
                // Always rendered, so the label does not shift sideways when
                // someone starts talking.
                div {
                    class: if now.talking { "h-2 w-2 shrink-0 rounded-full bg-emerald-400" } else { "h-2 w-2 shrink-0 rounded-full bg-neutral-500" },
                }
                span { class: text, "{shown}{fps}" }
                if let Some(muted_by) = muted_by {
                    span { class: "whitespace-nowrap text-xs text-red-300", "\u{b7} {muted_by}" }
                }
                if !now.mic_on {
                    // Red and white, plain colours: an `oklch()` palette shade
                    // would leave the icon invisible (see `icons`).
                    div { class: "flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-red-600 text-white",
                        MicIcon { off: true }
                    }
                }
                if let Some(quality) = now.mark {
                    SignalBars { quality }
                }
                if !mine {
                    span { class: "text-xs text-neutral-400", "\u{25BE}" }
                }
            }
            // Beside the label, not inside it: a click on an item would
            // otherwise bubble to the label and reopen the menu.
            if !mine {
                TileMenu {
                    who: props.who.clone(),
                    name: now.name.clone(),
                    their_room: now.room.clone(),
                    their_mic: now.mic_on,
                    connection,
                    moving,
                    our_room,
                    local: props.local.clone(),
                    me: props.me.clone(),
                }
            }
            // **Last, so it covers everything above, the talking dot
            // included**: these are positioned siblings without a z-index, so
            // they paint in order. Mounted always and hidden by class, so a
            // change of room never adds or removes a node. `pointer-events-none`
            // on every dim class lets clicks through it, so the label it covers
            // still opens the menu.
            div { class: props.dim }
        }
    }
}

/// A tile label's text: a peer's name when they have one, else `label` (their
/// short id, or "You" on ours, which keeps it whatever we are called).
fn shown_label(mine: bool, name: Option<&str>, label: &str) -> String {
    match name {
        Some(name) if !mine => name.to_string(),
        _ => label.to_string(),
    }
}

/// What a tile's border says about hosting, from where we sit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostMark {
    /// They cannot host, or the route switch is off.
    None,
    /// They could host, and carry nothing of this call for us now.
    Can,
    /// They carry the call for us now.
    Carrying,
}

impl HostMark {
    /// One hue, sky blue, which nothing else on a tile uses: emerald is
    /// talking and a good link, amber a stall or a weak link, red a muted
    /// microphone or a poor link. The `host` theme colour (`colour::HOST`),
    /// plain hex rather than a palette shade, as the icons need; `/40` is the
    /// same mix of it with transparent that the dim layers' `bg-black/N` use.
    /// Quiet is thinner and see-through; carrying is the same
    /// colour at full strength and thicker, so the two read as one thing
    /// turned up. A border, not a ring: rings are box shadows, and borders
    /// are what this renderer is known to draw on the menus.
    fn class(self) -> &'static str {
        match self {
            HostMark::None => "hidden pointer-events-none absolute inset-0 rounded-[12px]",
            HostMark::Can => "pointer-events-none absolute inset-0 rounded-[12px] border-2 border-host/40",
            HostMark::Carrying => "pointer-events-none absolute inset-0 rounded-[12px] border-[3px] border-host",
        }
    }
}

/// Whether `who` could host, and whether they carry the call for us now.
///
/// **Carrying is read from our seat**: a peer carries when our own media goes
/// through them (`seat.sending`), or when anyone's media reaches us through
/// them (`through`, each peer's hosts, from their route). We carry when we
/// are forwarding other people's media, which is guests actually pulling it
/// or our bridging it to the relay, not merely guests connected. A host
/// carrying for someone else, out of our sight, stays quiet here.
///
/// **A room with no relay server meets at its creator's session**, so what
/// reads as "the relay" from a guest's seat is that person: our media going
/// "to the relay" goes through them, and they could host while they are in
/// the room. On the creator's own seat a single guest is enough to carry:
/// there is nothing of anyone else's to forward, yet everything that guest
/// receives comes from us. An iroh relay server that is no participant has
/// no tile, so nothing here marks it.
///
/// Carrying wins even without a word of hosting from them: a route through
/// someone is the stronger evidence, and their presence can lag it.
fn host_mark(who: &str, mine: bool, seat: &Seat, through: &BTreeMap<String, Vec<String>>) -> HostMark {
    let meets_here = match &seat.meets_at {
        Some(MeetsAt::Us) => mine,
        Some(MeetsAt::Endpoint(id)) => !mine && id == who,
        None => false,
    };
    let carrying = if mine {
        if meets_here { seat.guests } else { seat.forwarding }
    } else {
        let ours = match &seat.sending {
            Some(Sending::Host(id)) => Some(id),
            Some(Sending::Relay) => match &seat.meets_at {
                Some(MeetsAt::Endpoint(id)) => Some(id),
                _ => None,
            },
            Some(Sending::Guests) | None => None,
        };
        ours.is_some_and(|id| id == who) || through.values().any(|hosts| hosts.iter().any(|host| host == who))
    };
    match (carrying, seat.can_host || meets_here) {
        (true, _) => HostMark::Carrying,
        (false, true) => HostMark::Can,
        (false, false) => HostMark::None,
    }
}

/// The dim layer, hidden: our own subroom, or crosstalk at full volume, where
/// a subroom is only a label.
pub(super) const UNDIMMED: &str = "hidden pointer-events-none absolute inset-0 rounded-[12px]";

/// The dim layer over a tile outside our subroom, darker the quieter they are
/// to us, so what you see agrees with what you hear. Five steps rather than a
/// continuous alpha, because every class must be a literal.
///
/// The thresholds and alphas are first guesses, to be tuned by eye.
pub(super) fn dim_class(level: Crosstalk) -> &'static str {
    match level.volume {
        _ if level.muted => "pointer-events-none absolute inset-0 rounded-[12px] bg-black/80",
        v if v >= 0.95 => UNDIMMED,
        v if v >= 0.6 => "pointer-events-none absolute inset-0 rounded-[12px] bg-black/30",
        v if v >= 0.3 => "pointer-events-none absolute inset-0 rounded-[12px] bg-black/50",
        v if v > 0.0 => "pointer-events-none absolute inset-0 rounded-[12px] bg-black/65",
        _ => "pointer-events-none absolute inset-0 rounded-[12px] bg-black/80",
    }
}

/// The avatar circle's classes for `who`: the same colour every time for the
/// same person, so a room of cards can still be told apart at a glance.
///
/// Whole literals, as everywhere: Tailwind finds classes by scanning the
/// source. The circle's colours are plain sRGB through `bg-[#…]`, not palette
/// shades, so the white icon inside stays visible (`icons`).
fn avatar_class(who: &str) -> &'static str {
    const AVATARS: [&str; 6] = [
        "flex h-16 w-16 items-center justify-center rounded-full bg-[#2563eb] text-white",
        "flex h-16 w-16 items-center justify-center rounded-full bg-[#7c3aed] text-white",
        "flex h-16 w-16 items-center justify-center rounded-full bg-[#db2777] text-white",
        "flex h-16 w-16 items-center justify-center rounded-full bg-[#ea580c] text-white",
        "flex h-16 w-16 items-center justify-center rounded-full bg-[#059669] text-white",
        "flex h-16 w-16 items-center justify-center rounded-full bg-[#0891b2] text-white",
    ];
    // FNV-1a: tiny, and stable across runs and machines, which a hasher seeded
    // per process is not.
    let hash = who.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    AVATARS[(hash % AVATARS.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How dark a dim class is, as its alpha in percent; hidden is zero.
    fn darkness(class: &str) -> u32 {
        if class.starts_with("hidden") {
            return 0;
        }
        class.rsplit("bg-black/").next().unwrap().parse().unwrap()
    }

    fn level(volume: f32, muted: bool) -> Crosstalk {
        Crosstalk { volume, muted }
    }

    #[test]
    fn quieter_is_never_lighter() {
        let mut last = 0;
        for step in (0..=100).rev() {
            let now = darkness(dim_class(level(step as f32 / 100.0, false)));
            assert!(now >= last, "{step}% is lighter than the step above it");
            last = now;
        }
    }

    #[test]
    fn muted_is_the_darkest_whatever_the_volume() {
        for step in 0..=100 {
            assert_eq!(dim_class(level(step as f32 / 100.0, true)), dim_class(level(0.0, false)));
        }
        assert_eq!(darkness(dim_class(level(1.0, true))), 80);
    }

    #[test]
    fn full_volume_is_not_dimmed() {
        assert_eq!(dim_class(level(1.0, false)), UNDIMMED);
    }

    #[test]
    fn the_default_volume_is_dimmed() {
        assert_eq!(darkness(dim_class(Crosstalk::default())), 65);
    }

    #[test]
    fn a_peer_is_shown_by_name_and_we_are_you() {
        assert_eq!(shown_label(false, Some("Ada"), "3929461d"), "Ada");
        assert_eq!(shown_label(false, None, "3929461d"), "3929461d");
        assert_eq!(shown_label(true, Some("Ada"), "You"), "You");
        assert_eq!(shown_label(true, None, "You"), "You");
    }

    fn routes(of: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        of.iter()
            .map(|(peer, hosts)| (peer.to_string(), hosts.iter().map(|host| host.to_string()).collect()))
            .collect()
    }

    fn seat(can_host: bool, sending: Option<Sending>) -> Seat {
        Seat { can_host, sending, ..Seat::default() }
    }

    fn hosts(id: &str) -> Option<Sending> {
        Some(Sending::Host(id.to_string()))
    }

    /// Our own seat while hosting, in a room on a relay server.
    fn ours(forwarding: bool, guests: bool) -> Seat {
        Seat { can_host: true, forwarding, guests, ..Seat::default() }
    }

    #[test]
    fn nobody_who_cannot_host_and_carries_nothing_is_marked() {
        assert_eq!(host_mark("h", false, &seat(false, None), &routes(&[])), HostMark::None);
        assert_eq!(host_mark("me", true, &seat(false, None), &routes(&[])), HostMark::None);
    }

    #[test]
    fn a_host_carrying_nothing_for_us_is_quiet() {
        let elsewhere = routes(&[("m", &["k"])]);
        assert_eq!(host_mark("h", false, &seat(true, hosts("k")), &elsewhere), HostMark::Can);
        assert_eq!(host_mark("h", false, &seat(true, Some(Sending::Relay)), &routes(&[])), HostMark::Can);
    }

    #[test]
    fn a_host_our_media_goes_through_carries() {
        assert_eq!(host_mark("h", false, &seat(true, hosts("h")), &routes(&[])), HostMark::Carrying);
    }

    #[test]
    fn a_host_anyones_media_reaches_us_through_carries_nearest_or_not() {
        assert_eq!(host_mark("h", false, &seat(true, None), &routes(&[("m", &["h"])])), HostMark::Carrying);
        assert_eq!(host_mark("k", false, &seat(true, None), &routes(&[("m", &["h", "k"])])), HostMark::Carrying);
    }

    #[test]
    fn a_route_through_someone_marks_them_before_their_presence_says_they_host() {
        assert_eq!(host_mark("h", false, &seat(false, None), &routes(&[("m", &["h"])])), HostMark::Carrying);
    }

    #[test]
    fn we_carry_only_while_forwarding_and_others_routes_do_not_mark_us() {
        assert_eq!(host_mark("me", true, &ours(false, false), &routes(&[])), HostMark::Can);
        assert_eq!(host_mark("me", true, &ours(true, true), &routes(&[])), HostMark::Carrying);
        // On a relay server, guests connected but pulling nothing is not carrying.
        assert_eq!(host_mark("me", true, &ours(false, true), &routes(&[])), HostMark::Can);
        // A peer's own route naming us is not how our tile learns it.
        let named = Seat { sending: hosts("me"), ..ours(false, false) };
        assert_eq!(host_mark("me", true, &named, &routes(&[("m", &["me"])])), HostMark::Can);
    }

    /// Our own seat when we created a room with no relay server.
    fn creator(guests: u32) -> Seat {
        // Forwarding needs two guests: with one, nothing is anyone else's.
        Seat { meets_at: Some(MeetsAt::Us), guests: guests > 0, forwarding: guests > 1, ..Seat::default() }
    }

    #[test]
    fn the_creator_of_a_room_without_a_relay_carries_once_a_guest_is_here() {
        assert_eq!(host_mark("me", true, &creator(0), &routes(&[])), HostMark::Can, "said even without a word of hosting");
        assert_eq!(host_mark("me", true, &creator(1), &routes(&[])), HostMark::Carrying);
        assert_eq!(host_mark("me", true, &creator(2), &routes(&[])), HostMark::Carrying);
    }

    /// A guest's seat, joined at `iroh://c/room`, our media going `sending`.
    fn guest(sending: Option<Sending>) -> Seat {
        Seat { meets_at: Some(MeetsAt::Endpoint("c".to_string())), sending, ..Seat::default() }
    }

    #[test]
    fn a_guest_sees_the_creator_carry_what_goes_to_the_meeting_place() {
        assert_eq!(host_mark("c", false, &guest(Some(Sending::Relay)), &routes(&[])), HostMark::Carrying);
        // Another guest arriving through them, from `arrival::read`.
        assert_eq!(host_mark("c", false, &guest(None), &routes(&[("m", &["c"])])), HostMark::Carrying);
        // Our media moved to another host: they are still the meeting place.
        assert_eq!(host_mark("c", false, &guest(hosts("h")), &routes(&[])), HostMark::Can);
        assert_eq!(host_mark("h", false, &guest(hosts("h")), &routes(&[])), HostMark::Carrying);
        // Nobody else is the meeting place, and our own tile is not.
        assert_eq!(host_mark("m", false, &guest(Some(Sending::Relay)), &routes(&[])), HostMark::None);
        assert_eq!(host_mark("me", true, &guest(Some(Sending::Relay)), &routes(&[])), HostMark::None);
    }

    #[test]
    fn an_iroh_relay_that_is_nobody_in_the_room_marks_nobody() {
        // Joined at `iroh://r/room`, a relay server: no tile is ever `r`.
        let at = |sending| Seat { meets_at: Some(MeetsAt::Endpoint("r".to_string())), sending, ..Seat::default() };
        for who in ["m", "h"] {
            assert_eq!(host_mark(who, false, &at(Some(Sending::Relay)), &routes(&[])), HostMark::None);
        }
        assert_eq!(host_mark("me", true, &at(Some(Sending::Relay)), &routes(&[])), HostMark::None);
        // A host there keeps the relay-room rules.
        let on = Seat { can_host: true, ..at(hosts("h")) };
        assert_eq!(host_mark("h", false, &on, &routes(&[])), HostMark::Carrying);
    }

    #[test]
    fn the_two_strengths_are_one_hue_and_only_none_is_hidden() {
        assert!(HostMark::None.class().starts_with("hidden"));
        for mark in [HostMark::Can, HostMark::Carrying] {
            assert!(!mark.class().contains("hidden"));
            assert!(mark.class().contains(" border-host"));
        }
    }

    #[test]
    fn a_person_keeps_their_colour() {
        assert_eq!(avatar_class("3929461d"), avatar_class("3929461d"));
    }

    #[test]
    fn colours_spread_across_people() {
        let ids = ["3929461d", "573584f2", "ee14dce4", "faf8f9e4", "b3b02b79", "e2a8c443"];
        let distinct: std::collections::HashSet<_> = ids.iter().map(|id| avatar_class(id)).collect();
        assert!(distinct.len() >= 3, "six ids landed on {} colours", distinct.len());
    }
}
