//! One cell of the call grid: someone's video, and a line saying how it is.
//!
//! **The label is in normal flow under the video, not laid over it.** An
//! overlay would only need to paint, which absolutely-positioned content does
//! manage in this blitz revision -- but a row beneath costs nothing, cannot
//! cover the picture, and is one less thing that depends on the renderer.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use dioxus_native::prelude::*;

use super::{MicIcon, ParticipantVideo, PersonIcon, Rooms};
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
}

impl Default for Reading {
    fn default() -> Self {
        Self {
            fps: None,
            talking: false,
            camera_on: true,
            mic_on: true,
            room: None,
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
    /// Size and placement, from `layout::tile_class`: the box *is* the card.
    pub class: &'static str,
    /// Who this is, by endpoint id: picks their avatar's colour, so everyone
    /// -- them included -- sees the same colour for the same person. Not the
    /// label, which reads "You" on your own tile.
    pub who: String,
}

#[component]
pub fn ParticipantTile(props: ParticipantTileProps) -> Element {
    let mut reading = use_signal(Reading::default);
    let Rooms(mut rooms) = use_context::<Rooms>();
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
            // Over the video's bottom-left corner. It needs no clicks, so being
            // laid over the video costs nothing even where overlays misbehave.
            div { class: "absolute bottom-2 left-2 flex flex-row items-center gap-2 rounded-md bg-black/50 px-2 py-1",
                // Always rendered, so the label does not shift sideways when
                // someone starts talking.
                div {
                    class: if now.talking { "h-2 w-2 shrink-0 rounded-full bg-emerald-400" } else { "h-2 w-2 shrink-0 rounded-full bg-neutral-500" },
                }
                span { class: text, "{props.label}{fps}" }
                if !now.mic_on {
                    // Red and white, plain colours: an `oklch()` palette shade
                    // would leave the icon invisible (see `icons`).
                    div { class: "flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-red-600 text-white",
                        MicIcon { off: true }
                    }
                }
            }
        }
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
    use super::avatar_class;

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
