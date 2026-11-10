//! The bottom control bar: microphone, camera and screen sharing on the left,
//! what the call is doing in the middle, and the subroom menu and Leave on the
//! right.
//!
//! **Small on purpose.** Everything here used to be full-width rows above and
//! below the video, and in a default-size window they left four people 46
//! pixels of height each. One row of icons gives the grid nearly all of it.

use std::sync::Arc;

use dioxus_native::prelude::*;

use super::{
    CameraIcon, DeviceMenu, DropdownOption, LeaveIcon, MenuSection, MicControl, OwnShare, ShareControl, StatsRow,
    SubroomMenu,
};
use crate::audio::Controls;
use crate::net::{Metrics, PeerStats, RemoteStatus, ShareWish, share_available};

#[derive(Clone, PartialEq, Props)]
pub struct ControlBarProps {
    /// The microphone, which manages its own device choice.
    pub audio: Arc<Controls>,
    /// Ourselves: the mic toggle records itself here for the room to hear of.
    pub local: Arc<PeerStats>,
    /// Our endpoint id as our own tile has it, for the subroom menu.
    pub me: String,
    /// Whether the camera is on, by the user's choice.
    pub camera_on: bool,
    /// Cameras to choose from, and the chosen one's `value`.
    pub cameras: Vec<DropdownOption>,
    pub camera: Option<String>,
    /// Turns the camera on or off.
    pub on_camera_toggle: EventHandler<()>,
    /// Switches to the camera with this `value`.
    pub on_camera_select: EventHandler<String>,
    /// Looks for cameras again.
    pub on_camera_rescan: EventHandler<()>,
    /// Something wrong with the camera that is not the user's doing.
    pub camera_problem: Option<String>,
    /// What the call is doing.
    pub status: Signal<RemoteStatus>,
    /// Link numbers for the status line.
    pub metrics: Arc<Metrics>,
    /// Leaves the call, or cancels one not yet made.
    pub leave: Callback<()>,
    /// What we want to share; the call serves it.
    pub share: ShareWish,
}

#[component]
pub fn ControlBar(props: ControlBarProps) -> Element {
    let status = props.status.read().clone();
    let OwnShare(own_share) = use_context::<OwnShare>();
    let (_, share_problem) = own_share();
    let off = !props.camera_on;
    // Only once something is under way: idle, there is nothing to leave, and
    // the connect row at the top is where the next step is.
    let leaving = status.is_cancellable();
    let leave_label = if status.is_connected() { "Leave" } else { "Cancel" };
    let status_class = if status.is_error() {
        "truncate text-xs text-red-400"
    } else {
        "truncate text-xs text-neutral-400"
    };

    rsx! {
        div { class: "flex shrink-0 flex-row items-center gap-4 border-t border-neutral-800 pt-3",
            div { class: "flex shrink-0 flex-row items-center gap-3",
                MicControl { audio: props.audio.clone(), local: props.local.clone() }
                div { class: "flex flex-row items-center gap-0.5",
                    button {
                        class: if off {
                            "flex h-10 w-10 items-center justify-center rounded-full bg-red-600 text-white hover:bg-red-500"
                        } else {
                            "flex h-10 w-10 items-center justify-center rounded-full bg-neutral-800 text-white hover:bg-neutral-700"
                        },
                        onclick: move |_| props.on_camera_toggle.call(()),
                        CameraIcon { off }
                    }
                    DeviceMenu {
                        name: "camera",
                        sections: vec![MenuSection {
                            title: "Camera",
                            options: props.cameras.clone(),
                            selected: props.camera.clone(),
                            onchange: props.on_camera_select,
                        }],
                        onrescan: move |_| props.on_camera_rescan.call(()),
                    }
                }
                // Only on a call: loopback and idle have nobody to show it to.
                // And only where this build can capture the screen.
                ShareControl { share: props.share.clone(), shown: status.is_connected() && share_available() }
            }
            // The middle shrinks and truncates first, so the buttons never move.
            div { class: "flex min-w-0 flex-1 flex-row items-center gap-2",
                if let Some(problem) = props.camera_problem.clone() {
                    span { class: "shrink-0 text-xs text-red-400", "{problem} \u{b7}" }
                }
                // Truncated rather than kept whole like the camera's: the
                // permission sentence is long, and the rest is in the log.
                if let Some(problem) = share_problem {
                    span { class: "min-w-0 truncate text-xs text-red-400", "{problem} \u{b7}" }
                }
                span { class: status_class, "{status}" }
                StatsRow { metrics: props.metrics.clone() }
            }
            // Only on a call: before one there is no room to divide.
            SubroomMenu {
                audio: props.audio.clone(),
                local: props.local.clone(),
                me: props.me.clone(),
                shown: status.is_connected(),
            }
            if leaving {
                button {
                    class: "flex h-10 shrink-0 flex-row items-center gap-2 rounded-full bg-red-600 px-4 text-sm text-white hover:bg-red-500",
                    onclick: move |_| props.leave.call(()),
                    LeaveIcon {}
                    "{leave_label}"
                }
            }
        }
    }
}
