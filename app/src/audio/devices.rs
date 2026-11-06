//! Enumerating the microphones and speakers the machine can see.
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
//!
//! Microphones and speakers are one hook over two scans: `moq_audio`'s input
//! and output devices have the same shape, and each list is its own context
//! type, so the two never collide.

use std::future::Future;

use dioxus_native::prelude::*;

use crate::channel::use_channel;
use crate::net::runtime::runtime;

/// Handle returned by [`use_microphones_provider`] and
/// [`use_outputs_provider`].
pub struct UseDevices<D: 'static> {
    /// Devices currently reported by the OS.
    pub devices: Signal<Vec<D>>,
    /// Flips to `true` after the first scan, so the interface can tell "not
    /// scanned yet" apart from "scanned, found nothing".
    pub ready: Signal<bool>,
    /// Rescan on demand.
    pub refresh: Callback<()>,
}

// By hand: a derive would demand `D: Copy`, which a device is not and a signal
// handle does not need.
impl<D: 'static> Clone for UseDevices<D> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<D: 'static> Copy for UseDevices<D> {}
impl<D: 'static> PartialEq for UseDevices<D> {
    fn eq(&self, other: &Self) -> bool {
        self.devices == other.devices && self.ready == other.ready && self.refresh == other.refresh
    }
}

pub type UseMicrophones = UseDevices<moq_audio::capture::Device>;
pub type UseOutputs = UseDevices<moq_audio::playback::Device>;

/// Provides the machine's microphone list to the subtree. Call once, near the
/// root.
pub fn use_microphones_provider() -> UseMicrophones {
    let devices = use_devices("microphone", moq_audio::capture::devices, |d| {
        format!("{} ({})", d.name, d.id)
    });
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

/// Provides the machine's speaker / headphone list to the subtree. Call once,
/// near the root.
pub fn use_outputs_provider() -> UseOutputs {
    let devices = use_devices("output", moq_audio::playback::devices, |d| {
        format!("{} ({}, {})", d.name, d.id, d.host)
    });
    use_context_provider(|| devices);
    devices
}

/// Reads the output list published by [`use_outputs_provider`].
pub fn use_shared_outputs() -> UseOutputs {
    use_context::<UseOutputs>()
}

/// Keeps a signal populated with what `scan` finds, rescanning on demand.
///
/// Private for the same reason the camera one is: one list per machine, reached
/// through the provider. Calling this directly would get a second enumeration
/// that no rescan of the shared list ever updates.
fn use_devices<D, F, Fut>(what: &'static str, scan: F, describe: fn(&D) -> String) -> UseDevices<D>
where
    D: Send + 'static,
    F: Fn() -> Fut + Copy + 'static,
    Fut: Future<Output = Result<Vec<D>, moq_audio::Error>> + Send + 'static,
{
    let mut devices = use_signal(Vec::<D>::new);
    let mut ready = use_signal(|| false);

    let tx = use_channel(move |list: Vec<D>| {
        devices.set(list);
        ready.set(true);
    });

    let refresh = use_callback(move |()| {
        let tx = tx.clone();
        let Some(runtime) = runtime() else {
            tracing::warn!("no runtime, so {what}s cannot be enumerated");
            return;
        };
        let scanning = scan();
        runtime.spawn(async move {
            match scanning.await {
                Ok(list) => {
                    // Ids as well as names, for the reason the camera log gives:
                    // the id is what gets persisted, and two devices are allowed
                    // to share a name.
                    tracing::debug!(
                        "enumerated {} {what}(s): {}",
                        list.len(),
                        list.iter().map(describe).collect::<Vec<_>>().join(", ")
                    );
                    let _ = tx.unbounded_send(list);
                }
                Err(error) => tracing::warn!("{what} enumeration failed: {error:#}"),
            }
        });
    });

    use_effect(move || refresh.call(()));

    UseDevices {
        devices,
        ready,
        refresh,
    }
}
