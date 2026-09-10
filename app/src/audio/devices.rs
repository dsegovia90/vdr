//! Enumerating the microphones the machine can see.
//!
//! The camera list's shape, deliberately: `moq_audio`'s device id is cpal's
//! `host:device`, which is stable across restarts and unique even when two
//! inputs share a name -- the same property `cameras::DeviceId` has, and the
//! reason a remembered microphone can be found again the way a remembered
//! camera is.
//!
//! One difference in the plumbing. Camera enumeration is a blocking call on a
//! worker thread; this one is async, so it runs on the shared runtime instead.
//! Either way it is off the interface thread, for the same reason: enumeration
//! reaches platform APIs that can take their time.

use dioxus_native::prelude::*;
use moq_audio::capture::Device;

use crate::channel::use_channel;
use crate::net::runtime::runtime;

/// Handle returned by [`use_microphones_provider`].
#[derive(Copy, Clone, PartialEq)]
pub struct UseMicrophones {
    /// Inputs currently reported by the OS.
    pub devices: Signal<Vec<Device>>,
    /// Flips to `true` after the first scan, so the interface can tell "not
    /// scanned yet" apart from "scanned, found nothing".
    pub ready: Signal<bool>,
    /// Rescan on demand.
    pub refresh: Callback<()>,
}

/// Provides the machine's microphone list to the subtree. Call once, near the
/// root.
pub fn use_microphones_provider() -> UseMicrophones {
    let devices = use_microphones();
    use_context_provider(|| devices);
    devices
}

/// Reads the microphone list published by [`use_microphones_provider`].
///
/// Panics if no provider is mounted above the caller -- a wiring mistake rather
/// than a runtime condition, so it should fail loudly and immediately.
pub fn use_shared_microphones() -> UseMicrophones {
    use_context::<UseMicrophones>()
}

/// Keeps a signal populated with the connected microphones, rescanning on
/// demand.
///
/// Private for the same reason the camera one is: one list per machine, reached
/// through the provider. Calling this directly would get a second enumeration
/// that no rescan of the shared list ever updates.
fn use_microphones() -> UseMicrophones {
    let mut devices = use_signal(Vec::<Device>::new);
    let mut ready = use_signal(|| false);

    let tx = use_channel(move |list: Vec<Device>| {
        devices.set(list);
        ready.set(true);
    });

    let refresh = use_callback(move |()| {
        let tx = tx.clone();
        let Some(runtime) = runtime() else {
            tracing::warn!("no runtime, so microphones cannot be enumerated");
            return;
        };
        runtime.spawn(async move {
            match moq_audio::capture::devices().await {
                Ok(list) => {
                    // Ids as well as names, for the reason the camera log gives:
                    // the id is what gets persisted, and two inputs are allowed
                    // to share a name.
                    tracing::debug!(
                        "enumerated {} microphone(s): {}",
                        list.len(),
                        list.iter()
                            .map(|d| format!("{} ({})", d.name, d.id))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    let _ = tx.unbounded_send(list);
                }
                Err(error) => tracing::warn!("microphone enumeration failed: {error:#}"),
            }
        });
    });

    use_effect(move || refresh.call(()));

    UseMicrophones {
        devices,
        ready,
        refresh,
    }
}
