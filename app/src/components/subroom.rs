//! The subroom menu beside Leave: who is in which subroom, one click to join
//! one or go back to the main room, and how loud everyone outside ours is.
//!
//! **A subroom is a name and nothing more.** It is open while someone's
//! presence names it; nobody creates or closes it, so it stops existing when
//! the last person leaves and its name is free again. Two people opening one at
//! the same instant pick the same free name and end up together, which is
//! harmless and visible.
//!
//! The volume itself is applied in `net::audio`, on every frame, from the same
//! state this menu writes; nothing here tells the audio anything.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dioxus_native::prelude::*;

use super::SpeakerIcon;
use super::device_menu::OpenMenu;
use crate::audio::Controls;
use crate::net::PeerStats;
use crate::settings::Settings;

/// This menu's name in the page's [`OpenMenu`].
const NAME: &str = "subroom";

/// The slider track's width in pixels: `w-48`. The two must change together.
const TRACK: f64 = 192.0;

/// Everyone's subroom, keyed by endpoint id: `None` is the main room. One map
/// for the page, written by each tile as it samples its person, and read here.
///
/// Up to half a second behind a peer's change, which only the menu sees: the
/// audio reads each peer's own state on every frame instead.
#[derive(Clone, Copy)]
pub struct Rooms(pub Signal<BTreeMap<String, Option<String>>>);

/// Makes the shared [`Rooms`] available to the tiles and the menu below.
pub fn use_rooms_provider() {
    use_context_provider(|| Rooms(Signal::new(BTreeMap::new())));
}

#[derive(Clone, PartialEq, Props)]
pub struct SubroomMenuProps {
    /// Holds the out-of-room volume and mute.
    pub audio: Arc<Controls>,
    /// Ourselves: choosing a subroom records it here for the room to hear of.
    pub local: Arc<PeerStats>,
    /// Our key in [`Rooms`], the same `who` our own tile has.
    pub me: String,
}

#[component]
pub fn SubroomMenu(props: SubroomMenuProps) -> Element {
    let OpenMenu(mut current) = use_context::<OpenMenu>();
    let Rooms(rooms) = use_context::<Rooms>();
    let open = move || current() == Some(NAME);
    let mut crosstalk = use_signal({
        let audio = props.audio.clone();
        move || audio.crosstalk()
    });
    let mut dragging = use_signal(|| false);

    let map = rooms.read().clone();
    let mine = map.get(&props.me).cloned().flatten();
    let subrooms = open_subrooms(&map);
    let label = mine.clone().unwrap_or_else(|| "Main room".to_string());

    // Ours is written straight into the map as well as told to the room, so
    // the button changes on the click rather than at the tile's next sample.
    let join = {
        let local = props.local.clone();
        let me = props.me.clone();
        move |room: Option<String>| {
            let mut rooms = rooms;
            local.set_room(room.clone());
            rooms.write().insert(me.clone(), room);
            current.set(None);
        }
    };
    let new_name = next_free_name(&subrooms.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>());

    let set_volume = {
        let audio = props.audio.clone();
        move |x: f64| {
            audio.set_crosstalk_volume((x / TRACK).clamp(0.0, 1.0) as f32);
            crosstalk.set(audio.crosstalk());
        }
    };
    // Saved once, when the drag ends, not on every move.
    let release = move || {
        if *dragging.peek() {
            dragging.set(false);
            let volume = crosstalk.peek().volume;
            Settings::update(move |settings| settings.crosstalk_volume = Some(volume));
        }
    };

    let now = crosstalk();
    let pct = (now.volume * 100.0).round() as u32;
    let shown = if now.muted { "Muted".to_string() } else { format!("{pct}%") };

    rsx! {
        div { class: "relative shrink-0",
            // Lifted above the click-catching layer for the reason the device
            // menus' ▾ are.
            button {
                class: if open() {
                    "relative z-[6] flex h-10 max-w-48 flex-row items-center gap-1.5 rounded-full bg-neutral-700 px-4 text-sm text-white"
                } else {
                    "relative z-[6] flex h-10 max-w-48 flex-row items-center gap-1.5 rounded-full bg-neutral-800 px-4 text-sm text-white hover:bg-neutral-700"
                },
                onclick: move |_| {
                    let next = if open() { None } else { Some(NAME) };
                    current.set(next);
                },
                span { class: "truncate", "{label}" }
                span { class: "shrink-0 text-xs text-neutral-400", "\u{25BE}" }
            }
            // **Mounted always, hidden while closed -- never removed**, as in
            // `device_menu`: blitz panics hit-testing a removed node that had a
            // positive z-index.
            Fragment {
                div {
                    class: if open() { "fixed inset-0 z-[5]" } else { "hidden fixed inset-0 z-[5]" },
                    onclick: move |_| current.set(None),
                }
                // Anchored right: the button is at the bar's right edge, and a
                // panel growing rightwards would leave the window.
                div {
                    class: if open() {
                        "absolute bottom-full right-0 z-10 mb-2 w-72 rounded-lg border border-neutral-700 bg-neutral-900 p-1"
                    } else {
                        "hidden absolute bottom-full right-0 z-10 mb-2 w-72 rounded-lg border border-neutral-700 bg-neutral-900 p-1"
                    },
                    div { class: "px-2.5 pb-1 pt-1.5 text-xs text-neutral-500", "Subrooms" }
                    if subrooms.is_empty() {
                        div { class: "px-2.5 py-2 text-sm text-neutral-500", "None open" }
                    }
                    for (name, members) in subrooms.iter() {
                        div {
                            key: "{name}",
                            class: if mine.as_ref() == Some(name) {
                                "flex flex-row items-center gap-2 rounded px-2.5 py-2 text-sm bg-blue-600 text-white"
                            } else {
                                "flex flex-row items-center gap-2 rounded px-2.5 py-2 text-sm text-neutral-200 hover:bg-neutral-800"
                            },
                            onclick: {
                                let mut join = join.clone();
                                let name = name.clone();
                                move |_| join(Some(name.clone()))
                            },
                            // Both come off the wire, so both may be long.
                            span { class: "max-w-28 shrink-0 truncate", "{name}" }
                            span { class: "min-w-0 truncate text-xs opacity-70", {who(members, &props.me)} }
                        }
                    }
                    div {
                        class: if mine.is_none() {
                            "rounded px-2.5 py-2 text-sm bg-blue-600 text-white"
                        } else {
                            "rounded px-2.5 py-2 text-sm text-neutral-200 hover:bg-neutral-800"
                        },
                        onclick: {
                            let mut join = join.clone();
                            move |_| join(None)
                        },
                        "Main room"
                    }
                    div {
                        class: "rounded px-2.5 py-2 text-sm text-neutral-400 hover:bg-neutral-800",
                        onclick: {
                            let mut join = join.clone();
                            move |_| join(Some(new_name.clone()))
                        },
                        "New subroom"
                    }
                    div { class: "my-1 h-px bg-neutral-800" }
                    div { class: "px-2.5 pb-1 pt-1.5 text-xs text-neutral-500", "Everyone outside my subroom" }
                    div { class: "flex flex-row items-center gap-3 px-2.5 py-2",
                        // **Hand-built: blitz has no range input.** It lays out
                        // text inputs and checkboxes only, so `type="range"`
                        // would be an empty box that ignores the pointer.
                        //
                        // The track, its fill and this hit area all start at
                        // the same left edge, so `element_coordinates` gives
                        // the same x whichever of them the pointer is over.
                        div {
                            class: "flex h-5 w-48 shrink-0 flex-row items-center",
                            onmousedown: {
                                let mut set_volume = set_volume.clone();
                                move |evt: MouseEvent| {
                                    dragging.set(true);
                                    set_volume(evt.data().element_coordinates().x);
                                }
                            },
                            onmousemove: {
                                let mut set_volume = set_volume.clone();
                                move |evt: MouseEvent| {
                                    if *dragging.peek() {
                                        set_volume(evt.data().element_coordinates().x);
                                    }
                                }
                            },
                            onmouseup: {
                                let mut release = release;
                                move |_| release()
                            },
                            // Released outside the slider must not leave it
                            // following the pointer.
                            onmouseleave: {
                                let mut release = release;
                                move |_| release()
                            },
                            div { class: "relative h-2 w-48 rounded-full bg-neutral-700",
                                div {
                                    class: if now.muted {
                                        "absolute left-0 top-0 h-2 rounded-full bg-neutral-500"
                                    } else {
                                        "absolute left-0 top-0 h-2 rounded-full bg-blue-500"
                                    },
                                    style: "width: {pct}%",
                                }
                            }
                        }
                        span { class: "w-10 shrink-0 text-right text-xs text-neutral-300", "{shown}" }
                        // Red and white, plain colours: see `icons`.
                        button {
                            class: if now.muted {
                                "flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-red-600 text-white hover:bg-red-500"
                            } else {
                                "flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-neutral-800 text-white hover:bg-neutral-700"
                            },
                            onclick: {
                                let audio = props.audio.clone();
                                move |_| {
                                    let muted = !crosstalk.peek().muted;
                                    audio.set_crosstalk_muted(muted);
                                    crosstalk.set(audio.crosstalk());
                                    Settings::update(move |settings| settings.crosstalk_muted = Some(muted));
                                }
                            },
                            SpeakerIcon { off: now.muted }
                        }
                    }
                }
            }
        }
    }
}

/// Who is in a subroom, for its row: short ids, and "you" for us.
fn who(members: &[String], me: &str) -> String {
    members
        .iter()
        .map(|id| if id == me { "you".to_string() } else { id.chars().take(8).collect() })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The number in a generated name, "Room 3" -> 3.
fn number(name: &str) -> Option<u32> {
    name.strip_prefix("Room ")?.parse().ok()
}

/// The name a new subroom takes: the smallest "Room n" no open subroom uses.
/// Names that are not of that form take nothing.
pub(crate) fn next_free_name(open: &[&str]) -> String {
    let taken: BTreeSet<u32> = open.iter().filter_map(|name| number(name)).collect();
    let free = (1..).find(|n| !taken.contains(n)).unwrap_or(1);
    format!("Room {free}")
}

/// The open subrooms and who is in each, from everyone's room.
///
/// A subroom exists only by someone naming it, so an empty one cannot appear,
/// and the main room is not one. Sorted by number, so "Room 10" follows
/// "Room 9"; any other name comes after, alphabetically.
pub(crate) fn open_subrooms(rooms: &BTreeMap<String, Option<String>>) -> Vec<(String, Vec<String>)> {
    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, room) in rooms {
        if let Some(room) = room {
            by_name.entry(room.clone()).or_default().push(id.clone());
        }
    }
    let mut open: Vec<_> = by_name.into_iter().collect();
    open.sort_by(|(a, _), (b, _)| {
        (number(a).unwrap_or(u32::MAX), a).cmp(&(number(b).unwrap_or(u32::MAX), b))
    });
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_subroom_is_room_1() {
        assert_eq!(next_free_name(&[]), "Room 1");
    }

    #[test]
    fn a_gap_is_filled_first() {
        assert_eq!(next_free_name(&["Room 1", "Room 3"]), "Room 2");
        assert_eq!(next_free_name(&["Room 1", "Room 2"]), "Room 3");
    }

    #[test]
    fn other_names_take_no_number() {
        assert_eq!(next_free_name(&["Kitchen", "Room x", "Room 2"]), "Room 1");
    }

    fn map(entries: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        entries
            .iter()
            .map(|(id, room)| (id.to_string(), room.map(str::to_string)))
            .collect()
    }

    #[test]
    fn the_main_room_is_not_a_subroom() {
        assert!(open_subrooms(&map(&[("a", None), ("b", None)])).is_empty());
    }

    #[test]
    fn subrooms_list_their_members_in_number_order() {
        let open = open_subrooms(&map(&[
            ("a", Some("Room 10")),
            ("b", Some("Room 2")),
            ("c", None),
            ("d", Some("Room 2")),
            ("e", Some("Kitchen")),
        ]));
        let names: Vec<_> = open.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["Room 2", "Room 10", "Kitchen"]);
        assert_eq!(open[0].1, ["b", "d"]);
    }

    #[test]
    fn a_subroom_everyone_left_is_gone() {
        let open = open_subrooms(&map(&[("a", Some("Room 1")), ("b", Some("Room 2"))]));
        assert_eq!(open.len(), 2);
        let open = open_subrooms(&map(&[("a", Some("Room 1")), ("b", None)]));
        assert_eq!(open.len(), 1, "Room 2 closed when b left it");
        assert_eq!(next_free_name(&open.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>()), "Room 2");
    }
}
