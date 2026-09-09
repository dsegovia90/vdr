use std::cell::Cell;
use std::rc::Rc;

use cameras::{CameraSource, DeviceId, PixelFormat, Resolution, StreamConfig};
use dioxus_native::prelude::*;

use super::{Dropdown, DropdownOption, ParticipantVideo};
use crate::args::Args;
use crate::camera::{use_camera_stream, use_shared_devices, FrameSlot, Mirror};
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
    let placeholder = if missing {
        "Camera disconnected"
    } else if *devices.ready.read() {
        "No cameras found"
    } else {
        "Scanning..."
    };
    let can_stream = !options.is_empty();
    // "Camera off" is the user's own doing; a vanished device or a dead
    // capture thread is not.
    let status_class = if !camera_enabled() {
        "text-sm text-neutral-400"
    } else if missing || stream.status.read().is_error() {
        "text-sm text-red-400"
    } else {
        "text-sm text-neutral-400"
    };

    rsx! {
        // Blitz sorts z-index among siblings only -- it has no stacking
        // contexts -- so the dropdown's own z-20 never competes with the
        // canvas, which lives under the next sibling. The row has to win here.
        div { class: "relative z-20 flex flex-row items-end gap-3",
            Dropdown {
                label: "Camera",
                options,
                selected: selected.read().as_ref().map(|id| id.0.clone()),
                placeholder: placeholder.to_string(),
                onchange: move |value: String| {
                    // Saved here rather than in an effect watching `selected`:
                    // this is the one place a *person* changes the camera, and
                    // signals notify on redundant writes.
                    Settings { camera: Some(value.clone()) }.save();
                    selected.set(Some(DeviceId(value)));
                },
            }
            button {
                class: "rounded-md bg-neutral-800 px-4 py-2 text-sm text-neutral-100 hover:bg-neutral-700",
                onclick: move |_| devices.refresh.call(()),
                "Rescan"
            }
            if can_stream {
                button {
                    class: "rounded-md bg-blue-600 px-4 py-2 text-sm text-white hover:bg-blue-500",
                    onclick: move |_| {
                        let next = !*camera_enabled.peek();
                        camera_enabled.set(next);
                    },
                    if camera_enabled() { "Turn camera off" } else { "Turn camera on" }
                }
            }
        }
        ParticipantVideo {
            label: "camera",
            slot,
            // Mirrored, because that is what people expect of their own
            // preview. Remote participants get `Mirror::None`.
            mirror: Mirror::SelfView,
            class: "w-full max-w-4xl -z-1",
        }
        ParticipantVideo {
            label: "remote",
            slot: remote.slot,
            // Not a self-view: these frames took the path a peer's would, and
            // mirroring them would be the bug task 9 removed.
            mirror: Mirror::None,
            class: "w-full max-w-4xl -z-1",
        }
        p {
            class: if remote.status.read().is_error() { "text-sm text-red-400" } else { "text-sm text-neutral-400" },
            "{remote.status}"
        }
        p { class: status_class,
            if !camera_enabled() {
                "Camera off"
            } else if missing {
                "Camera disconnected"
            } else {
                "{stream.status}"
            }
        }
    }
}
