//! Rooms joined before, under the connect row, one click from joining again.
//!
//! What is shown is a room's name and its relay's host, never its link: a link
//! can carry the room's token. See [`crate::recent`].

use dioxus_native::prelude::*;

use super::connect::join_room;
use super::{Button, CloseIcon, PinIcon, Size};
use crate::args::Peering;
use crate::net::RemoteStatus;
use crate::recent::{self, RecentRooms};

/// One row's worth, read out of the list so no guard is held while rendering.
struct Row {
    key: String,
    name: String,
    host: String,
    when: String,
    link: String,
    pinned: bool,
}

/// The list, while idle and only when there is something in it.
///
/// **Short, and scrolls rather than grows**: the page is a fixed-height column
/// whose video area takes what is left, so a long list here would squeeze the
/// video and could push the bar out of the window.
#[component]
pub fn RecentRoomList(
    rooms: Signal<RecentRooms>,
    /// Read to decide whether to render, and reported into on a bad link.
    status: Signal<RemoteStatus>,
    /// The connect row's own, so a Join behaves as Connect does.
    connect: Callback<Peering>,
) -> Element {
    let mut rooms = rooms;
    // The same states that show the paste field: not joining, not in a call,
    // not loopback.
    if !status.read().is_waiting() {
        return rsx! {};
    }
    let now = recent::now();
    let rows: Vec<Row> = rooms
        .read()
        .rooms()
        .iter()
        .map(|room| Row {
            key: room.key(),
            name: room.name(),
            host: room.host(),
            when: recent::relative(room.last_joined(), now),
            link: room.link().to_string(),
            pinned: room.pinned(),
        })
        .collect();
    if rows.is_empty() {
        return rsx! {};
    }

    rsx! {
        div { class: "flex max-h-40 w-full max-w-2xl shrink-0 flex-col gap-1",
            span { class: "text-sm text-neutral-400", "Recent rooms" }
            div { class: "flex min-h-0 flex-col overflow-y-auto",
                for row in rows {
                    div { key: "{row.key}", class: "flex flex-row items-center gap-3 rounded-md px-2 py-1 hover:bg-neutral-900",
                        span { class: "min-w-0 truncate text-sm text-neutral-100", "{row.name}" }
                        span { class: "min-w-0 truncate text-xs text-neutral-500", "{row.host}" }
                        span { class: "ml-auto shrink-0 whitespace-nowrap text-xs text-neutral-500", "{row.when}" }
                        Button {
                            size: Size::Small,
                            onclick: {
                                let link = row.link.clone();
                                move |_| join_room(&link, connect, status)
                            },
                            "Join"
                        }
                        button {
                            // Plain colours, not palette shades: an inline
                            // icon takes `currentColor`, which drops oklch.
                            class: if row.pinned {
                                "flex h-6 w-6 shrink-0 cursor-pointer items-center justify-center rounded-md text-white hover:bg-neutral-800"
                            } else {
                                "flex h-6 w-6 shrink-0 cursor-pointer items-center justify-center rounded-md text-[rgb(115,115,115)] hover:bg-neutral-800"
                            },
                            onclick: {
                                let key = row.key.clone();
                                let pinned = row.pinned;
                                move |_| recent::change(&mut rooms, |list| list.set_pinned(&key, !pinned))
                            },
                            PinIcon { on: row.pinned }
                        }
                        button {
                            class: "flex h-6 w-6 shrink-0 cursor-pointer items-center justify-center rounded-md text-[rgb(115,115,115)] hover:bg-neutral-800",
                            onclick: {
                                let key = row.key.clone();
                                move |_| recent::change(&mut rooms, |list| list.remove(&key))
                            },
                            CloseIcon {}
                        }
                    }
                }
            }
        }
    }
}
