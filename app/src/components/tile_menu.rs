//! The menu under a peer's tile label: join their subroom, invite them to
//! ours, mute their microphone, and, with moving the call switched on in
//! Experimental, move the call to them when they host the room, or back to
//! the relay from the tile of the host it is on.
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
pub enum Item {
    /// Go to their subroom, by name.
    Join(String),
    /// Ask them to come to ours.
    Invite,
    /// Ask their client to turn their microphone off.
    Mute,
    /// Move the call to them, a host: said in our presence, followed by
    /// everyone who has moving the call switched on.
    MoveHere,
    /// Move the call off them, back to the relay (or a host while the relay
    /// is unreachable). Offered where it was moved, because that is where
    /// people look for the way back.
    MoveBack,
}

/// Which move, if any, `who`'s menu offers: back to the relay when the call
/// is on them, here when they host it and it is not, nothing while moving
/// the call is switched off. Back is offered even if they have stopped
/// hosting, since the call is still said to be on them.
pub(super) fn move_item(move_calls: bool, they_host: bool, call_on: Option<&str>, who: &str) -> Option<Item> {
    if !move_calls {
        None
    } else if call_on == Some(who) {
        Some(Item::MoveBack)
    } else if they_host {
        Some(Item::MoveHere)
    } else {
        None
    }
}

/// What applies, given their room, ours, whether their mic is on, and the
/// move [`move_item`] offers. Inapplicable items are left out rather than
/// greyed.
pub(super) fn items(theirs: Option<&str>, ours: Option<&str>, their_mic: bool, moving: Option<Item>) -> Vec<Item> {
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
    items.extend(moving);
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
    /// What they call themselves, if anything. With one, the menu also
    /// shows their short id: a name is only a claim, and two people can
    /// make the same one.
    pub name: Option<String>,
    /// Their subroom, as their tile last sampled it.
    pub their_room: Option<String>,
    /// Whether their microphone is on.
    pub their_mic: bool,
    /// Their connection in one line, when they report it and our
    /// connection report is on. A line rather than a tooltip on the bars,
    /// since blitz has no tooltips.
    pub connection: Option<String>,
    /// The move offered on their tile, from [`move_item`].
    pub moving: Option<Item>,
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

    let offered = items(props.their_room.as_deref(), props.our_room.as_deref(), props.their_mic, props.moving.clone());
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
            if let Some(name) = props.name.as_ref() {
                div { class: "flex flex-col px-2.5 pb-1.5 pt-1",
                    span { class: "truncate text-sm text-neutral-100", "{name}" }
                    span { class: "text-xs text-neutral-500", {crate::name::short(&props.who)} }
                }
                div { class: "my-1 h-px bg-neutral-800" }
            }
            if let Some(line) = props.connection.as_ref() {
                div { class: "px-2.5 pb-1.5 pt-1 text-xs text-neutral-400", "{line}" }
                div { class: "my-1 h-px bg-neutral-800" }
            }
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
                                Item::MoveHere => local.move_call(Some(who.clone()), &me),
                                Item::MoveBack => local.move_call(None, &me),
                            }
                            current.set(None);
                        }
                    },
                    match &item {
                        Item::Join(room) => rsx! { "Join {room}" },
                        Item::Invite => rsx! { "{invite_to}" },
                        Item::Mute => rsx! { "Mute their mic" },
                        Item::MoveHere => rsx! { "Move the call here" },
                        Item::MoveBack => rsx! { "Move the call back to the relay" },
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
        assert_eq!(items(None, None, true, None), vec![Item::Mute]);
        assert_eq!(items(Some("Attic"), Some("Attic"), true, None), vec![Item::Mute]);
    }

    #[test]
    fn same_room_with_their_mic_off_offers_nothing() {
        assert_eq!(items(Some("Attic"), Some("Attic"), false, None), vec![]);
    }

    #[test]
    fn they_in_a_subroom_we_in_main() {
        assert_eq!(
            items(Some("Kitchen"), None, true, None),
            vec![Item::Join("Kitchen".into()), Item::Invite, Item::Mute]
        );
    }

    #[test]
    fn we_in_a_subroom_they_in_main() {
        assert_eq!(items(None, Some("Kitchen"), true, None), vec![Item::Invite, Item::Mute]);
    }

    #[test]
    fn a_host_offers_moving_the_call_last() {
        assert_eq!(items(None, None, false, Some(Item::MoveHere)), vec![Item::MoveHere]);
        assert_eq!(items(None, None, true, Some(Item::MoveHere)), vec![Item::Mute, Item::MoveHere]);
        assert_eq!(items(None, None, true, Some(Item::MoveBack)), vec![Item::Mute, Item::MoveBack]);
    }

    #[test]
    fn the_host_the_call_is_on_offers_moving_it_back_instead_of_here() {
        assert_eq!(move_item(true, true, Some("a"), "a"), Some(Item::MoveBack));
        // Still the way back after they stop hosting.
        assert_eq!(move_item(true, false, Some("a"), "a"), Some(Item::MoveBack));
    }

    #[test]
    fn another_host_offers_here_and_never_back() {
        assert_eq!(move_item(true, true, Some("a"), "b"), Some(Item::MoveHere));
        assert_eq!(move_item(true, true, None, "b"), Some(Item::MoveHere));
        assert_eq!(move_item(true, false, Some("a"), "b"), None);
        assert_eq!(move_item(true, false, None, "b"), None);
    }

    #[test]
    fn nothing_to_move_while_the_switch_is_off() {
        assert_eq!(move_item(false, true, None, "b"), None);
        assert_eq!(move_item(false, true, Some("a"), "a"), None);
    }

    #[test]
    fn two_different_subrooms() {
        assert_eq!(
            items(Some("Attic"), Some("Loft"), false, None),
            vec![Item::Join("Attic".into()), Item::Invite]
        );
    }
}
