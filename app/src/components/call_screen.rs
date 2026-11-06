//! The call screen: who to call while idle, everyone's video, and the bar.
//!
//! Layout only. The camera's state lives in `camera::use_camera_choice`, the
//! call's in `net::use_remote`, and the arrangement of the video in `layout`.

use cameras::DeviceId;
use dioxus_native::prelude::*;

use super::{
    ConnectRow, ControlBar, DropdownOption, ParticipantTile, layout, use_open_menu_provider,
    use_rooms_provider,
};
use crate::camera::{FrameSlot, Mirror, use_camera_choice};
use crate::net::use_remote;

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
    let tile = layout::tile_class(tiles);
    // Our tile's `who`, and so our key among everyone's rooms.
    let me = remote.me.read().clone().unwrap_or_default();

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
        div { class: layout::CONTAINER,
            ParticipantTile {
                label: "You",
                slot,
                // Mirrored, because that is what people expect of their own
                // preview. Remote participants get `Mirror::None`.
                mirror: Mirror::SelfView,
                // Our own state, so we see the card and badge others see.
                stats: Some(remote.local.clone()),
                // Our endpoint id once bound, so our avatar matches what
                // others see; loopback has none, and any constant will do.
                who: me.clone(),
                class: tile,
            }
            if let Some(class) = layout::break_after(tiles, 1) {
                div { class }
            }
            // **Keyed, and that is the point of the id.** Without a key dioxus
            // may reuse the wrong component instance when the list reorders,
            // and two people's video swaps tiles. Each card's line break shares
            // its key's fragment, so the two move together.
            for (index, participant) in participants.into_iter().enumerate() {
                Fragment { key: "{participant.id}",
                    ParticipantTile {
                        label: participant.short(),
                        slot: participant.slot.clone(),
                        // Not a self-view: these frames took the path a peer's
                        // would, and mirroring them would be the bug task 9 removed.
                        mirror: Mirror::None,
                        stats: Some(participant.stats.clone()),
                        who: participant.id.clone(),
                        class: tile,
                    }
                    if let Some(class) = layout::break_after(tiles, index + 2) {
                        div { class }
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
