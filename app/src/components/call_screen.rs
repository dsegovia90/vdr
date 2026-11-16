//! The call screen: who to call while idle, everyone's video, and the bar.
//!
//! Layout, and the keyboard shortcuts. The camera's state lives in
//! `camera::use_camera_choice`, the call's in `net::use_remote`, the
//! arrangement of the video in `layout`, and what a key means in `hotkeys`.
//!
//! **The page's root element is here**, not in `main`, because it is where
//! keys arrive (see `keys`) and the actions they trigger need the call.

use cameras::DeviceId;
use dioxus_native::prelude::*;

use super::device_menu::OpenMenu;
use super::keys::{chord_of, use_keys_provider, use_settings_panel_provider};
use super::microphone::set_mic;
use super::tile::{UNDIMMED, dim_class};
use super::{
    ConnectRow, ControlBar, CrosstalkLevel, DropdownOption, OwnShare, ParticipantTile, Rooms, SettingsModal,
    ShareTile, layout, settings, share, use_crosstalk_provider, use_open_menu_provider, use_rooms_provider,
};
use crate::audio::use_shared_microphones;
use crate::camera::{FrameSlot, Mirror, use_camera_choice};
use crate::hotkeys::Action;
use crate::net::{Feed, Participant, share_available, use_remote};

/// One child of the tile area, in order: a group's label, or a card with the
/// line break after it. All in one keyed list, so someone changing subroom
/// moves within it rather than unmounting and mounting again elsewhere, which
/// would rebuild their video and drop what their tile has sampled.
struct Item {
    key: String,
    /// The label's text and classes.
    label: Option<(String, &'static str)>,
    /// The peer whose card this is: `None` for our own, and for a label.
    peer: Option<Participant>,
    /// Whether the card is a shared screen rather than a camera.
    screen: bool,
    tile: bool,
    /// The card's size and placement: `layout::TILE` in rows, `SHARE` or
    /// `STRIP` in share mode.
    class: &'static str,
    dim: &'static str,
    line_break: Option<&'static str>,
}

#[component]
pub fn CallScreen() -> Element {
    // One open menu at a time across the bar; see `device_menu::OpenMenu`.
    use_open_menu_provider();
    // Everyone's subroom, filled by the tiles and read by the subroom menu.
    use_rooms_provider();
    let (focus, hotkeys) = use_keys_provider();
    let panel = use_settings_panel_provider();
    let OpenMenu(mut menu) = use_context::<OpenMenu>();
    let microphones = use_shared_microphones();

    // Shared between the capture thread that fills it and the paint source that
    // drains it on the render thread.
    let slot = use_hook(FrameSlot::default);
    // A peer's video, or our own round-tripped through the codec when there is
    // no peer to call. Either way it arrives decoded, in its own slot.
    let remote = use_remote();
    // The out-of-room volume, shared by the subroom menu that sets it and the
    // tiles that dim to it. After `use_remote`, which loads the saved setting.
    use_crosstalk_provider(&remote.audio);
    let Rooms(rooms) = use_context::<Rooms>();
    let CrosstalkLevel(crosstalk) = use_context::<CrosstalkLevel>();
    let camera = use_camera_choice(slot.clone(), remote.tap.clone());

    // The camera button and its shortcut, one path: the room hears of it
    // through our presence either way.
    let toggle_camera = use_callback({
        let local = remote.local.clone();
        move |()| {
            camera.toggle();
            // Told to the room by the call's announcer, and shown on our own
            // tile the same way others will see it.
            local.set_camera(*camera.enabled.peek());
        }
    });

    let camera_options: Vec<DropdownOption> = camera
        .devices
        .devices
        .read()
        .iter()
        .map(|device| DropdownOption {
            value: device.id.0.clone(),
            label: device.name.clone(),
        })
        .collect();

    // Read into a Vec first: holding the signal's guard across the nodes below
    // would keep it borrowed while they render.
    let all = remote.participants.read().clone();
    let (cameras, screens): (Vec<Participant>, Vec<Participant>) =
        all.into_iter().partition(|participant| participant.feed == Feed::Camera);
    let OwnShare(own_share) = use_context::<OwnShare>();
    let (sharing, _) = own_share();

    // **One key press, one action, by the bar's own code paths.** Esc and
    // the shortcut recorder come first, since they work with the settings
    // open; everything else waits while the settings are open or a text field
    // has the keyboard. Held keys repeat, and a repeat is ignored, so holding
    // `m` does not flap the microphone.
    let on_key = {
        let audio = remote.audio.clone();
        let local = remote.local.clone();
        let share = remote.share.clone();
        let status = remote.status;
        let cancel = remote.cancel;
        move |event: KeyboardEvent| {
            if event.is_auto_repeating() {
                return;
            }
            focus.key_pressed(matches!(event.key(), Key::Tab));
            let Some(chord) = chord_of(&event) else { return };
            // Read into a local first: a borrow taken in the `if let` itself
            // lasts for the whole block, and recording writes this signal.
            let recording = *panel.recording.peek();
            if let Some(action) = recording {
                settings::record(panel, hotkeys, action, chord);
                return;
            }
            if chord.is_escape() {
                if *panel.open.peek() {
                    panel.close(focus);
                } else if menu.peek().is_some() {
                    menu.set(None);
                }
                return;
            }
            if *focus.typing.peek() || *panel.open.peek() {
                return;
            }
            let Some(action) = hotkeys.0.peek().lookup(chord) else { return };
            tracing::debug!("shortcut {chord}: {}", action.id());
            match action {
                Action::ToggleMic => {
                    // The button does nothing without a microphone either.
                    if microphones.devices.peek().is_empty() {
                        tracing::debug!("no microphone to toggle");
                        return;
                    }
                    set_mic(&audio, &local, !audio.enabled());
                }
                Action::ToggleCamera => toggle_camera.call(()),
                Action::ShareScreen => {
                    let on = own_share.peek().0 || share.get().is_some();
                    // Only where the Share button is shown: on a call, on a
                    // build that can capture the screen. Stopping is always
                    // allowed.
                    if on || (status.peek().is_connected() && share_available()) {
                        share::press(&share, on, menu);
                    }
                }
                Action::Leave => {
                    if status.peek().is_cancellable() {
                        cancel.call(());
                    }
                }
                Action::OpenSettings => panel.show(),
            }
        }
    };
    // Our tile's `who`, and so our key among everyone's rooms.
    let me = remote.me.read().clone().unwrap_or_default();

    // **Every card in order: ours, then each camera followed by that person's
    // screen.** A screen is one more card in its sharer's group, so it is
    // grouped by its sharer's id; a screen whose sharer has no camera card
    // goes at the end, still in their group. `None` is our own card.
    let mut cards: Vec<(Option<Participant>, bool)> = vec![(None, false)];
    if sharing {
        cards.push((None, true));
    }
    for camera in &cameras {
        cards.push((Some(camera.clone()), false));
        if let Some(screen) = screens.iter().find(|screen| screen.id == camera.id) {
            cards.push((Some(screen.clone()), true));
        }
    }
    for screen in &screens {
        if !cameras.iter().any(|camera| camera.id == screen.id) {
            cards.push((Some(screen.clone()), true));
        }
    }
    let tiles = cards.len();

    let rooms = rooms.read().clone();
    let ours = rooms.get(&me).cloned().flatten();
    let dim = dim_class(crosstalk());
    let ids: Vec<&str> = cards
        .iter()
        .map(|(peer, _)| peer.as_ref().map_or(me.as_str(), |peer| peer.id.as_str()))
        .collect();
    let groups = layout::groups(&ids, &rooms);
    let cols = layout::columns(tiles);
    // **Labels only once there is a subroom**: a call with none looks as it
    // always has. Once there is one, every group has a label, main included.
    let labelled = groups.iter().any(|group| group.room.is_some());
    let labels = if labelled { groups.len() } else { 0 };
    let sizing = layout::sizing(layout::rows(&groups, cols), cols, labels);

    // **Share mode** when a share's sharer is in our room, ours included; see
    // `layout`. A share's room is its sharer's.
    let room_of = |index: usize| rooms.get(ids[index]).and_then(Option::as_deref);
    let shares: Vec<usize> = (0..cards.len()).filter(|&index| cards[index].1).collect();
    let share_rooms: Vec<layout::Share> = shares
        .iter()
        .map(|&index| layout::Share { room: room_of(index) })
        .collect();
    let here: Vec<usize> = layout::here(&share_rooms, ours.as_deref())
        .into_iter()
        .map(|position| shares[position])
        .collect();
    let share_mode = !here.is_empty();
    let strip = layout::strip(&groups, ours.as_deref(), &here);
    let sizing = if share_mode {
        layout::share_sizing(layout::share_grid(here.len()), strip.len())
    } else {
        sizing
    };

    // Each card once, by index, with the key it keeps in both modes.
    let mut card_items: Vec<Option<Item>> = Vec::with_capacity(tiles);
    for (index, (peer, screen)) in cards.iter().cloned().enumerate() {
        // **Ours is keyed with a constant**, not `who`: that changes when the
        // endpoint binds, and a new key would remount it. A peer's carries the
        // arrival, so a source that comes back with a new slot gets a new tile
        // (see `Participant::generation`). Keys with colons never meet a room
        // label's: ids are hex.
        let key = match (&peer, screen) {
            (None, false) => "self".to_string(),
            (None, true) => "screen:self".to_string(),
            (Some(peer), false) => format!("{}:{}", peer.id, peer.generation),
            (Some(peer), true) => format!("screen:{}:{}", peer.id, peer.generation),
        };
        let mine = room_of(index) == ours.as_deref();
        card_items.push(Some(Item {
            key,
            label: None,
            peer,
            screen,
            tile: true,
            class: layout::TILE,
            dim: if mine { UNDIMMED } else { dim },
            line_break: None,
        }));
    }
    let mut take = |index: usize| card_items[index].take();

    let mut items = Vec::with_capacity(tiles + labels);
    let mut label_items = Vec::with_capacity(labels);
    for group in &groups {
        let mine = group.room == ours;
        if labelled {
            let name = group.room.as_deref().unwrap_or("Main room");
            label_items.push(Item {
                // "main" has no colon, so no room name can produce it.
                key: group.room.as_ref().map_or_else(|| "main".to_string(), |room| format!("room:{room}")),
                label: Some((
                    // People, not cards: a screen is not someone else.
                    format!("{name} \u{b7} {}", people(&group.members, &ids)),
                    if share_mode {
                        layout::LABEL_HIDDEN
                    } else if mine {
                        layout::LABEL_MINE
                    } else {
                        layout::LABEL
                    },
                )),
                peer: None,
                screen: false,
                tile: false,
                class: layout::TILE,
                dim: UNDIMMED,
                line_break: None,
            });
        }
    }
    if share_mode {
        // The shares above, the break, the strip; then the labels, hidden but
        // kept, so a return to rows does not mount them anew.
        for (position, &index) in here.iter().enumerate() {
            if let Some(mut item) = take(index) {
                item.class = layout::SHARE;
                item.dim = UNDIMMED;
                if position + 1 == here.len() {
                    item.line_break = Some(layout::SHARE_BREAK);
                }
                items.push(item);
            }
        }
        for &index in &strip {
            if let Some(mut item) = take(index) {
                item.class = layout::STRIP;
                items.push(item);
            }
        }
        items.extend(label_items);
    } else {
        let mut labels = label_items.into_iter();
        for group in &groups {
            if labelled {
                items.extend(labels.next());
            }
            for (position, &index) in group.members.iter().enumerate() {
                if let Some(mut item) = take(index) {
                    item.line_break = layout::break_after(cols, group.members.len(), position + 1);
                    items.push(item);
                }
            }
        }
    }

    rsx! {
        // `h-screen`, not `h-full`: a percentage height on the root never
        // resolves in blitz -- checked on both the old pin and beta.4 -- and
        // without a definite height the video area cannot take what is left,
        // so the bar floats mid-window. `overflow-hidden` because nothing here
        // should ever scroll, and it stopped a one-pixel scrollbar flicker.
        //
        // Focusable from code only (`tabindex` -1), so it can hold the
        // keyboard for the shortcuts; see `keys`. `outline-none`, or holding
        // it would draw a ring round the whole window.
        div {
            class: "flex h-screen flex-col gap-4 overflow-hidden p-6 outline-none",
            tabindex: "-1",
            onmounted: move |event| focus.mounted(event.data()),
            onkeydown: on_key,
            // Blitz drops focus on a click anywhere it does not consider
            // focusable, which is every button we have; the blur comes after
            // it has.
            onblur: move |_| focus.root_blurred(),
            // For a click after focus went with no blur at all (a focused
            // field removed): focused here, then cleared by the click, then
            // restored by the blur above.
            onclick: move |_| focus.restore(),
        // Only while there is no call: your key and who to call. A call is the
        // grid and the bar below, nothing else.
        ConnectRow {
            me: remote.me,
            status: remote.status,
            connect: remote.connect,
        }
        // **Everyone in one area, ourselves included, and it takes whatever
        // height is left.** `flex-1 min-h-0` inside the app's full-height
        // column is what makes it shrink to fit rather than push the page into
        // scrolling. The rows hug the middle; see `layout`.
        div { class: layout::CONTAINER, style: sizing,
            // **Keyed, and that is the point of the id.** Without a key dioxus
            // may reuse the wrong component instance when the list reorders,
            // and two people's video swaps tiles. Each card's line break shares
            // its key's fragment, so the two move together.
            for item in items {
                Fragment { key: "{item.key}",
                    if let Some((text, class)) = item.label {
                        div { class, "{text}" }
                    }
                    if item.tile && item.screen {
                        if let Some(participant) = item.peer {
                            ShareTile {
                                label: format!("{} \u{b7} screen", participant.short()),
                                slot: participant.slot.clone(),
                                class: item.class,
                                dim: item.dim,
                            }
                        } else {
                            ShareTile {
                                label: "You \u{b7} screen",
                                slot: remote.share.preview(),
                                class: item.class,
                                dim: item.dim,
                                stop: {
                                    let share = remote.share.clone();
                                    EventHandler::new(move |_| share.set(None))
                                },
                            }
                        }
                        if let Some(class) = item.line_break {
                            div { class }
                        }
                    } else if item.tile {
                        if let Some(participant) = item.peer {
                            ParticipantTile {
                                label: participant.short(),
                                slot: participant.slot.clone(),
                                // Not a self-view: these frames took the path a
                                // peer's would, and mirroring them would be the
                                // bug task 9 removed.
                                mirror: Mirror::None,
                                stats: Some(participant.stats.clone()),
                                who: participant.id.clone(),
                                class: item.class,
                                dim: item.dim,
                                local: remote.local.clone(),
                                me: me.clone(),
                            }
                        } else {
                            ParticipantTile {
                                label: "You",
                                slot: slot.clone(),
                                // Mirrored, because that is what people expect
                                // of their own preview.
                                mirror: Mirror::SelfView,
                                // Our own state, so we see the card and badge
                                // others see.
                                stats: Some(remote.local.clone()),
                                // Our endpoint id once bound, so our avatar
                                // matches what others see; loopback has none,
                                // and any constant will do.
                                who: me.clone(),
                                class: item.class,
                                dim: item.dim,
                                local: remote.local.clone(),
                                me: me.clone(),
                            }
                        }
                        if let Some(class) = item.line_break {
                            div { class }
                        }
                    }
                }
            }
        }
        ControlBar {
            audio: remote.audio.clone(),
            local: remote.local.clone(),
            me,
            camera_on: (camera.enabled)(),
            cameras: camera_options,
            camera: camera.selected.read().as_ref().map(|id| id.0.clone()),
            on_camera_toggle: move |_| toggle_camera.call(()),
            on_camera_select: move |value: String| camera.choose(DeviceId(value)),
            on_camera_rescan: move |_| camera.devices.refresh.call(()),
            camera_problem: camera.problem(),
            status: remote.status,
            metrics: remote.metrics.clone(),
            leave: remote.cancel,
            share: remote.share.clone(),
        }
        // Last, so it paints over everything, the video included.
        SettingsModal {}
        }
    }
}

/// How many people a group's cards belong to: someone sharing has two cards.
fn people(members: &[usize], ids: &[&str]) -> usize {
    members
        .iter()
        .map(|&index| ids[index])
        .collect::<std::collections::HashSet<_>>()
        .len()
}

#[cfg(test)]
mod tests {
    use super::people;

    #[test]
    fn a_sharer_is_counted_once() {
        let ids = ["me", "a", "a", "b"];
        assert_eq!(people(&[0, 1, 2, 3], &ids), 3);
        assert_eq!(people(&[1, 2], &ids), 1);
    }
}
