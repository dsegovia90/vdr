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
//! conventional spot rather than one we invented. That crate itself is not used:
//! it needs the `dioxus` umbrella and panics on filesystem errors.

use std::collections::BTreeMap;

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
/// **We never read or write it** — the path is handed to `moq-tokio`, which
/// generates a key into it on first use and reads it back afterwards, so there
/// is no key handling of ours to get wrong. The directory is ours to create,
/// because that write does not.
///
/// A permanent key means a permanent address: the string someone wrote down
/// last week still dials. Deleting this file is how you stop being that person,
/// and `--ephemeral` is how you run without one at all.
pub fn identity_path() -> Option<std::path::PathBuf> {
    let dir = data_dir()?;
    if let Err(error) = std::fs::create_dir_all(&dir) {
        tracing::warn!("could not create {}: {error}", dir.display());
        return None;
    }
    let path = dir.join(IDENTITY);
    private(&path);
    Some(path)
}

/// Makes an existing key file readable by its owner only.
///
/// **moq-tokio refuses a key others can read** ("use chmod 600"), and every key
/// written before it started checking was created 644. Tightened here rather
/// than left to each tester to fix by hand; a missing file is the first launch
/// and is created private by moq-tokio itself.
pub(crate) fn private(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(metadata) = std::fs::metadata(path) else { return };
        if metadata.permissions().mode() & 0o077 != 0 {
            match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
                Ok(()) => tracing::info!("made {} private to this user", path.display()),
                Err(error) => tracing::warn!("could not make {} private: {error}", path.display()),
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// The log file, creating its directory if that is missing.
///
/// Next to the identity, in the app's own data folder, so someone who was on a
/// call can find it and send it: a double-clicked app has no terminal to read
/// from. `None` where the platform has no such folder, and then the log goes to
/// stderr only.
pub fn log_path() -> Option<std::path::PathBuf> {
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    log_file()
}

/// Where [`log_path`] puts the log, without creating anything: for showing
/// the path, not writing to it.
pub fn log_file() -> Option<std::path::PathBuf> {
    Some(data_dir()?.join("vdr.log"))
}

/// The app's data folder: identity, settings and logs.
///
/// An `--ephemeral` run gets its own folder under the system temp directory
/// (`$TMPDIR` on macOS, `/tmp` on Linux), named by process id, and leaves the
/// real one alone. The flag already means "nothing of this run persists",
/// and a second copy of the app on the same machine, started to test
/// something while someone is on a real call, would otherwise write the
/// same log file and rotate the real call's log away. That happened once,
/// and the log of a failed call was lost. The startup line says where the
/// log went.
///
/// Nothing removes the folder on exit, on purpose: the log is what a test run
/// is read by afterwards, and a crash skips the exit path anyway. The system
/// sweeps its temp folder on its own (macOS after three days unused and at
/// reboot, Linux at reboot or by systemd-tmpfiles), which is why it is the
/// temp folder and not a scratch location of our own.
///
/// Read from the raw arguments rather than [`crate::args::Args`], because
/// logging is set up before the arguments are parsed, and the parser reports
/// through logging.
///
/// **`--profile <dir>` is the persistent kind of the same thing**: a folder
/// of the tester's choosing, so a scripted instance keeps its identity, its
/// rooms and their hosts across a restart, which a test of "the app remembers
/// where the room can meet" needs and `--ephemeral` forgets by design. It is
/// never the real folder: a profile that names it, or names nothing, falls
/// back to the throwaway one. `--ephemeral` wins over it.
pub fn data_dir() -> Option<std::path::PathBuf> {
    let real = directories::BaseDirs::new().map(|dirs| dirs.data_local_dir().join(APP_DIR));
    let args: Vec<String> = std::env::args().skip(1).collect();
    match folder(&args, real.as_deref()) {
        Folder::Throwaway => Some(std::env::temp_dir().join(format!("{APP_DIR}-{}", std::process::id()))),
        Folder::Profile(dir) => Some(dir),
        Folder::Real => real,
    }
}

/// Which data folder a run's arguments ask for.
#[derive(Debug, PartialEq)]
enum Folder {
    Throwaway,
    Profile(std::path::PathBuf),
    Real,
}

/// [`data_dir`]'s decision, apart from the process so it can be tested.
/// `real` is the real folder, when the platform has one.
fn folder(args: &[String], real: Option<&std::path::Path>) -> Folder {
    if args.iter().any(|arg| arg == "--ephemeral") {
        return Folder::Throwaway;
    }
    let Some(at) = args.iter().position(|arg| arg == "--profile") else {
        return Folder::Real;
    };
    // A missing value, or another flag where the folder should be, is a
    // mistyped test run: never a reason to fall through to the real folder.
    let Some(dir) = args.get(at + 1).filter(|dir| !dir.is_empty() && !dir.starts_with("--")) else {
        return Folder::Throwaway;
    };
    let dir = std::path::Path::new(dir);
    let dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(here) => here.join(dir),
            Err(_) => return Folder::Throwaway,
        }
    };
    // Compared as written and as resolved, since either may be a link.
    let same = |a: &std::path::Path, b: &std::path::Path| {
        a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
    };
    if real.is_some_and(|real| same(&dir, real)) {
        return Folder::Throwaway;
    }
    Folder::Profile(dir)
}

/// Whether this run keeps its data away from the real folder, with
/// `--ephemeral` or `--profile`: such a run is a test, and never updates the
/// installed app.
pub fn is_test_run() -> bool {
    let real = directories::BaseDirs::new().map(|dirs| dirs.data_local_dir().join(APP_DIR));
    let args: Vec<String> = std::env::args().skip(1).collect();
    folder(&args, real.as_deref()) != Folder::Real
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

    /// The speakers or headphones the user last selected, as `moq_audio`'s
    /// output id. `None` is the system default, as for the microphone.
    pub output: Option<String>,

    /// How loud everyone outside our subroom is, `0.0..=1.0`. `None` is the
    /// default in [`crate::audio::Crosstalk`].
    pub crosstalk_volume: Option<f32>,

    /// Whether everyone outside our subroom is muted. `None` is unmuted.
    pub crosstalk_muted: Option<bool>,

    /// Keyboard shortcuts the user changed: action id to chord, as
    /// `"toggle_mic": "ctrl+alt+m"`, with `""` for one removed on purpose.
    /// Changes only, so a better default later reaches everyone who never
    /// touched it. Read through [`crate::hotkeys::Keymap::from_overrides`],
    /// which skips what it does not understand rather than failing.
    #[serde(skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "lenient_hotkeys")]
    pub hotkeys: BTreeMap<String, String>,

    /// What we call ourselves in a call, cleaned as it was saved. `None` is
    /// no name, and the room sees our short id. Cleaned again wherever it is
    /// read, so a hand edit cannot send what the field would not have let
    /// through.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient_name")]
    pub name: Option<String>,

    /// Whether to look for a newer release on startup and once a day.
    /// `None` is the default, which is yes ([`checks_updates`](Self::checks_updates)):
    /// an `Option` so that a file from before this field, and `Default`, both
    /// mean on. Only ever a check; installing always waits to be asked.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient_flag")]
    pub check_updates: Option<bool>,

    /// The newest release whose banner was dismissed, as `0.2.0`. The banner
    /// stays away until a release newer than this; the dots on the settings
    /// button and the Updates tab do not care. Read through
    /// [`crate::update::banner`], which treats anything unparseable as none.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient_text")]
    pub dismissed_update: Option<String>,

    /// Whether the connection report is on: telling the room how
    /// our connection is doing and showing what others tell. `None` is the
    /// default, which is on ([`connection_report_on`](Self::connection_report_on)), as
    /// for `check_updates`.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient_flag")]
    pub connection_report: Option<bool>,

    /// Whether to host the rooms we join: accept their members directly as
    /// well as through the relay, and say so in presence, so that the room
    /// can still meet if its relay is down. `None` is the default, which is
    /// **off** ([`hosts_rooms`](Self::hosts_rooms)): it lends our uplink to
    /// other people's video, which nobody should find they had agreed to.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient_flag")]
    pub host_rooms: Option<bool>,

    /// Whether to follow, and be able to make, moves of the call to a host:
    /// an experiment. `None` is the default, which is **off**
    /// ([`moves_calls`](Self::moves_calls)): a move sends media through
    /// someone's computer.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient_flag")]
    pub move_calls: Option<bool>,
    /// Whether each video is marked with the way it arrives (the relay, a
    /// host, or directly) and whoever hosts. `None` is the default, which is
    /// **off** ([`shows_routes`](Self::shows_routes)): it is a debugging aid.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient_flag")]
    pub show_routes: Option<bool>,
}

impl Settings {
    /// Whether the automatic update check is on.
    pub fn checks_updates(&self) -> bool {
        self.check_updates.unwrap_or(true)
    }

    /// Whether the connection report is on.
    pub fn connection_report_on(&self) -> bool {
        self.connection_report.unwrap_or(true)
    }

    /// Whether we host the rooms we join.
    pub fn hosts_rooms(&self) -> bool {
        self.host_rooms.unwrap_or(false)
    }

    /// Whether we follow and make moves of the call.
    pub fn moves_calls(&self) -> bool {
        self.move_calls.unwrap_or(false)
    }

    /// Whether each video is marked with the way it arrives.
    pub fn shows_routes(&self) -> bool {
        self.show_routes.unwrap_or(false)
    }
}

/// A text field, or none when it is not text: a hand edit costs only it.
fn lenient_text<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(text) => Ok(Some(text)),
        serde_json::Value::Null => Ok(None),
        other => {
            tracing::warn!("settings: ignoring {other}: not text");
            Ok(None)
        }
    }
}

/// An on/off field, or the default when it is not a boolean: a hand edit
/// that writes `"check_updates": "no"` loses that choice, not the file.
fn lenient_flag<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Bool(flag) => Ok(Some(flag)),
        serde_json::Value::Null => Ok(None),
        other => {
            tracing::warn!("settings: ignoring {other}: not true or false");
            Ok(None)
        }
    }
}

/// The `name` field, or no name when it is not text: a hand edit that writes
/// `"name": 5` loses the name, not the rest of the file.
fn lenient_name<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(name) => Ok(Some(name)),
        serde_json::Value::Null => Ok(None),
        other => {
            tracing::warn!("settings: ignoring name {other}: not text");
            Ok(None)
        }
    }
}

/// The `hotkeys` table, keeping what is text and dropping the rest with a log
/// line. A hand edit that writes `"toggle_mic": 5` loses that one entry, not
/// the camera, microphone and everything else in the file.
fn lenient_hotkeys<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    let serde_json::Value::Object(entries) = value else {
        if !value.is_null() {
            tracing::warn!("settings: hotkeys is not a table; ignoring it");
        }
        return Ok(BTreeMap::new());
    };
    Ok(entries
        .into_iter()
        .filter_map(|(id, chord)| match chord {
            serde_json::Value::String(chord) => Some((id, chord)),
            other => {
                tracing::warn!("settings: ignoring hotkey {other} for {id}: not text");
                None
            }
        })
        .collect())
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
    Some(data_dir()?.join(FILE))
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
/// whole reason the choice is remembered — but it is only reached when there is
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

/// Which output to play through on startup: the microphone's rule, for the
/// microphone's reasons -- outputs have a system default too.
pub fn initial_output(
    devices: &[moq_audio::playback::Device],
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

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn a_profile_is_its_own_folder_and_never_the_real_one() {
        let real = std::env::temp_dir().join("vdr-real-folder-for-a-test");
        let elsewhere = std::env::temp_dir().join("vdr-profile-a");
        let named = |dir: &std::path::Path| folder(&args(&["--profile", &dir.to_string_lossy()]), Some(&real));
        assert_eq!(folder(&args(&[]), Some(&real)), Folder::Real);
        assert_eq!(named(&elsewhere), Folder::Profile(elsewhere.clone()));
        assert_eq!(named(&real), Folder::Throwaway, "the real folder is never a profile");
        assert_eq!(folder(&args(&["--profile"]), Some(&real)), Folder::Throwaway);
        assert_eq!(folder(&args(&["--profile", "--camera-off"]), Some(&real)), Folder::Throwaway);
        assert_eq!(folder(&args(&["--profile", "", "--x"]), Some(&real)), Folder::Throwaway);
        assert_eq!(
            folder(&args(&["--ephemeral", "--profile", &elsewhere.to_string_lossy()]), Some(&real)),
            Folder::Throwaway
        );
        let Folder::Profile(relative) = folder(&args(&["--profile", "b"]), Some(&real)) else { panic!("a profile") };
        assert!(relative.is_absolute() && relative.ends_with("b"), "{relative:?}");
    }

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
        // The point of remembering: the C922 is second in this run's
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

    #[cfg(unix)]
    #[test]
    fn a_readable_key_file_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("vdr-key-test-{}", std::process::id()));
        std::fs::write(&path, "key").expect("test fixture writable");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        private(&path);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn settings_round_trip_through_json() {
        let settings = Settings {
            camera: Some("c922-uid".into()),
            microphone: Some("coreaudio:yeti".into()),
            output: Some("coreaudio:headphones".into()),
            ..Settings::default()
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
            output: Some("coreaudio:headphones".into()),
            ..Settings::default()
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
        assert_eq!(back.output, None, "a missing output is the default");
        assert_eq!(back.crosstalk_volume, None, "a missing volume is the default");
        assert_eq!(back.crosstalk_muted, None, "a missing mute is the default");
    }

    #[test]
    fn the_crosstalk_setting_round_trips() {
        let settings = Settings {
            crosstalk_volume: Some(0.65),
            crosstalk_muted: Some(true),
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).expect("serialises");
        let back: Settings = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, settings);
    }

    #[test]
    fn a_file_written_before_hotkeys_existed_still_loads() {
        let back: Settings = serde_json::from_str(
            r#"{"camera":"abc","microphone":null,"output":null,"crosstalk_volume":0.5,"crosstalk_muted":false}"#,
        )
        .expect("an older file is valid");
        assert_eq!(back.crosstalk_volume, Some(0.5));
        assert!(back.hotkeys.is_empty(), "no changed shortcuts");
    }

    #[test]
    fn a_file_written_before_names_existed_still_loads() {
        let back: Settings = serde_json::from_str(
            r#"{"camera":"abc","microphone":null,"output":null,"crosstalk_volume":0.5,"hotkeys":{"leave":"f5"}}"#,
        )
        .expect("an older file is valid");
        assert_eq!(back.camera.as_deref(), Some("abc"));
        assert_eq!(back.name, None, "no name");
    }

    #[test]
    fn a_name_round_trips_and_is_left_out_when_unset() {
        let settings = Settings {
            name: Some("Ada \u{1F600}".into()),
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).expect("serialises");
        let back: Settings = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, settings);
        let json = serde_json::to_string(&Settings::default()).expect("serialises");
        assert!(!json.contains("name"), "{json}");
    }

    #[test]
    fn a_malformed_name_costs_only_itself() {
        for json in [r#"{"camera":"abc","name":5}"#, r#"{"camera":"abc","name":{"a":1}}"#, r#"{"camera":"abc","name":null}"#] {
            let back: Settings = serde_json::from_str(json).expect("still valid");
            assert_eq!(back.camera.as_deref(), Some("abc"), "{json}");
            assert_eq!(back.name, None, "{json}");
        }
    }

    #[test]
    fn a_malformed_hotkey_costs_only_itself() {
        let back: Settings = serde_json::from_str(r#"{"camera":"abc","hotkeys":{"toggle_mic":5,"leave":"f5"}}"#)
            .expect("still valid");
        assert_eq!(back.camera.as_deref(), Some("abc"));
        assert_eq!(back.hotkeys.len(), 1);
        let back: Settings = serde_json::from_str(r#"{"camera":"abc","hotkeys":[1]}"#).expect("still valid");
        assert_eq!(back.camera.as_deref(), Some("abc"));
        assert!(back.hotkeys.is_empty());
    }

    #[test]
    fn hotkeys_round_trip_and_are_left_out_when_unchanged() {
        let json = serde_json::to_string(&Settings::default()).expect("serialises");
        assert!(!json.contains("hotkeys"), "an older build never sees the field it does not need");
        let settings = Settings {
            hotkeys: [("toggle_mic".to_string(), "ctrl+alt+m".to_string()), ("leave".to_string(), String::new())]
                .into_iter()
                .collect(),
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).expect("serialises");
        let back: Settings = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, settings);
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
    fn a_file_written_before_outputs_existed_still_loads() {
        let back: Settings = serde_json::from_str(r#"{"camera":"abc","microphone":"coreaudio:yeti"}"#)
            .expect("an older file is valid");
        assert_eq!(back.microphone.as_deref(), Some("coreaudio:yeti"));
        assert_eq!(back.output, None);
    }

    #[test]
    fn update_checks_are_on_unless_turned_off() {
        assert!(Settings::default().checks_updates());
        let older: Settings = serde_json::from_str(r#"{"camera":"abc","name":"Ada"}"#).expect("an older file is valid");
        assert!(older.checks_updates(), "a file from before the setting means on");
        let off: Settings = serde_json::from_str(r#"{"check_updates":false}"#).expect("valid");
        assert!(!off.checks_updates());
        let json = serde_json::to_string(&off).expect("serialises");
        let back: Settings = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, off);
        let json = serde_json::to_string(&Settings::default()).expect("serialises");
        assert!(!json.contains("check_updates"), "left out until changed: {json}");
    }

    #[test]
    fn a_dismissed_update_round_trips_and_older_files_load() {
        let older: Settings = serde_json::from_str(r#"{"camera":"abc","check_updates":false}"#).expect("valid");
        assert_eq!(older.dismissed_update, None);
        let settings = Settings {
            dismissed_update: Some("0.2.0".into()),
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).expect("serialises");
        let back: Settings = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, settings);
        let json = serde_json::to_string(&Settings::default()).expect("serialises");
        assert!(!json.contains("dismissed_update"), "{json}");
        let bad: Settings = serde_json::from_str(r#"{"camera":"abc","dismissed_update":5}"#).expect("still valid");
        assert_eq!(bad.camera.as_deref(), Some("abc"));
        assert_eq!(bad.dismissed_update, None);
    }

    #[test]
    fn a_malformed_update_setting_costs_only_itself() {
        let back: Settings = serde_json::from_str(r#"{"camera":"abc","check_updates":"no"}"#).expect("still valid");
        assert_eq!(back.camera.as_deref(), Some("abc"));
        assert!(back.checks_updates());
    }

    #[test]
    fn the_connection_report_is_on_unless_turned_off_and_read_leniently() {
        assert!(Settings::default().connection_report_on());
        let older: Settings = serde_json::from_str(r#"{"camera":"abc","check_updates":false}"#).expect("valid");
        assert!(older.connection_report_on(), "a file from before the setting means on");
        let off: Settings = serde_json::from_str(r#"{"connection_report":false}"#).expect("valid");
        assert!(!off.connection_report_on());
        let back: Settings = serde_json::from_str(&serde_json::to_string(&off).expect("serialises")).expect("deserialises");
        assert_eq!(back, off);
        let json = serde_json::to_string(&Settings::default()).expect("serialises");
        assert!(!json.contains("connection_report"), "left out until changed: {json}");
        let bad: Settings = serde_json::from_str(r#"{"camera":"abc","connection_report":"nope"}"#).expect("still valid");
        assert_eq!(bad.camera.as_deref(), Some("abc"));
        assert!(bad.connection_report_on(), "a malformed flag costs only itself");
    }

    #[test]
    fn hosting_rooms_is_off_unless_turned_on_and_read_leniently() {
        assert!(!Settings::default().hosts_rooms());
        let older: Settings = serde_json::from_str(r#"{"camera":"abc","connection_report":false}"#).expect("valid");
        assert!(!older.hosts_rooms(), "a file from before the setting means off");
        let on: Settings = serde_json::from_str(r#"{"host_rooms":true}"#).expect("valid");
        assert!(on.hosts_rooms());
        let back: Settings = serde_json::from_str(&serde_json::to_string(&on).expect("serialises")).expect("deserialises");
        assert_eq!(back, on);
        let json = serde_json::to_string(&Settings::default()).expect("serialises");
        assert!(!json.contains("host_rooms"), "left out until changed: {json}");
        let bad: Settings = serde_json::from_str(r#"{"camera":"abc","host_rooms":"yes"}"#).expect("still valid");
        assert_eq!(bad.camera.as_deref(), Some("abc"));
        assert!(!bad.hosts_rooms(), "a malformed flag costs only itself");
    }

    #[test]
    fn moving_calls_is_off_unless_turned_on_and_read_leniently() {
        assert!(!Settings::default().moves_calls());
        let older: Settings = serde_json::from_str(r#"{"camera":"abc","host_rooms":true}"#).expect("valid");
        assert!(!older.moves_calls(), "a file from before the setting means off");
        let on: Settings = serde_json::from_str(r#"{"move_calls":true}"#).expect("valid");
        assert!(on.moves_calls());
        let json = serde_json::to_string(&Settings::default()).expect("serialises");
        assert!(!json.contains("move_calls"), "left out until changed: {json}");
        let bad: Settings = serde_json::from_str(r#"{"camera":"abc","move_calls":7}"#).expect("still valid");
        assert_eq!(bad.camera.as_deref(), Some("abc"));
        assert!(!bad.moves_calls(), "a malformed flag costs only itself");
    }

    #[test]
    fn showing_routes_is_off_unless_turned_on_and_read_leniently() {
        assert!(!Settings::default().shows_routes());
        let older: Settings = serde_json::from_str(r#"{"camera":"abc","host_rooms":true}"#).expect("valid");
        assert!(!older.shows_routes(), "a file from before the setting means off");
        let on: Settings = serde_json::from_str(r#"{"show_routes":true}"#).expect("valid");
        assert!(on.shows_routes());
        let back: Settings = serde_json::from_str(&serde_json::to_string(&on).expect("serialises")).expect("deserialises");
        assert_eq!(back, on);
        let json = serde_json::to_string(&Settings::default()).expect("serialises");
        assert!(!json.contains("show_routes"), "left out until changed: {json}");
        let bad: Settings = serde_json::from_str(r#"{"camera":"abc","show_routes":"yes"}"#).expect("still valid");
        assert_eq!(bad.camera.as_deref(), Some("abc"));
        assert!(!bad.shows_routes(), "a malformed flag costs only itself");
    }

    #[test]
    fn with_nothing_remembered_the_system_default_is_used() {
        assert_eq!(choose_microphone(&["coreaudio:one"], None), None);
    }
}
