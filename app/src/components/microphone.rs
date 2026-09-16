//! Picking an input, muting it, and seeing that it is working.
//!
//! The camera row's shape, with one thing it does not have: a level. A camera
//! that is on shows a picture, so "is this working" answers itself. A
//! microphone that is on looks exactly like a microphone that is muted, broken,
//! or pointed at the wrong device -- so a meter is not decoration here, it is
//! the only feedback the control has.

use std::sync::Arc;
use std::time::Duration;

use dioxus_native::prelude::*;

use super::{Button, Dropdown, DropdownOption, Tone};
use crate::audio::{Controls, MicStatus, use_shared_microphones};
use crate::channel::use_channel;
use crate::settings::{self, Settings};

/// How often the meter is sampled.
///
/// About fifteen times a second: fast enough that speech moves the bar rather
/// than stepping it, slow enough that it is nowhere near the cost of the audio
/// it is describing.
const POLL: Duration = Duration::from_millis(66);

/// The level and status, sampled off the audio thread.
///
/// Polled rather than pushed. The level changes every buffer -- fifty times a
/// second -- and almost none of those changes are worth a repaint, so sampling
/// at the rate the bar is actually drawn at is both simpler and less work than
/// a message per buffer.
#[derive(Clone, Copy, PartialEq, Default)]
struct Meter {
    level: f32,
    status: MicStatus,
    peer_talking: bool,
}

#[derive(Clone, PartialEq, Props)]
pub struct MicrophoneProps {
    /// The microphone and what the peer is doing with theirs.
    pub audio: Arc<Controls>,
}

#[component]
pub fn MicrophoneRow(props: MicrophoneProps) -> Element {
    let devices = use_shared_microphones();
    let mut meter = use_signal(Meter::default);

    let mut muted = use_signal(|| false);
    let mut selected = use_signal::<Option<String>>(|| None);

    let saved = use_hook(|| Settings::load().microphone);

    let chosen_once = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(false)));
    use_effect({
        let chosen_once = std::rc::Rc::clone(&chosen_once);
        let saved = saved.clone();
        let audio = props.audio.clone();
        move || {
            if chosen_once.get() {
                return;
            }
            if !*devices.ready.read() {
                return;
            }
            let chosen = settings::initial_microphone(&devices.devices.read(), saved.as_deref());

            selected.set(chosen.clone());
            audio.set_device(chosen);
            chosen_once.set(true);
        }
    });

    let tx = use_channel(move |next: Meter| meter.set(next));
    use_hook({
        let audio = props.audio.clone();
        move || {
            let spawned = std::thread::Builder::new()
                .name("mic-meter".into())
                .spawn(move || {
                    loop {
                        let next = Meter {
                            level: audio.level(),
                            status: audio.status(),
                            peer_talking: audio.peer_talking(),
                        };
                        if tx.unbounded_send(next).is_err() {
                            return;
                        }
                        std::thread::sleep(POLL);
                    }
                });
            if let Err(error) = spawned {
                tracing::warn!("could not spawn the microphone meter thread: {error}");
            }
        }
    });

    let options: Vec<DropdownOption> = devices
        .devices
        .read()
        .iter()
        .map(|device| DropdownOption {
            value: device.id.clone(),

            label: if device.default {
                format!("{} (default)", device.name)
            } else {
                device.name.clone()
            },
        })
        .collect();
    let has_devices = !options.is_empty();
    let placeholder = if *devices.ready.read() {
        if has_devices {
            "System default"
        } else {
            "No microphones found"
        }
    } else {
        "Scanning..."
    };

    let reading = meter();
    let status_class = if reading.status.is_error() {
        "text-sm text-red-400"
    } else {
        "text-sm text-neutral-400"
    };
    let width = (reading.level.clamp(0.0, 1.0).sqrt() * 100.0).round() as u32;
    let bar_class = if muted() {
        "h-full rounded bg-neutral-600"
    } else {
        "h-full rounded bg-emerald-500"
    };

    rsx! {
        div { class: "flex flex-row items-end gap-3",
            Dropdown {
                label: "Microphone",
                options,
                selected: selected.read().clone(),
                placeholder: placeholder.to_string(),
                onchange: {
                    let audio = props.audio.clone();
                    move |value: String| {
                        let chosen = value.clone();
                        Settings::update(move |settings| settings.microphone = Some(chosen));
                        audio.set_device(Some(value.clone()));
                        selected.set(Some(value));
                    }
                },
            }
            Button {
                onclick: move |_| devices.refresh.call(()),
                "Rescan"
            }
            if has_devices {
                Button {
                    tone: Tone::Primary,
                    onclick: {
                        let audio = props.audio.clone();
                        move |_| {
                            let next = !*muted.peek();
                            muted.set(next);
                            audio.set_enabled(!next);
                        }
                    },
                    if muted() { "Unmute" } else { "Mute" }
                }
            }
        }
        div { class: "h-2 w-full max-w-4xl rounded bg-neutral-800",
            div { class: bar_class, style: "width: {width}%" }
        }
        p { class: status_class,
            if muted() {
                "Muted"
            } else {
                "{reading.status}"
            }
            if reading.peer_talking {
                " \u{00b7} they are talking"
            }
        }
    }
}
