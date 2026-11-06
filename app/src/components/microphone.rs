//! Picking an input, muting it, and seeing that it is working -- as one small
//! control in the bottom bar: an icon toggle, a level along its bottom edge,
//! and a ▾ for the device list.
//!
//! The camera control's shape, with one thing it does not have: a level. A camera
//! that is on shows a picture, so "is this working" answers itself. A
//! microphone that is on looks exactly like a microphone that is muted, broken,
//! or pointed at the wrong device -- so a meter is not decoration here, it is
//! the only feedback the control has.

use std::sync::Arc;
use std::time::Duration;

use dioxus_native::prelude::*;

use super::{DeviceMenu, DropdownOption, MenuSection, MicIcon};
use crate::audio::{Controls, MicStatus, Talking, use_shared_microphones, use_shared_outputs};
use crate::channel::use_channel;
use crate::net::PeerStats;
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
    /// Whether the mic is on, which a peer's mute request can change from
    /// outside this control; the button follows it.
    enabled: bool,
}

#[derive(Clone, PartialEq, Props)]
pub struct MicrophoneProps {
    /// The microphone and what the peer is doing with theirs.
    pub audio: Arc<Controls>,
    /// Ourselves: muting is recorded here, for our tile and for the room.
    pub local: Arc<PeerStats>,
}

#[component]
pub fn MicControl(props: MicrophoneProps) -> Element {
    let devices = use_shared_microphones();
    let outputs = use_shared_outputs();
    let mut meter = use_signal(Meter::default);

    let mut muted = use_signal(|| false);
    let mut selected = use_signal::<Option<String>>(|| None);

    let mut output = use_signal::<Option<String>>(|| None);

    let saved = use_hook(|| Settings::load().microphone);
    let saved_output = use_hook(|| Settings::load().output);

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

    // The remembered output, once the outputs have been scanned: the
    // microphone's rule, applied once.
    let output_once = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(false)));
    use_effect({
        let audio = props.audio.clone();
        move || {
            if output_once.get() || !*outputs.ready.read() {
                return;
            }
            let chosen = settings::initial_output(&outputs.devices.read(), saved_output.as_deref());
            output.set(chosen.clone());
            audio.set_output(chosen);
            output_once.set(true);
        }
    });

    let tx = use_channel(move |next: Meter| {
        // A peer muting us turns the mic off behind the button's back; this
        // is how the button finds out, within one sample.
        if *muted.peek() == next.enabled {
            muted.set(!next.enabled);
        }
        meter.set(next);
    });
    use_hook({
        let audio = props.audio.clone();
        let local = props.local.clone();
        move || {
            let spawned = std::thread::Builder::new()
                .name("mic-meter".into())
                .spawn(move || {
                    // Our own tile's talking dot, by the same rule as everyone
                    // else's, from the level we send (zero while muted).
                    let mut talking = Talking::every(POLL);
                    loop {
                        let next = Meter {
                            level: audio.level(),
                            status: audio.status(),
                            enabled: audio.enabled(),
                        };
                        local.set_talking(talking.push(next.level));
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
        .map(|device| option(&device.id, &device.name, device.default))
        .collect();
    let output_options: Vec<DropdownOption> = outputs
        .devices
        .read()
        .iter()
        .map(|device| option(&device.id, &device.name, device.default))
        .collect();
    let has_devices = !options.is_empty();

    let reading = meter();
    let width = (reading.level.clamp(0.0, 1.0).sqrt() * 100.0).round() as u32;
    let off = muted();
    // A problem with the device reads differently from choosing to mute: red
    // text beside the button, because the icon alone looks the same either way.
    let problem = (!off && reading.status.is_error()).then(|| reading.status.to_string());

    rsx! {
        div { class: "flex flex-row items-center gap-0.5",
            button {
                class: if off {
                    "relative flex h-10 w-10 items-center justify-center overflow-hidden rounded-full bg-red-600 text-white hover:bg-red-500"
                } else {
                    "relative flex h-10 w-10 items-center justify-center overflow-hidden rounded-full bg-neutral-800 text-white hover:bg-neutral-700"
                },
                onclick: {
                    let audio = props.audio.clone();
                    let local = props.local.clone();
                    move |_| {
                        if !has_devices {
                            return;
                        }
                        let next = !*muted.peek();
                        muted.set(next);
                        audio.set_enabled(!next);
                        local.set_mic(!next);
                        // Only our own unmute clears who muted us.
                        if !next {
                            local.set_muted_by(None);
                        }
                    }
                },
                MicIcon { off }
                // The level, as a thin line along the bottom: the only sign the
                // microphone is actually hearing anything.
                if !off {
                    div { class: "absolute bottom-0 left-0 h-1 bg-emerald-500", style: "width: {width}%" }
                }
            }
            DeviceMenu {
                name: "microphone",
                sections: vec![
                    MenuSection {
                        title: "Microphone",
                        options,
                        selected: selected.read().clone(),
                        onchange: EventHandler::new({
                            let audio = props.audio.clone();
                            move |value: String| {
                                // Saved here rather than in an effect: this is the one
                                // place a *person* changes the input.
                                let chosen = value.clone();
                                Settings::update(move |settings| settings.microphone = Some(chosen));
                                audio.set_device(Some(value.clone()));
                                selected.set(Some(value));
                            }
                        }),
                    },
                    MenuSection {
                        title: "Speakers",
                        options: output_options,
                        selected: output.read().clone(),
                        onchange: EventHandler::new({
                            let audio = props.audio.clone();
                            move |value: String| {
                                let chosen = value.clone();
                                Settings::update(move |settings| settings.output = Some(chosen));
                                audio.set_output(Some(value.clone()));
                                output.set(Some(value));
                            }
                        }),
                    },
                ],
                onrescan: move |_| {
                    devices.refresh.call(());
                    outputs.refresh.call(());
                },
            }
            if let Some(problem) = problem {
                span { class: "ml-1 text-xs text-red-400 whitespace-nowrap", "{problem}" }
            }
        }
    }
}

/// A device as the menu lists it, marking the system default.
fn option(id: &str, name: &str, default: bool) -> DropdownOption {
    DropdownOption {
        value: id.to_string(),
        label: if default {
            format!("{name} (default)")
        } else {
            name.to_string()
        },
    }
}
