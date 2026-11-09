//! The menu under a peer's tile label: join their subroom, invite them to
//! ours, mute their microphone.
//!
//! Joining is local and instant: subrooms are open, so it only sets our own
//! room. Inviting and muting are requests said in our presence document and
//! acted on by the addressee (`net::call`); nothing here waits for them.
//!
//! The label itself is the trigger and lives in `tile`; this is the layer and
//! panel, placed beside the label rather than inside it, so a click on an item
//! does not bubble to the label and reopen the menu.

use std::sync::Arc;

use dioxus_native::prelude::*;

use super::Rooms;
use super::device_menu::OpenMenu;
use super::subroom::join;
use crate::net::{Ask, PeerStats};

/// One thing the menu offers.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Item {
    /// Go to their subroom, by name.
    Join(String),
    /// Ask them to come to ours.
    Invite,
    /// Ask their client to turn their microphone off.
    Mute,
}

/// What applies, given their room, ours, and whether their mic is on.
/// Inapplicable items are left out rather than greyed.
pub(super) fn items(theirs: Option<&str>, ours: Option<&str>, their_mic: bool) -> Vec<Item> {
    let mut items = Vec::new();
    if let Some(room) = theirs {
        if Some(room) != ours {
            items.push(Item::Join(room.to_string()));
        }
    }
    if theirs != ours {
        items.push(Item::Invite);
    }
    if their_mic {
        items.push(Item::Mute);
    }
    items
}

/// The page's [`OpenMenu`] name for `who`'s tile. No bar menu's name has a
/// colon, so it cannot collide with one.
pub(super) fn menu_name(who: &str) -> String {
    format!("tile:{who}")
}

#[derive(Clone, PartialEq, Props)]
pub struct TileMenuProps {
    /// Whose tile this is, by endpoint id.
    pub who: String,
    /// Their subroom, as their tile last sampled it.
    pub their_room: Option<String>,
    /// Whether their microphone is on.
    pub their_mic: bool,
    /// Our subroom, from the page's [`Rooms`].
    pub our_room: Option<String>,
    /// Ourselves: where requests are recorded, and our room is set.
    pub local: Arc<PeerStats>,
    /// Our key in [`Rooms`].
    pub me: String,
}

#[component]
pub fn TileMenu(props: TileMenuProps) -> Element {
    let OpenMenu(mut current) = use_context::<OpenMenu>();
    let Rooms(rooms) = use_context::<Rooms>();
    let name = menu_name(&props.who);
    let open = current.read().as_deref() == Some(name.as_str());

    // **Closed when the tile goes**, so a peer who leaves and returns does
    // not come back with their menu open. The page may be going too.
    use_drop({
        let name = name.clone();
        move || {
            if let Ok(mut current) = current.try_write() {
                if current.as_deref() == Some(name.as_str()) {
                    *current = None;
                }
            }
        }
    });

    let offered = items(props.their_room.as_deref(), props.our_room.as_deref(), props.their_mic);
    let invite_to = match props.our_room.as_deref() {
        Some(_) => "Invite to my subroom",
        None => "Invite to main room",
    };

    rsx! {
        // **Mounted always, hidden while closed**, as in `device_menu`: the
        // layer and panel have a positive z-index, and blitz panicked
        // hit-testing such a node after it was removed.
        div {
            class: if open { "fixed inset-0 z-[5]" } else { "hidden fixed inset-0 z-[5]" },
            onclick: move |_| current.set(None),
        }
        // **Opens upward from the label**, as the bar's menus do: `bottom-10`
        // clears the label (`bottom-2`, about 24 px tall, and a gap). Narrower
        // than the bar's menus so it does not leave a narrow tile.
        div {
            class: if open {
                "absolute bottom-10 left-2 z-10 w-48 rounded-lg border border-neutral-700 bg-neutral-900 p-1"
            } else {
                "hidden absolute bottom-10 left-2 z-10 w-48 rounded-lg border border-neutral-700 bg-neutral-900 p-1"
            },
            if offered.is_empty() {
                div { class: "px-2.5 py-2 text-sm text-neutral-500", "Nothing to do" }
            }
            for item in offered {
                div {
                    key: "{item:?}",
                    class: "cursor-pointer rounded px-2.5 py-2 text-sm text-neutral-200 hover:bg-neutral-800",
                    onclick: {
                        let item = item.clone();
                        let local = props.local.clone();
                        let who = props.who.clone();
                        let me = props.me.clone();
                        move |_| {
                            match &item {
                                Item::Join(room) => {
                                    join(&local, rooms, &me, Some(room.clone()));
                                    tracing::info!("joined {room}, {who}'s subroom");
                                }
                                Item::Invite => {
                                    local.ask(&who, Ask::Invite);
                                    tracing::info!("asked {who}: {:?}", Ask::Invite);
                                }
                                Item::Mute => {
                                    local.ask(&who, Ask::Mute);
                                    tracing::info!("asked {who}: {:?}", Ask::Mute);
                                }
                            }
                            current.set(None);
                        }
                    },
                    match &item {
                        Item::Join(room) => rsx! { "Join {room}" },
                        Item::Invite => rsx! { "{invite_to}" },
                        Item::Mute => rsx! { "Mute their mic" },
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_room_with_their_mic_on_offers_mute() {
        assert_eq!(items(None, None, true), vec![Item::Mute]);
        assert_eq!(items(Some("Attic"), Some("Attic"), true), vec![Item::Mute]);
    }

    #[test]
    fn same_room_with_their_mic_off_offers_nothing() {
        assert_eq!(items(Some("Attic"), Some("Attic"), false), vec![]);
    }

    #[test]
    fn they_in_a_subroom_we_in_main() {
        assert_eq!(
            items(Some("Kitchen"), None, true),
            vec![Item::Join("Kitchen".into()), Item::Invite, Item::Mute]
        );
    }

    #[test]
    fn we_in_a_subroom_they_in_main() {
        assert_eq!(items(None, Some("Kitchen"), true), vec![Item::Invite, Item::Mute]);
    }

    #[test]
    fn two_different_subrooms() {
        assert_eq!(
            items(Some("Attic"), Some("Loft"), false),
            vec![Item::Join("Attic".into()), Item::Invite]
        );
    }
}
