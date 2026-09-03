//! Enumerating the cameras the machine can see.

use cameras::Device;
use dioxus::prelude::*;

use super::channel::use_channel;

/// Handle returned by [`use_devices_provider`].
#[derive(Copy, Clone, PartialEq)]
pub struct UseDevices {
    /// Cameras currently reported by the OS.
    pub devices: Signal<Vec<Device>>,
    /// Flips to `true` after the first scan, so the UI can tell "not scanned
    /// yet" apart from "scanned, found nothing".
    pub ready: Signal<bool>,
    /// Rescan on demand.
    pub refresh: Callback<()>,
}

/// Provides the machine's camera list to the subtree. Call once, near the root.
///
/// Enumeration is a property of the machine, not of a tile.
pub fn use_devices_provider() -> UseDevices {
    let devices = use_devices();
    use_context_provider(|| devices);
    devices
}

/// Reads the camera list published by [`use_devices_provider`].
///
/// Panics if no provider is mounted above the caller -- that is a wiring
/// mistake, not a runtime condition, so it should fail loudly and immediately.
pub fn use_shared_devices() -> UseDevices {
    use_context::<UseDevices>()
}

/// Keeps a signal populated with the connected cameras, rescanning on demand.
///
/// Deliberately private: one list per machine, reached through
/// [`use_devices_provider`]. A component calling this directly would get a
/// second enumeration that no rescan of the shared list ever updates.
fn use_devices() -> UseDevices {
    let mut devices = use_signal(Vec::<Device>::new);
    let mut ready = use_signal(|| false);

    let tx = use_channel(move |list: Vec<Device>| {
        devices.set(list);
        ready.set(true);
    });

    let refresh = use_callback(move |()| {
        let tx = tx.clone();
        // Enumeration hits platform APIs and can block for hundreds of
        // milliseconds, so it runs on a worker thread and reports back.
        let spawned = std::thread::Builder::new()
            .name("camera-enumerate".into())
            .spawn(move || match cameras::devices() {
                Ok(list) => {
                    tracing::debug!("enumerated {} camera(s)", list.len());
                    let _ = tx.unbounded_send(list);
                }
                Err(error) => tracing::warn!("device enumeration failed: {error}"),
            });
        if let Err(error) = spawned {
            tracing::warn!("could not spawn enumeration thread: {error}");
        }
    });

    use_effect(move || refresh.call(()));

    UseDevices {
        devices,
        ready,
        refresh,
    }
}
