//! User preferences that outlive a launch.
//!
//! One small JSON file, written when the user changes something and read once at
//! startup. `args.rs` is the sibling of this: that is what the command line said
//! *this run*, this is what the user chose *last time*.
//!
//! **Nothing here panics.** A missing file is the first launch, a corrupt one is
//! a bad shutdown, and an unwritable directory is someone else's disk problem —
//! none of them is a reason a video call should fail to start. Every failure is
//! a log line and a default, which is the posture [`crate::args`] already takes
//! for a mistyped key.
//!
//! The location comes from `directories`' `data_local_dir()`
//! (`~/Library/Application Support/vdr/settings.json` on macOS). That is the
//! same directory `dioxus-sdk-storage`'s own desktop backend uses, so it is the
//! conventional spot rather than one we invented — see ticket 17 for why we do
//! not use that crate itself.

use cameras::{Device, DeviceId};
use serde::{Deserialize, Serialize};

/// Subdirectory under the platform's local-data directory.
const APP_DIR: &str = "vdr";

/// The settings file itself.
const FILE: &str = "settings.json";

/// Where our iroh secret key lives, next to the settings.
const IDENTITY: &str = "identity.key";

/// The file our endpoint's secret key is kept in, creating its directory if
/// that is missing.
///
/// **We never read or write it** — the path is handed to `moq-native`, which
/// generates a key into it on first use and reads it back afterwards, so there
/// is no key handling of ours to get wrong. The directory is ours to create,
/// because that write does not.
///
/// A permanent key means a permanent address: the string someone wrote down
/// last week still dials. Deleting this file is how you stop being that person,
/// and `--ephemeral` is how you run without one at all.
pub fn identity_path() -> Option<std::path::PathBuf> {
    let dirs = directories::BaseDirs::new()?;
    let dir = dirs.data_local_dir().join(APP_DIR);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        tracing::warn!("could not create {}: {error}", dir.display());
        return None;
    }
    Some(dir.join(IDENTITY))
}

/// The log file, creating its directory if that is missing.
///
/// Next to the identity, in the app's own data folder, so someone who was on a
/// call can find it and send it: a double-clicked app has no terminal to read
/// from. `None` where the platform has no such folder, and then the log goes to
/// stderr only.
pub fn log_path() -> Option<std::path::PathBuf> {
    let dirs = directories::BaseDirs::new()?;
    let dir = dirs.data_local_dir().join(APP_DIR);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("vdr.log"))
}

/// Everything the app remembers between launches.
///
/// `#[serde(default)]` on the struct rather than `deny_unknown_fields`: a file
/// written by a newer build must not stop an older one from starting, and a
/// field added later must not invalidate every existing file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The [`DeviceId`] the user last selected, as its inner string.
    ///
    /// Stored even while that camera is absent — see [`choose`]. Unplugging a
    /// webcam should not permanently forget it.
    pub camera: Option<String>,

    /// The microphone the user last selected, as `moq_audio`'s device id.
    ///
    /// `None` means the system default, which is a real choice here rather than
    /// a missing one: an audio stack that follows whatever the OS considers the
    /// default is what most calling apps do, and what a user who has never
    /// opened the picker expects. Kept while absent for the same reason the
    /// camera is.
    pub microphone: Option<String>,
}

impl Settings {
    /// Reads the settings file, or returns defaults.
    ///
    /// Never fails: an absent file is the ordinary first-launch case and is not
    /// even logged at warning level.
    pub fn load() -> Self {
        let Some(path) = path() else {
            tracing::debug!("no local-data directory on this platform; using default settings");
            return Self::default();
        };
        Self::load_from(&path)
    }

    /// Reads from an explicit path. See [`write_to`](Self::write_to).
    fn load_from(path: &std::path::Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!("no settings file yet at {}", path.display());
                return Self::default();
            }
            Err(error) => {
                tracing::warn!("could not read {}: {error}", path.display());
                return Self::default();
            }
        };
        match serde_json::from_str(&text) {
            Ok(settings) => settings,
            Err(error) => {
                // Deliberately not deleted or rewritten: leaving it lets a
                // person look at what went wrong.
                tracing::warn!("{} is not valid settings ({error}); ignoring it", path.display());
                Self::default()
            }
        }
    }

    /// Changes one preference and writes the file, preserving the rest.
    ///
    /// **Read, modify, write — not write.** With a single field it was harmless
    /// to build a fresh `Settings` from the one thing being changed and save
    /// that. With two it is a silent erase: choosing a camera would forget the
    /// microphone, and the damage would only show up a launch later. Loading
    /// first costs a file read on a worker thread and removes the whole class.
    ///
    /// Off the UI thread for the same reason enumeration is: the write is tiny,
    /// but a network-mounted or failing home directory is real and the render
    /// thread is not where that should be discovered. Nothing waits on the
    /// result — a preference that fails to save is worth a log line and nothing
    /// more.
    pub fn update(change: impl FnOnce(&mut Self) + Send + 'static) {
        let spawned = std::thread::Builder::new()
            .name("settings-save".into())
            .spawn(move || {
                let mut settings = Self::load();
                change(&mut settings);
                settings.write();
            });
        if let Err(error) = spawned {
            tracing::warn!("could not spawn the settings thread: {error}");
        }
    }

    /// The blocking half of [`update`](Self::update).
    fn write(&self) {
        let Some(path) = path() else {
            tracing::warn!("no local-data directory on this platform; not saving settings");
            return;
        };
        self.write_to(&path);
    }

    /// Writes to an explicit path.
    ///
    /// Split from [`write`](Self::write) so a test can exercise the real write
    /// without touching the user's actual settings file — the save path is
    /// otherwise only reachable by clicking the dropdown.
    fn write_to(&self, path: &std::path::Path) {
        let Some(dir) = path.parent() else {
            tracing::warn!("{} has no parent directory; not saving", path.display());
            return;
        };
        if let Err(error) = std::fs::create_dir_all(dir) {
            tracing::warn!("could not create {}: {error}", dir.display());
            return;
        }
        let json = match serde_json::to_string_pretty(self) {
            Ok(json) => json,
            Err(error) => {
                tracing::warn!("could not serialise settings: {error}");
                return;
            }
        };
        match std::fs::write(path, json) {
            Ok(()) => tracing::debug!("saved settings to {}", path.display()),
            Err(error) => tracing::warn!("could not write {}: {error}", path.display()),
        }
    }
}

/// Where the settings file lives, if this platform has such a place.
///
/// `None` rather than a panic: `BaseDirs::new` fails when the environment has no
/// home directory at all, which is unusual but not our business to crash over.
fn path() -> Option<std::path::PathBuf> {
    let dirs = directories::BaseDirs::new()?;
    Some(dirs.data_local_dir().join(APP_DIR).join(FILE))
}

/// Which camera to open on startup.
///
/// Adapter over [`choose`], which holds the actual rule. Split because
/// `cameras::Device` is `#[non_exhaustive]` and therefore cannot be constructed
/// in a unit test — the rule would otherwise be untestable.
pub fn initial_device(
    devices: &[Device],
    persisted: Option<&DeviceId>,
    cli: Option<&str>,
) -> Option<DeviceId> {
    let pairs: Vec<(&DeviceId, &str)> = devices
        .iter()
        .map(|device| (&device.id, device.name.as_str()))
        .collect();
    choose(&pairs, persisted, cli)
}

/// The startup-selection rule, over plain data.
///
/// In order: `--camera` if it matches something, then the remembered device if
/// it is still connected, then whatever the OS listed first. The last is
/// arbitrary — enumeration order is not stable across launches, which is the
/// whole reason this ticket exists — but it is only reached when there is
/// nothing better to go on.
fn choose(
    devices: &[(&DeviceId, &str)],
    persisted: Option<&DeviceId>,
    cli: Option<&str>,
) -> Option<DeviceId> {
    // `--camera` is a per-run override for pointing two instances on one machine
    // at different devices. It deliberately does not persist: otherwise
    // `--camera c922` would quietly become the user's permanent default.
    if let Some(wanted) = cli {
        if let Some((id, _)) = devices
            .iter()
            .find(|(_, name)| name.to_lowercase().contains(wanted))
        {
            return Some((*id).clone());
        }
    }
    // Absent rather than forgotten: if the remembered camera is unplugged we
    // fall through to something usable, but the stored value is left alone so
    // replugging returns to it.
    if let Some(wanted) = persisted {
        if let Some((id, _)) = devices.iter().find(|(id, _)| *id == wanted) {
            return Some((*id).clone());
        }
    }
    devices.first().map(|(id, _)| (*id).clone())
}

/// Which microphone to publish on startup.
///
/// Adapter over [`choose_microphone`], split for the reason
/// [`initial_device`] is.
pub fn initial_microphone(
    devices: &[moq_audio::capture::Device],
    persisted: Option<&str>,
) -> Option<String> {
    let ids: Vec<&str> = devices.iter().map(|device| device.id.as_str()).collect();
    choose_microphone(&ids, persisted)
}

/// The microphone-selection rule, over plain data.
///
/// Deliberately *not* the camera's rule. That one falls back to the first
/// device the OS listed, because a camera has no notion of a default and a
/// preview showing nothing is useless. Audio does have one, and it is a better
/// answer than an arbitrary pick: `None` here means "whatever the system
/// considers the default input", which is what the user's other apps use and
/// what follows them when they plug in a headset mid-call.
///
/// So the only rule is: honour the remembered input if it is still connected,
/// and otherwise fall back to the system default rather than to a guess.
fn choose_microphone(devices: &[&str], persisted: Option<&str>) -> Option<String> {
    let wanted = persisted?;
    devices
        .iter()
        .find(|id| **id == wanted)
        .map(|id| (*id).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> DeviceId {
        DeviceId(value.to_string())
    }

    /// Two cameras, in the order the OS happened to report them this run.
    fn devices() -> Vec<(DeviceId, String)> {
        vec![
            (id("facetime-uid"), "Apple Inc FaceTime HD Camera".into()),
            (id("c922-uid"), "Unknown C922 Pro Stream Webcam".into()),
        ]
    }

    fn pairs(devices: &[(DeviceId, String)]) -> Vec<(&DeviceId, &str)> {
        devices.iter().map(|(i, n)| (i, n.as_str())).collect()
    }

    #[test]
    fn with_nothing_remembered_it_takes_the_first_device() {
        let devices = devices();
        assert_eq!(
            choose(&pairs(&devices), None, None),
            Some(id("facetime-uid"))
        );
    }

    #[test]
    fn a_remembered_camera_wins_over_enumeration_order() {
        // The point of the whole ticket: the C922 is second in this run's
        // ordering and must still be the one opened.
        let devices = devices();
        assert_eq!(
            choose(&pairs(&devices), Some(&id("c922-uid")), None),
            Some(id("c922-uid"))
        );
    }

    #[test]
    fn a_remembered_camera_that_is_gone_falls_back_to_the_first() {
        let devices = devices();
        assert_eq!(
            choose(&pairs(&devices), Some(&id("unplugged-uid")), None),
            Some(id("facetime-uid"))
        );
    }

    #[test]
    fn the_cli_override_beats_a_remembered_camera() {
        let devices = devices();
        assert_eq!(
            choose(&pairs(&devices), Some(&id("facetime-uid")), Some("c922")),
            Some(id("c922-uid")),
            "--camera is what this run asked for; the file is what last run chose"
        );
    }

    #[test]
    fn a_cli_override_that_matches_nothing_falls_through_to_the_remembered_one() {
        let devices = devices();
        assert_eq!(
            choose(&pairs(&devices), Some(&id("c922-uid")), Some("nosuchcamera")),
            Some(id("c922-uid")),
            "a typo in --camera should not also discard the saved choice"
        );
    }

    #[test]
    fn matching_is_case_insensitive_and_by_substring() {
        let devices = devices();
        assert_eq!(
            choose(&pairs(&devices), None, Some("facetime")),
            Some(id("facetime-uid"))
        );
    }

    #[test]
    fn no_devices_means_no_choice() {
        assert_eq!(choose(&[], Some(&id("c922-uid")), Some("c922")), None);
    }

    #[test]
    fn settings_round_trip_through_json() {
        let settings = Settings {
            camera: Some("c922-uid".into()),
            microphone: Some("coreaudio:yeti".into()),
        };
        let json = serde_json::to_string(&settings).expect("serialises");
        let back: Settings = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(settings, back);
    }

    #[test]
    fn a_write_can_be_read_back_and_a_missing_file_is_the_default() {
        // The save path is otherwise only reachable through the dropdown, so
        // this is the only automated cover it gets.
        let dir = std::env::temp_dir().join(format!("vdr-settings-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            Settings::load_from(&path),
            Settings::default(),
            "a missing file is the first launch, not an error"
        );

        let settings = Settings {
            camera: Some("c922-uid".into()),
            microphone: Some("coreaudio:yeti".into()),
        };
        settings.write_to(&path);
        assert_eq!(Settings::load_from(&path), settings, "what was written comes back");

        // The parent directory did not exist beforehand: writing must create it.
        assert!(path.exists(), "write_to creates its directory");

        std::fs::write(&path, "{ not json").expect("test fixture writable");
        assert_eq!(
            Settings::load_from(&path),
            Settings::default(),
            "a corrupt file falls back rather than failing"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_object_is_valid_and_a_missing_field_defaults() {
        // What an older build's file looks like once a field is added.
        let back: Settings = serde_json::from_str("{}").expect("an empty object is valid");
        assert_eq!(back, Settings::default());
    }

    #[test]
    fn a_file_written_before_microphones_existed_still_loads() {
        // The actual shape of an older build's file, not a synthetic one.
        let back: Settings =
            serde_json::from_str(r#"{"camera":"abc"}"#).expect("an older file is valid");
        assert_eq!(back.camera.as_deref(), Some("abc"));
        assert_eq!(back.microphone, None, "a missing microphone is the default");
    }

    #[test]
    fn a_remembered_microphone_is_honoured_while_connected() {
        assert_eq!(
            choose_microphone(&["coreaudio:one", "coreaudio:two"], Some("coreaudio:two")),
            Some("coreaudio:two".to_string())
        );
    }

    #[test]
    fn an_absent_microphone_falls_back_to_the_system_default() {
        // Not to the first device: `None` means "let the OS decide", which is a
        // better answer than picking someone's webcam microphone for them.
        assert_eq!(
            choose_microphone(&["coreaudio:one"], Some("coreaudio:gone")),
            None
        );
    }

    #[test]
    fn with_nothing_remembered_the_system_default_is_used() {
        assert_eq!(choose_microphone(&["coreaudio:one"], None), None);
    }
}
