//! The call screen: who to call while idle, everyone's video, and the bar.
//!
//! Layout only. The camera's state lives in `camera::use_camera_choice`, the
//! call's in `net::use_remote`, and the arrangement of the video in `layout`.

use cameras::DeviceId;
use dioxus_native::prelude::*;

use super::tile::{UNDIMMED, dim_class};
use super::{
    ConnectRow, ControlBar, CrosstalkLevel, DropdownOption, ParticipantTile, Rooms, layout,
    use_crosstalk_provider, use_open_menu_provider, use_rooms_provider,
};
use crate::camera::{FrameSlot, Mirror, use_camera_choice};
use crate::net::{Participant, use_remote};

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
    tile: bool,
    dim: &'static str,
    line_break: Option<&'static str>,
}

#[component]
pub fn CallScreen() -> Element {
    // One open menu at a time across the bar; see `device_menu::OpenMenu`.
    use_open_menu_provider();
    // Everyone's subroom, filled by the tiles and read by the subroom menu.
    use_rooms_provider();

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
    let participants = remote.participants.read().clone();
    let tiles = 1 + participants.len();
    // Our tile's `who`, and so our key among everyone's rooms.
    let me = remote.me.read().clone().unwrap_or_default();

    let rooms = rooms.read().clone();
    let ours = rooms.get(&me).cloned().flatten();
    let dim = dim_class(crosstalk());
    let ids: Vec<&str> = std::iter::once(me.as_str())
        .chain(participants.iter().map(|participant| participant.id.as_str()))
        .collect();
    let groups = layout::groups(&ids, &rooms);
    let cols = layout::columns(tiles);
    // **Labels only once there is a subroom**: a call with none looks as it
    // always has. Once there is one, every group has a label, main included.
    let labelled = groups.iter().any(|group| group.room.is_some());
    let labels = if labelled { groups.len() } else { 0 };
    let sizing = layout::sizing(layout::rows(&groups, cols), cols, labels);

    let mut items = Vec::with_capacity(tiles + labels);
    for group in &groups {
        let mine = group.room == ours;
        if labelled {
            let name = group.room.as_deref().unwrap_or("Main room");
            items.push(Item {
                // "main" has no colon, so no room name can produce it.
                key: group.room.as_ref().map_or_else(|| "main".to_string(), |room| format!("room:{room}")),
                label: Some((
                    format!("{name} \u{b7} {}", group.members.len()),
                    if mine { layout::LABEL_MINE } else { layout::LABEL },
                )),
                peer: None,
                tile: false,
                dim: UNDIMMED,
                line_break: None,
            });
        }
        for (position, &index) in group.members.iter().enumerate() {
            let peer = index.checked_sub(1).map(|peer| participants[peer].clone());
            items.push(Item {
                // **Ours is keyed with a constant**, not `who`: that changes
                // when the endpoint binds, and a new key would remount it.
                key: peer.as_ref().map_or_else(|| "self".to_string(), |peer| peer.id.clone()),
                label: None,
                peer,
                tile: true,
                dim: if mine { UNDIMMED } else { dim },
                line_break: layout::break_after(cols, group.members.len(), position + 1),
            });
        }
    }

    rsx! {
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
                    if item.tile {
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
                                class: layout::TILE,
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
                                class: layout::TILE,
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
            on_camera_toggle: {
                let local = remote.local.clone();
                move |_| {
                    camera.toggle();
                    // Told to the room by the call's announcer, and shown on
                    // our own tile the same way others will see it.
                    local.set_camera(*camera.enabled.peek());
                }
            },
            on_camera_select: move |value: String| camera.choose(DeviceId(value)),
            on_camera_rescan: move |_| camera.devices.refresh.call(()),
            camera_problem: camera.problem(),
            status: remote.status,
            metrics: remote.metrics.clone(),
            leave: remote.cancel,
        }
    }
}
