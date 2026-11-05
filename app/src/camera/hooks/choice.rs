//! Which camera, whether it is on, and the stream that results.
//!
//! The user's choices and the hardware's state, kept apart on purpose: the
//! chosen device and on/off are intent, and the stream status says what the
//! device is actually doing. They can disagree -- a chosen camera that has
//! been unplugged -- and the interface shows both.

use std::cell::Cell;
use std::rc::Rc;

use cameras::{CameraSource, DeviceId, PixelFormat, Resolution, StreamConfig};
use dioxus_native::prelude::*;

use super::{UseCameraStream, UseDevices, use_camera_stream, use_shared_devices};
use crate::args::Args;
use crate::camera::{FrameSlot, FrameTap};
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

/// The camera as the page sees it. `Copy`, so handlers can take it freely.
#[derive(Clone, Copy, PartialEq)]
pub struct CameraChoice {
    /// The cameras the OS reports, and how to rescan.
    pub devices: UseDevices,
    /// Never a position: enumeration order is not stable across launches,
    /// and a device disappearing shifts every later index down by one.
    pub selected: Signal<Option<DeviceId>>,
    /// User intent, kept out of `StreamStatus` so the two can disagree.
    pub enabled: Signal<bool>,
    /// What the device is actually doing.
    pub stream: UseCameraStream,
}

impl CameraChoice {
    /// Switches to `id`, and remembers it for the next launch.
    ///
    /// Saved here rather than in an effect watching `selected`: this is the
    /// one place a *person* changes the camera, and signals notify on
    /// redundant writes.
    pub fn choose(mut self, id: DeviceId) {
        let chosen = id.0.clone();
        Settings::update(move |settings| settings.camera = Some(chosen));
        self.selected.set(Some(id));
    }

    /// Turns the camera off, or back on.
    pub fn toggle(mut self) {
        let next = !*self.enabled.peek();
        self.enabled.set(next);
    }

    /// Whether the chosen camera has gone from the device list. Kept chosen
    /// rather than cleared, so replugging it reconnects.
    pub fn missing(&self) -> bool {
        self.selected
            .read()
            .as_ref()
            .is_some_and(|id| !self.devices.devices.read().iter().any(|device| &device.id == id))
    }

    /// Something wrong that is not the user's doing, in words. "Off" is not a
    /// problem: the user chose it, and the icon already says so.
    pub fn problem(&self) -> Option<String> {
        if !(self.enabled)() {
            None
        } else if self.missing() {
            Some("Camera disconnected".to_string())
        } else if self.stream.status.read().is_error() {
            Some(self.stream.status.read().to_string())
        } else {
            None
        }
    }
}

/// Chooses a camera, opens it, and pumps its frames into `slot` (the self
/// view) and `tap` (the encoder).
pub fn use_camera_choice(slot: FrameSlot, tap: FrameTap) -> CameraChoice {
    let devices = use_shared_devices();
    let mut selected = use_signal::<Option<DeviceId>>(|| None);
    let enabled = use_signal(|| true);
    let mut source = use_signal::<Option<CameraSource>>(|| None);
    let stream = use_camera_stream(slot, source, CONFIG, Some(tap));

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
        let next = if !enabled() {
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

    CameraChoice {
        devices,
        selected,
        enabled,
        stream,
    }
}
