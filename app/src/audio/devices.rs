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
    D: Listed,
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
            match scanning.await.map(listable) {
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

/// What the filter needs from a device, so one filter serves both lists.
trait Listed {
    fn name(&self) -> &str;
    fn id(&self) -> &str;
}

impl Listed for moq_audio::capture::Device {
    fn name(&self) -> &str {
        &self.name
    }
    fn id(&self) -> &str {
        &self.id
    }
}

impl Listed for moq_audio::playback::Device {
    fn name(&self) -> &str {
        &self.name
    }
    fn id(&self) -> &str {
        &self.id
    }
}

/// Drops devices nobody should pick from a menu, logging each at debug.
fn listable<D: Listed>(list: Vec<D>) -> Vec<D> {
    list.into_iter()
        .filter(|device| match skip_reason(device.name(), device.id()) {
            None => true,
            Some(reason) => {
                tracing::debug!("skipping {} ({}): {reason}", device.name(), device.id());
                false
            }
        })
        .collect()
}

/// The virtual audio drivers conferencing apps install for their own loopback
/// (screen-share audio, their own echo path), matched case-insensitively
/// against a device's name or id. Nobody picks one of these on purpose, and
/// one chosen by accident means a silent call.
///
/// Only those. Virtual devices a user installs and routes on purpose --
/// Krisp, BlackHole, Loopback, VB-Cable -- stay listed.
///
/// Both as observed on macOS. Matches stay this narrow on purpose: a broad
/// "webex" would also hide Cisco's Webex Desk, a real USB speakerphone.
const CONFERENCING_LOOPBACKS: &[(&str, &str)] = &[
    ("zoomaudiodevice", "Zoom's loopback driver"),
    ("zoom.us", "Zoom's loopback driver"),
    ("microsoft teams audio", "Teams' loopback driver"),
    ("msloopbackdriver", "Teams' loopback driver"),
];

/// Why a device is left out of the menu, or `None` to list it.
///
/// **ALSA lists every PCM it has a hint for**, and cpal's ALSA host (the only
/// Linux host in this build) passes them all through: `default`, then per
/// card `sysdefault`, `front`, `surround*`, `iec958`, `hdmi`, `dmix`,
/// `dsnoop`, the raw `hw` and `plughw`, and plugin PCMs such as `pipewire` and
/// `pulse`. Only two kinds are worth offering. `default` is the system route
/// -- PipeWire or PulseAudio on a modern desktop -- and `sysdefault:CARD=X`
/// is the one per-card entry that converts formats. The rest are raw or
/// plugin PCMs that either fail to open (a raw `hw:` did, offering no sample
/// format the engine can write) or duplicate those two. Other hosts --
/// CoreAudio, WASAPI, a future PipeWire host -- list real devices and pass
/// through.
///
/// If picking a card by its `sysdefault` entry still fails on PipeWire
/// machines, the proper fix is enabling `moq-audio`'s `pipewire` feature on
/// Linux and preferring that host, which needs libpipewire on the Linux build
/// machine. That is inferred, not tested.
fn skip_reason(name: &str, id: &str) -> Option<&'static str> {
    let (name, id_lower) = (name.to_lowercase(), id.to_lowercase());
    if let Some((_, reason)) = CONFERENCING_LOOPBACKS
        .iter()
        .find(|(needle, _)| name.contains(needle) || id_lower.contains(needle))
    {
        return Some(reason);
    }
    match id.strip_prefix("alsa:") {
        Some(pcm) if pcm != "default" && !pcm.starts_with("sysdefault:CARD=") => {
            Some("an ALSA PCM that is neither the system default nor a card's sysdefault")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::skip_reason;

    fn worth_listing(name: &str, id: &str) -> bool {
        skip_reason(name, id).is_none()
    }

    #[test]
    fn mac_conferencing_drivers_are_hidden() {
        assert!(worth_listing("MacBook Pro Microphone", "coreaudio:BuiltInMicrophoneDevice"));
        assert!(worth_listing("MacBook Pro Speakers", "coreaudio:BuiltInSpeakerDevice"));
        assert!(!worth_listing("Microsoft Teams Audio", "coreaudio:MSLoopbackDriverDevice_UID"));
        assert!(!worth_listing("ZoomAudioDevice", "coreaudio:zoom.us.zoomaudiodevice.001"));
    }

    #[test]
    fn user_chosen_virtual_devices_stay() {
        assert!(worth_listing("BlackHole 2ch", "coreaudio:BlackHole2ch_UID"));
        assert!(worth_listing("Krisp Microphone", "coreaudio:KrispMicrophone"));
        assert!(worth_listing("Cisco Webex Desk", "coreaudio:CiscoWebexDesk"));
    }

    #[test]
    fn matching_ignores_case() {
        assert!(!worth_listing("ZOOMAUDIODEVICE", "coreaudio:x"));
        assert!(!worth_listing("Some Device", "wasapi:MSLOOPBACKDRIVER"));
    }

    #[test]
    fn alsa_keeps_default_and_sysdefault_only() {
        assert!(worth_listing("Default ALSA Output", "alsa:default"));
        assert!(worth_listing("HDA Intel PCH", "alsa:sysdefault:CARD=PCH"));
        assert!(!worth_listing("HDA Intel PCH", "alsa:hw:CARD=1,DEV=0"));
        assert!(!worth_listing("HDA Intel PCH", "alsa:plughw:CARD=1,DEV=0"));
        assert!(!worth_listing("HDA Intel PCH", "alsa:front:CARD=PCH,DEV=0"));
        assert!(!worth_listing("PipeWire Sound Server", "alsa:pipewire"));
        assert!(!worth_listing("PulseAudio Sound Server", "alsa:pulse"));
        assert!(!worth_listing("HDMI 0", "alsa:hdmi:CARD=PCH,DEV=0"));
    }

    #[test]
    fn other_hosts_pass_through() {
        assert!(worth_listing("USB Audio", "coreaudio:AppleUSBAudioEngine:1234"));
        assert!(worth_listing("Speakers", "wasapi:{0.0.0.00000000}.{abc}"));
    }
}
