use dioxus::{html::track::label, prelude::*};
use dioxus_cameras::{
    cameras::{self, CameraSource, Device, PixelFormat, Resolution, StreamConfig},
    use_camera_stream, use_devices, PreviewScript, StreamPreview, StreamStatus,
};

fn device_display_string(d: &Device) -> String {
    let (id, name) = (d.id.0.clone(), d.name.clone());

    format!("{name} - {id}")
}

#[component]
pub fn Camera() -> Element {
    let mut source = use_signal::<Option<CameraSource>>(|| None);
    let devices = use_devices();

    use_effect(move || {
        if source.peek().is_none() {
            if let Some(first) = devices.devices.read().first().cloned() {
                source.set(Some(CameraSource::Usb(first)))
            }
        }
    });

    let config = StreamConfig {
        resolution: Resolution {
            width: 1280,
            height: 720,
        },
        framerate: 30,
        pixel_format: PixelFormat::Bgra8,
    };
    let stream = use_camera_stream(0, source, config);
    let selected = source.read().as_ref().map(|s| match s {
        CameraSource::Usb(d) => device_display_string(d),
    });

    rsx! {
        match selected {
            Some(display_name) => rsx! {
                p { "{display_name}" }
            },
            None => rsx! { p { "No camera selected" } },
        }

        form {
            select {
                oninput: move |evt| {
                    let found = devices.devices
                        .read()
                        .iter()
                        .find(|d| d.id.0 == evt.value())
                        .map(|c| CameraSource::Usb(c.clone()));
                    source.set(found)
                },

                for d in devices.devices.read().iter() {
                    option {
                        label: device_display_string(d),
                        value: d.id.0.clone()
                    }
                }
            }
        }
        div {
            class: "w-full aspect-video -scale-x-100",
            if stream.active.read().clone() {
                StreamPreview { id: 0 }
            }
        }




        p { "{stream.status}" }
        button {
            onclick: move |_| stream.active.clone().toggle(),
            "Toggle preview"
        }
        // button {
        //     onclick: move |_| { let _ = stream.capture_frame.call(()); },
        //     "Take picture"
        // }
        PreviewScript {}
    }
}
