//! The subroom menu beside Leave: who is in which subroom, one click to join
//! one or go back to the main room, and how loud everyone outside ours is.
//!
//! **A subroom is a name and nothing more.** It is open while someone's
//! presence names it; nobody creates or closes it, so it stops existing when
//! the last person leaves and its name is free again.
//!
//! **New subrooms are rooms of a house, picked at random** from those not in
//! use, so two people pressing New subroom before either has heard of the
//! other's usually get different rooms. When they do pick the same one they
//! end up together, which is harmless and visible. Only once every room of
//! the house is taken does a new one fall back to "Room n".
//!
//! The volume itself is applied in `net::audio`, on every frame, from the same
//! state this menu writes; nothing here tells the audio anything.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dioxus_native::prelude::*;

use super::SpeakerIcon;
use super::device_menu::OpenMenu;
use crate::audio::{Controls, Crosstalk};
use crate::net::PeerStats;
use crate::settings::Settings;

/// This menu's name in the page's [`OpenMenu`].
const NAME: &str = "subroom";

/// The slider track's width in pixels: `w-40`. The two must change together.
///
/// Sized to fit the panel: `w-72` less its padding leaves 260 px for the
/// slider row, which holds this, the percentage (`w-10`), the mute button
/// (`w-8`) and two `gap-3`s.
const TRACK: f64 = 160.0;

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

/// The out-of-room volume and mute as last set, for the page: the menu writes
/// it, and the tiles outside our subroom dim to match it.
#[derive(Clone, Copy)]
pub struct CrosstalkLevel(pub Signal<Crosstalk>);

/// Makes the shared [`CrosstalkLevel`] available to the menu and the tiles,
/// starting from what the audio holds (the saved setting, by then).
pub fn use_crosstalk_provider(audio: &Controls) {
    let initial = audio.crosstalk();
    use_context_provider(|| CrosstalkLevel(Signal::new(initial)));
}

#[derive(Clone, PartialEq, Props)]
pub struct SubroomMenuProps {
    /// Holds the out-of-room volume and mute.
    pub audio: Arc<Controls>,
    /// Ourselves: choosing a subroom records it here for the room to hear of.
    pub local: Arc<PeerStats>,
    /// Our key in [`Rooms`], the same `who` our own tile has.
    pub me: String,
    /// Whether we are on a call. Off a call the menu is hidden, not removed.
    pub shown: bool,
}

#[component]
pub fn SubroomMenu(props: SubroomMenuProps) -> Element {
    let OpenMenu(mut current) = use_context::<OpenMenu>();
    let Rooms(rooms) = use_context::<Rooms>();
    let shown = props.shown;
    let open = move || shown && current() == Some(NAME);
    // **Closed when the call goes.** The open flag is the page's, so left set
    // through a reconnect it would bring the panel and its click-away layer
    // back open the moment the call returned.
    use_effect(use_reactive!(|shown| {
        if !shown && *current.peek() == Some(NAME) {
            current.set(None);
        }
    }));
    let CrosstalkLevel(mut crosstalk) = use_context::<CrosstalkLevel>();
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
        // **Mounted always, hidden off a call**, for the F9 reason below: the
        // panel and layer under it carry a positive z-index.
        div { class: if props.shown { "relative shrink-0" } else { "hidden relative shrink-0" },
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
                            class: "flex h-5 w-40 shrink-0 flex-row items-center",
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
                            div { class: "relative h-2 w-40 rounded-full bg-neutral-700",
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

/// The names a new subroom draws from. Kept large so that two picks made at
/// once rarely meet, and short enough for the menu row and the tile label.
const HOUSE: [&str; 20] = [
    "Attic", "Balcony", "Boathouse", "Cellar", "Garage", "Greenhouse", "Hallway", "Kitchen", "Library", "Loft",
    "Lounge", "Nursery", "Pantry", "Porch", "Shed", "Study", "Sunroom", "Treehouse", "Veranda", "Workshop",
];

/// The number in a fallback name, "Room 3" -> 3.
fn number(name: &str) -> Option<u32> {
    name.strip_prefix("Room ")?.parse().ok()
}

/// Where a subroom sorts: rooms of the house alphabetically, then "Room n" by
/// number, so "Room 10" follows "Room 9", then any other name alphabetically.
/// Shared by this menu and the tile groups.
pub(super) fn room_order(name: &str) -> (u8, u32, &str) {
    if HOUSE.contains(&name) {
        (0, 0, name)
    } else if let Some(n) = number(name) {
        (1, n, name)
    } else {
        (2, 0, name)
    }
}

/// The name a new subroom takes: a room of the house no open subroom uses,
/// picked at random; with all of them in use, the smallest free "Room n".
pub(crate) fn next_free_name(open: &[&str]) -> String {
    free_name(open, roll())
}

/// [`next_free_name`] with the random number supplied, so tests can choose it.
fn free_name(open: &[&str], roll: u64) -> String {
    let free: Vec<&str> = HOUSE.iter().copied().filter(|name| !open.contains(name)).collect();
    if !free.is_empty() {
        return free[(roll % free.len() as u64) as usize].to_string();
    }
    let taken: BTreeSet<u32> = open.iter().filter_map(|name| number(name)).collect();
    let n = (1..).find(|n| !taken.contains(n)).unwrap_or(1);
    format!("Room {n}")
}

/// A random number from the clock, scrambled (splitmix64). Two people pressing
/// at once are on different machines with different clocks, which is all the
/// randomness this needs; no crate in the tree is worth adding for it.
fn roll() -> u64 {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    let mut z = nanos.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The open subrooms and who is in each, from everyone's room.
///
/// A subroom exists only by someone naming it, so an empty one cannot appear,
/// and the main room is not one. Sorted by [`room_order`].
pub(crate) fn open_subrooms(rooms: &BTreeMap<String, Option<String>>) -> Vec<(String, Vec<String>)> {
    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, room) in rooms {
        if let Some(room) = room {
            by_name.entry(room.clone()).or_default().push(id.clone());
        }
    }
    let mut open: Vec<_> = by_name.into_iter().collect();
    open.sort_by(|(a, _), (b, _)| room_order(a).cmp(&room_order(b)));
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slider_row_fits_the_panel() {
        // `w-72` (288) less `p-1` and the row's `px-2.5` each side is 260;
        // the row is the track, `gap-3`, `w-10`, `gap-3`, the `w-8` button,
        // all `shrink-0`, so an overflow pushes the button outside the border.
        assert!(TRACK + 12.0 + 40.0 + 12.0 + 32.0 <= 260.0);
    }

    #[test]
    fn a_new_subroom_is_a_free_room_of_the_house() {
        let open = ["Kitchen", "Attic", "Room 1", "Workshop"];
        for roll in 0..100 {
            let name = free_name(&open, roll);
            assert!(HOUSE.contains(&name.as_str()), "{name} is not in the house");
            assert!(!open.contains(&name.as_str()), "{name} is already open");
        }
        for _ in 0..100 {
            let name = next_free_name(&open);
            assert!(HOUSE.contains(&name.as_str()) && !open.contains(&name.as_str()));
        }
    }

    #[test]
    fn every_free_room_can_be_picked() {
        let picked: BTreeSet<String> = (0..HOUSE.len() as u64).map(|roll| free_name(&[], roll)).collect();
        assert_eq!(picked.len(), HOUSE.len());
    }

    #[test]
    fn a_full_house_falls_back_to_numbers() {
        let mut open = HOUSE.to_vec();
        assert_eq!(next_free_name(&open), "Room 1");
        open.push("Room 1");
        assert_eq!(next_free_name(&open), "Room 2");
        open.extend(["Room 3", "Room x"]);
        assert_eq!(next_free_name(&open), "Room 2", "a gap is filled first");
    }

    #[test]
    fn rooms_sort_house_then_numbers_then_the_rest() {
        let mut names = ["Zebra", "Room 10", "Lounge", "Room 2", "Attic", "Basement"];
        names.sort_by_key(|name| room_order(name));
        assert_eq!(names, ["Attic", "Lounge", "Room 2", "Room 10", "Basement", "Zebra"]);
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
    fn subrooms_list_their_members_in_order() {
        let open = open_subrooms(&map(&[
            ("a", Some("Room 10")),
            ("b", Some("Room 2")),
            ("c", None),
            ("d", Some("Room 2")),
            ("e", Some("Kitchen")),
        ]));
        let names: Vec<_> = open.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["Kitchen", "Room 2", "Room 10"]);
        assert_eq!(open[1].1, ["b", "d"]);
    }

    #[test]
    fn a_subroom_everyone_left_is_gone() {
        let open = open_subrooms(&map(&[("a", Some("Attic")), ("b", Some("Loft"))]));
        assert_eq!(open.len(), 2);
        let open = open_subrooms(&map(&[("a", Some("Attic")), ("b", None)]));
        assert_eq!(open.len(), 1, "Loft closed when b left it");
        let mut taken: Vec<&str> = HOUSE.iter().copied().filter(|name| *name != "Loft").collect();
        taken.extend(open.iter().map(|(n, _)| n.as_str()));
        assert_eq!(next_free_name(&taken), "Loft", "and its name is free again");
    }
}
