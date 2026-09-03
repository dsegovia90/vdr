use cameras::{CameraSource, PixelFormat, Resolution, StreamConfig};
use dioxus::native::use_wgpu;
use dioxus::prelude::*;

use super::Dropdown;
use crate::camera::{use_camera_stream, use_devices, CameraPaintSource, FrameSlot};

/// Requested capture format. BGRA keeps the per-frame CPU conversion down to a
/// channel shuffle; other formats work but cost more to unpack.
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
    let devices = use_devices();
    let mut selected = use_signal(|| 0usize);
    let mut source = use_signal::<Option<CameraSource>>(|| None);

    // Shared between the capture thread that fills it and the paint source that
    // drains it on the render thread.
    let slot = use_hook(FrameSlot::default);
    let paint_source_id = use_wgpu({
        let slot = slot.clone();
        move || CameraPaintSource::new("camera", slot)
    });

    let stream = use_camera_stream(slot, source, CONFIG);

    // Follows the dropdown, and picks up the first camera once a scan lands.
    use_effect(move || {
        let next = devices
            .devices
            .read()
            .get(selected())
            .cloned()
            .map(CameraSource::Usb);
        // Signals notify on every write, even a redundant one, so writing
        // unguarded here would reopen the camera on each rerender.
        if *source.peek() != next {
            source.set(next);
        }
    });

    let options: Vec<String> = devices
        .devices
        .read()
        .iter()
        .map(|device| device.name.clone())
        .collect();
    let placeholder = if *devices.ready.read() {
        "No cameras found"
    } else {
        "Scanning..."
    };
    let can_stream = !options.is_empty();

    rsx! {
        // Blitz sorts z-index among siblings only -- it has no stacking
        // contexts -- so the dropdown's own z-20 never competes with the
        // canvas, which lives under the next sibling. The row has to win here.
        div { class: "relative z-20 flex flex-row items-end gap-3",
            Dropdown {
                label: "Camera",
                options,
                selected: selected(),
                placeholder: placeholder.to_string(),
                onchange: move |index| selected.set(index),
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
                        let mut active = stream.active;
                        let next = !*active.peek();
                        active.set(next);
                    },
                    if *stream.active.read() { "Pause" } else { "Resume" }
                }
            }
        }
        // A canvas has no intrinsic size and blitz reads the parent height as
        // indefinite, so a percentage height would collapse it to 0px and it
        // would never be painted. Flex stretch sizes it without percentages.
        div { class: "flex w-full max-w-4xl aspect-video rounded-lg bg-black -z-1",
            canvas { class: "flex-1 self-stretch", "src": paint_source_id }
        }
        p { class: "text-sm text-neutral-400", "{stream.status}" }
    }
}
