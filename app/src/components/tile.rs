//! One cell of the call grid: someone's video, and a line saying how it is.
//!
//! **The label is in normal flow under the video, not laid over it.** An
//! overlay would only need to paint, which absolutely-positioned content does
//! manage in this blitz revision -- but a row beneath costs nothing, cannot
//! cover the picture, and is one less thing that depends on the renderer.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use dioxus_native::prelude::*;

use super::device_menu::OpenMenu;
use super::tile_menu::{TileMenu, menu_name};
use super::{Invites, MicIcon, ParticipantVideo, PersonIcon, Rooms};
use crate::audio::Crosstalk;
use crate::camera::{FrameSlot, Mirror};
use crate::channel::use_channel;
use crate::net::PeerStats;

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
    /// Who muted us. Our own tile only; empty on a peer's.
    muted_by: Option<String>,
    /// Who invites us, to which room. Our own tile only; passed on to the
    /// page's [`Invites`].
    invites: BTreeMap<String, Option<String>>,
}

impl Default for Reading {
    fn default() -> Self {
        Self {
            fps: None,
            talking: false,
            camera_on: true,
            mic_on: true,
            room: None,
            muted_by: None,
            invites: BTreeMap::new(),
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct ParticipantTileProps {
    /// Names the stream in logs, and is the label's text.
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
}

#[component]
pub fn ParticipantTile(props: ParticipantTileProps) -> Element {
    let mut reading = use_signal(Reading::default);
    let Rooms(mut rooms) = use_context::<Rooms>();
    let Invites(mut invites) = use_context::<Invites>();
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
            }
            if rooms.peek().get(&key) != Some(&next.room) {
                rooms.write().insert(key.clone(), next.room.clone());
            }
            written.replace(Some(key));
            if mine && *invites.peek() != next.invites {
                invites.set(next.invites.clone());
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
        }
    });
    use_hook({
        let stats = props.stats.clone();
        move || {
            let Some(stats) = stats else { return };
            let spawned = std::thread::Builder::new()
                .name("tile".into())
                .spawn(move || loop {
                    let next = Reading {
                        fps: stats.video_fps(),
                        talking: stats.talking(),
                        camera_on: stats.camera_on(),
                        mic_on: stats.mic_on(),
                        room: stats.room(),
                        muted_by: stats.muted_by(),
                        invites: stats.invites(),
                    };
                    if tx.unbounded_send(next).is_err() {
                        return;
                    }
                    std::thread::sleep(POLL);
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
        .map(|by| format!("muted by {}", by.chars().take(8).collect::<String>()));
    let name = menu_name(&props.who);
    let open = !mine && current.read().as_deref() == Some(name.as_str());
    let our_room = rooms.read().get(&props.me).cloned().flatten();
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
                span { class: text, "{props.label}{fps}" }
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
                if !mine {
                    span { class: "text-xs text-neutral-400", "\u{25BE}" }
                }
            }
            // Beside the label, not inside it: a click on an item would
            // otherwise bubble to the label and reopen the menu.
            if !mine {
                TileMenu {
                    who: props.who.clone(),
                    their_room: now.room.clone(),
                    their_mic: now.mic_on,
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
