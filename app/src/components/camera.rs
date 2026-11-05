use std::cell::Cell;
use std::rc::Rc;

use cameras::{CameraSource, DeviceId, PixelFormat, Resolution, StreamConfig};
use dioxus_native::prelude::*;

use super::{ConnectRow, ControlBar, DropdownOption, ParticipantTile, layout, use_open_menu_provider};
use crate::args::Args;
use crate::camera::{FrameSlot, Mirror, use_camera_stream, use_shared_devices};
use crate::net::use_remote;
use crate::settings::{self, Settings};

/// What we ask the camera for, before `camera::format::negotiate` narrows it to
/// a mode the device actually reports.
///
/// 1280x720 comfortably covers a canvas that has never measured wider than
/// ~900px, and is a mode every webcam offers. The canvas size is known only in
/// the paint source, and reacting to it needs hysteresis -- see task 15.
///
/// BGRA costs no CPU conversion at all: the bytes upload verbatim into a
/// `Bgra8Unorm` texture and the GPU swaps channels when the shader samples it.
/// On macOS it is also the only thing on offer -- the backend hardcodes its
/// output to 32BGRA whatever the device's native format.
///
/// Not a signal, so changing this does not reopen a running camera.
const CONFIG: StreamConfig = StreamConfig {
    resolution: Resolution {
        width: 1280,
        height: 720,
    },
    framerate: 30,
    pixel_format: PixelFormat::Bgra8,
};

#[component]
pub fn Camera() -> Element {
    // One open menu at a time across the bar; see `device_menu::OpenMenu`.
    use_open_menu_provider();
    let devices = use_shared_devices();
    // Never a position: enumeration order is not stable across launches, and a
    // device disappearing shifts every later index down by one.
    let mut selected = use_signal::<Option<DeviceId>>(|| None);
    // User intent, kept out of `StreamStatus` so the two can disagree.
    let mut camera_enabled = use_signal(|| true);
    let mut source = use_signal::<Option<CameraSource>>(|| None);

    // Shared between the capture thread that fills it and the paint source that
    // drains it on the render thread.
    let slot = use_hook(FrameSlot::default);
    // A peer's video, or our own round-tripped through the codec when there is
    // no peer to call. Either way it arrives decoded, in its own slot.
    let remote = use_remote();
    let stream = use_camera_stream(slot.clone(), source, CONFIG, Some(remote.tap.clone()));

    // What the user chose last launch. Read once: the file is ours, nothing
    // else writes it while we run.
    let saved = use_hook(Settings::load);

    // Chooses once, so a later rescan cannot relocate the user's choice. Not a
    // signal: nothing renders off it.
    let chosen_once = use_hook(|| Rc::new(Cell::new(false)));
    use_effect({
        let chosen_once = Rc::clone(&chosen_once);
        let persisted = saved.camera.clone().map(DeviceId);
        move || {
            if chosen_once.get() {
                return;
            }
            let devices = devices.devices.read();
            // `--camera`, then the remembered device, then whatever the OS
            // listed first. `settings::choose` documents the order and why.
            let chosen = settings::initial_device(
                &devices,
                persisted.as_ref(),
                Args::get().camera.as_deref(),
            );
            if let Some(chosen) = chosen {
                selected.set(Some(chosen));
                chosen_once.set(true);
            }
        }
    });

    // `None` either way stops the stream: it never falls through to whatever
    // other camera happens to be present.
    use_effect(move || {
        let next = if !camera_enabled() {
            None
        } else {
            let wanted = selected.read().clone();
            wanted
                .and_then(|id| devices.devices.read().iter().find(|d| d.id == id).cloned())
                .map(CameraSource::Usb)
        };
        // Signals notify on every write, even a redundant one, so writing
        // unguarded here would reopen the camera on each rerender.
        if *source.peek() != next {
            source.set(next);
        }
    });

    let options: Vec<DropdownOption> = devices
        .devices
        .read()
        .iter()
        .map(|device| DropdownOption {
            value: device.id.0.clone(),
            label: device.name.clone(),
        })
        .collect();
    // Kept rather than cleared, so replugging the device reconnects it.
    let missing = selected
        .read()
        .as_ref()
        .is_some_and(|id| !devices.devices.read().iter().any(|device| &device.id == id));
    // "Camera off" is the user's own doing and the icon already says it; a
    // vanished device or a dead capture thread is not, and gets words.
    let camera_problem = if !camera_enabled() {
        None
    } else if missing {
        Some("Camera disconnected".to_string())
    } else if stream.status.read().is_error() {
        Some(stream.status.read().to_string())
    } else {
        None
    };

    // Read into a Vec first: holding the signal's guard across the nodes below
    // would keep it borrowed while they render.
    let participants = remote.participants.read().clone();
    let tiles = 1 + participants.len();
    let tile = layout::tile_class(tiles);

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
                stats: None,
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
            camera_on: camera_enabled(),
            cameras: options,
            camera: selected.read().as_ref().map(|id| id.0.clone()),
            on_camera_toggle: move |_| {
                let next = !*camera_enabled.peek();
                camera_enabled.set(next);
            },
            on_camera_select: move |value: String| {
                // Saved here rather than in an effect watching `selected`:
                // this is the one place a *person* changes the camera, and
                // signals notify on redundant writes.
                let chosen = value.clone();
                Settings::update(move |settings| settings.camera = Some(chosen));
                selected.set(Some(DeviceId(value)));
            },
            on_camera_rescan: move |_| devices.refresh.call(()),
            camera_problem,
            status: remote.status,
            metrics: remote.metrics.clone(),
            leave: remote.cancel,
        }
    }
}
