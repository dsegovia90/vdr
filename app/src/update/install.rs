//! Putting a verified package where the running app is, and starting it.
//!
//! **Only over a real install.** A build folder, a throwaway `--ephemeral`
//! run, or an app macOS is running from a randomised read-only copy (App
//! Translocation, for an app opened straight from Downloads) is not
//! something to overwrite; each says why instead. The path checks are pure
//! ([`locate`]) and under test; the rest needs a disk.
//!
//! **The swap is two renames on one volume**: the new copy is unpacked
//! beside the old one first, so the moment the app is half-replaced never
//! exists. If the second rename fails, the first is undone. The old copy is
//! set aside rather than deleted, because it is what is running; the next
//! launch removes it ([`cleanup`]).

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use semver::Version;

use super::release::Platform;

/// Beside the install: the new copy while it is unpacked and checked.
const STAGING: &str = ".vdr-update-staging";

/// Beside the install: the old copy, set aside until the next launch.
const OLD: &str = ".vdr-update-old";

/// What can be replaced.
#[derive(Clone, Debug, PartialEq)]
pub enum Location {
    /// `<X>/Something.app`, holding the running binary.
    MacBundle(PathBuf),
    /// The unpacked tarball's folder, holding the running binary.
    LinuxFolder(PathBuf),
}

impl Location {
    fn path(&self) -> &Path {
        match self {
            Self::MacBundle(path) | Self::LinuxFolder(path) => path,
        }
    }

    fn beside(&self, name: &str) -> Option<PathBuf> {
        Some(self.path().parent()?.join(name))
    }
}

/// What this copy of the app can do with an update.
#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    /// Download, check, replace, restart.
    Swap(Location),
    /// Download and check, then leave the package for the person: the
    /// folder is not one we are confident we own. The reason is shown.
    DownloadOnly(String),
    /// Nothing but the release page. The reason is shown.
    Refused(String),
}

/// Where the running binary would be replaced, from its path alone.
///
/// The error is a sentence for the settings window.
pub fn locate(exe: &Path, ephemeral: bool, platform: Platform) -> Result<Location, String> {
    if ephemeral {
        return Err("This is an --ephemeral run, which never updates itself.".into());
    }
    let components: Vec<Component> = exe.components().collect();
    if components.iter().any(|part| part.as_os_str() == "target") {
        return Err("This copy runs from a build folder. Rebuild it instead.".into());
    }
    match platform {
        Platform::MacOs => {
            if components.iter().any(|part| part.as_os_str() == "AppTranslocation") {
                return Err(
                    "macOS is running this copy from a temporary location. Move Vdr to Applications, open it from there, and try again."
                        .into(),
                );
            }
            // `<bundle>.app/Contents/MacOS/<binary>`.
            let macos = exe.parent().filter(|dir| dir.file_name() == Some(OsStr::new("MacOS")));
            let contents = macos.and_then(Path::parent).filter(|dir| dir.file_name() == Some(OsStr::new("Contents")));
            let bundle = contents
                .and_then(Path::parent)
                .filter(|dir| dir.extension() == Some(OsStr::new("app")))
                .ok_or("This copy is not inside an app bundle.")?;
            Ok(Location::MacBundle(bundle.to_path_buf()))
        }
        Platform::LinuxX86_64 => {
            let folder = exe.parent().filter(|dir| dir.parent().is_some()).ok_or("This copy's folder cannot be replaced.")?;
            Ok(Location::LinuxFolder(folder.to_path_buf()))
        }
        Platform::Other => Err("There is no package for this platform.".into()),
    }
}

/// What to do with an update here, from the running binary's real path and
/// the disk.
pub fn plan() -> Plan {
    let platform = Platform::current();
    let exe = match std::env::current_exe().and_then(|exe| exe.canonicalize()) {
        Ok(exe) => exe,
        Err(error) => return Plan::Refused(format!("Could not tell where this copy is ({error}).")),
    };
    let location = match locate(&exe, crate::args::Args::get().ephemeral, platform) {
        Ok(location) => location,
        Err(reason) => return Plan::Refused(reason),
    };
    let Some(parent) = location.path().parent() else {
        return Plan::Refused("This copy's folder cannot be replaced.".into());
    };
    if !writable(parent) {
        let reason = format!("{} is not writable by you.", parent.display());
        return match location {
            Location::MacBundle(_) => Plan::Refused(reason),
            Location::LinuxFolder(_) => Plan::DownloadOnly(reason),
        };
    }
    if let Location::LinuxFolder(folder) = &location {
        if !looks_like_package(folder) {
            return Plan::DownloadOnly(format!("{} does not look like an unpacked vdr download.", folder.display()));
        }
    }
    Plan::Swap(location)
}

/// Whether we can create and rename in `dir`: try it. Permission bits alone
/// miss ACLs, read-only volumes and SIP.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".vdr-write-test-{}", std::process::id()));
    let created = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    created
}

/// What `build-linux.sh` puts beside the binary, and so what tells our
/// unpacked tarball from a folder someone copied the binary into.
fn looks_like_package(folder: &Path) -> bool {
    ["vdr", "JOINING.md", "LICENSE-MIT"].iter().all(|name| folder.join(name).is_file())
}

/// Whether replacing `current` with `staged` loses nothing that is not
/// ours: every name in the folder being replaced must be in the new
/// package too. A folder with anything else in it was not only ours, and is
/// left alone.
pub fn only_ours(current: &[String], staged: &[String]) -> bool {
    current.iter().all(|name| staged.contains(name))
}

/// Removes what an earlier update left beside the install: the old copy it
/// set aside, and a staging folder from one that did not finish. At
/// startup, on a thread of its own.
pub fn cleanup(plan: &Plan) {
    let Plan::Swap(location) = plan else { return };
    for name in [OLD, STAGING] {
        if let Some(path) = location.beside(name) {
            if path.exists() {
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => tracing::info!("removed {} left by an earlier update", path.display()),
                    Err(error) => tracing::warn!("could not remove {}: {error}", path.display()),
                }
            }
        }
    }
}

/// Replaces the install at `location` with the package at `archive`, which
/// has already been verified. Returns once the new copy is in place.
///
/// `call_started` is asked just before the swap: a call that began while
/// the package unpacked stops the install there, with nothing changed.
pub fn install(location: &Location, archive: &Path, version: &Version, call_started: impl Fn() -> bool) -> Result<(), String> {
    let staging = location.beside(STAGING).ok_or("no folder beside the app")?;
    let old = location.beside(OLD).ok_or("no folder beside the app")?;
    for leftover in [&staging, &old] {
        remove(leftover)?;
    }
    std::fs::create_dir(&staging).map_err(|error| format!("could not create {}: {error}", staging.display()))?;
    let result = (|| {
        let staged = match location {
            Location::MacBundle(_) => stage_mac(archive, &staging, version)?,
            Location::LinuxFolder(current) => stage_linux(archive, &staging, current)?,
        };
        if call_started() {
            return Err("a call started, so the update was not installed".to_string());
        }
        swap(location.path(), &staged, &old)
    })();
    let _ = remove(&staging);
    result
}

/// Removes a file or folder we made, if it is there.
fn remove(path: &Path) -> Result<(), String> {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove {}: {error}", path.display())),
    }
}

/// The current copy aside, the new one in, and the first undone if the
/// second fails.
fn swap(current: &Path, staged: &Path, old: &Path) -> Result<(), String> {
    std::fs::rename(current, old).map_err(|error| format!("could not move the current copy aside: {error}"))?;
    if let Err(error) = std::fs::rename(staged, current) {
        return match std::fs::rename(old, current) {
            Ok(()) => Err(format!("could not move the new copy into place ({error}); the current copy is unchanged")),
            Err(undo) => {
                tracing::error!("update: could not put the old copy back from {}: {undo}", old.display());
                Err(format!(
                    "could not move the new copy into place ({error}), nor the old one back ({undo}): it is at {}",
                    old.display()
                ))
            }
        };
    }
    Ok(())
}

/// Runs a system tool, turning a failure into a sentence with its output.
fn run(command: &mut Command) -> Result<std::process::Output, String> {
    let shown = format!("{command:?}");
    let output = command.output().map_err(|error| format!("could not run {shown}: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{shown} failed: {}", stderr.trim()));
    }
    Ok(output)
}

/// Unpacks the zip and checks the bundle in it. `package-macos.sh` zips a
/// `vdr` folder holding `Vdr.app` and the docs.
fn stage_mac(archive: &Path, staging: &Path, version: &Version) -> Result<PathBuf, String> {
    note_quarantine(archive);
    // ditto, not unzip: it is what made the zip, and it restores the
    // bundle's attributes and signature layout exactly.
    run(Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(archive).arg(staging))?;
    let bundle = staging.join("vdr").join("Vdr.app");
    if !bundle.join("Contents").join("Info.plist").is_file() {
        return Err("the package does not hold vdr/Vdr.app".into());
    }
    note_quarantine(&bundle);
    let plist = bundle.join("Contents").join("Info.plist");
    let output = run(Command::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg("Print :CFBundleShortVersionString")
        .arg(&plist))?;
    let found = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if found != version.to_string() {
        return Err(format!("the package is version {found}, not {version}"));
    }
    run(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(&bundle))?;
    Ok(bundle)
}

/// Logs whether macOS marked `path` as downloaded from the internet. We
/// never add the mark (a download made by our own process gets none unless
/// the app opts in), and want to know if one ever turns up.
fn note_quarantine(path: &Path) {
    let marked = Command::new("/usr/bin/xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(path)
        .output()
        .is_ok_and(|output| output.status.success());
    if marked {
        tracing::warn!("update: {} carries com.apple.quarantine", path.display());
    } else {
        tracing::debug!("update: {} carries no quarantine mark", path.display());
    }
}

/// Unpacks the tarball and checks the folder in it. `build-linux.sh` tars
/// one folder holding `vdr`, its assets and the docs.
fn stage_linux(archive: &Path, staging: &Path, current: &Path) -> Result<PathBuf, String> {
    run(Command::new("tar").arg("-xzf").arg(archive).arg("-C").arg(staging))?;
    let top = names(staging)?;
    let [folder] = top.as_slice() else {
        return Err("the package does not hold exactly one folder".into());
    };
    let staged = staging.join(folder);
    if !staged.join("vdr").is_file() {
        return Err("the package has no vdr binary".into());
    }
    if !only_ours(&names(current)?, &names(&staged)?) {
        return Err(format!(
            "{} holds files that are not part of vdr, so it was left alone",
            current.display()
        ));
    }
    Ok(staged)
}

/// The names in a folder.
fn names(dir: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|error| format!("could not read {}: {error}", dir.display()))?;
    Ok(entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect())
}

/// Starts the new copy. On macOS, a new instance through Launch Services,
/// so it is the app a double-click would have opened, and this one exits;
/// on Linux, the new binary replaces this process. Returns only on failure.
///
/// No arguments are passed on: a copy started with `--relay` would join
/// that room again.
pub fn relaunch(location: &Location) -> String {
    match location {
        Location::MacBundle(bundle) => {
            match Command::new("/usr/bin/open").arg("-n").arg(bundle).status() {
                Ok(status) if status.success() => {
                    tracing::info!("update: started the new copy; exiting");
                    std::process::exit(0);
                }
                Ok(status) => format!("open exited with {status}"),
                Err(error) => format!("could not run open: {error}"),
            }
        }
        Location::LinuxFolder(folder) => exec(&folder.join("vdr")),
    }
}

#[cfg(unix)]
fn exec(binary: &Path) -> String {
    use std::os::unix::process::CommandExt;
    tracing::info!("update: starting {}", binary.display());
    let error = Command::new(binary).exec();
    format!("could not start {}: {error}", binary.display())
}

#[cfg(not(unix))]
fn exec(binary: &Path) -> String {
    format!("cannot restart into {} on this platform", binary.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(path: &str) -> Result<Location, String> {
        locate(Path::new(path), false, Platform::MacOs)
    }

    #[test]
    fn an_installed_mac_app_is_its_bundle() {
        assert_eq!(
            mac("/Applications/Vdr.app/Contents/MacOS/vdr"),
            Ok(Location::MacBundle("/Applications/Vdr.app".into()))
        );
        // Renamed, or somewhere of the person's own: still a bundle.
        assert_eq!(
            mac("/Users/a/Apps/Video.app/Contents/MacOS/vdr"),
            Ok(Location::MacBundle("/Users/a/Apps/Video.app".into()))
        );
    }

    #[test]
    fn a_build_folder_is_refused() {
        assert!(mac("/Users/a/vdr/app/target/dx/vdr/release/macos/Vdr.app/Contents/MacOS/vdr").is_err());
        assert!(mac("/Users/a/vdr/app/target/debug/vdr").is_err());
        assert!(locate(Path::new("/home/a/vdr/app/target/release/vdr"), false, Platform::LinuxX86_64).is_err());
        // A folder merely containing the word is fine.
        assert!(mac("/Users/a/targets/Vdr.app/Contents/MacOS/vdr").is_ok());
    }

    #[test]
    fn a_translocated_app_is_refused() {
        let error = mac("/private/var/folders/xy/abc123/T/AppTranslocation/0F3A-1B2C/d/Vdr.app/Contents/MacOS/vdr").unwrap_err();
        assert!(error.contains("Applications"), "{error}");
    }

    #[test]
    fn an_ephemeral_run_is_refused() {
        let error = locate(Path::new("/Applications/Vdr.app/Contents/MacOS/vdr"), true, Platform::MacOs).unwrap_err();
        assert!(error.contains("ephemeral"), "{error}");
        assert!(locate(Path::new("/opt/vdr/vdr"), true, Platform::LinuxX86_64).is_err());
    }

    #[test]
    fn a_binary_outside_a_bundle_is_refused_on_macos() {
        assert!(mac("/usr/local/bin/vdr").is_err());
        assert!(mac("/Applications/Vdr/Contents/MacOS/vdr").is_err(), "no .app");
        assert!(mac("/Applications/Vdr.app/Contents/Resources/vdr").is_err());
    }

    #[test]
    fn linux_replaces_the_binarys_folder() {
        assert_eq!(
            locate(Path::new("/home/a/apps/app/vdr"), false, Platform::LinuxX86_64),
            Ok(Location::LinuxFolder("/home/a/apps/app".into()))
        );
        assert!(locate(Path::new("/vdr"), false, Platform::LinuxX86_64).is_err(), "never the root");
    }

    #[test]
    fn other_platforms_have_nothing_to_install() {
        assert!(locate(Path::new("C:/vdr/vdr.exe"), false, Platform::Other).is_err());
    }

    #[test]
    fn a_folder_with_files_of_its_own_is_not_replaced() {
        let package: Vec<String> = ["vdr", "assets", "JOINING.md", "LICENSE-MIT"].map(String::from).to_vec();
        assert!(only_ours(&package, &package));
        assert!(only_ours(&package[..2], &package), "fewer is fine: an older layout");
        let mut mine = package.clone();
        mine.push("notes.txt".into());
        assert!(!only_ours(&mine, &package));
    }

    #[test]
    fn a_failed_swap_puts_the_old_copy_back() {
        let dir = std::env::temp_dir().join(format!("vdr-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Vdr.app")).unwrap();
        std::fs::write(dir.join("Vdr.app/marker"), "old").unwrap();
        // The staged copy does not exist, so the second rename fails.
        let result = swap(&dir.join("Vdr.app"), &dir.join("missing"), &dir.join(OLD));
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(dir.join("Vdr.app/marker")).unwrap(), "old");
        assert!(!dir.join(OLD).exists());

        std::fs::create_dir_all(dir.join("new")).unwrap();
        std::fs::write(dir.join("new/marker"), "new").unwrap();
        swap(&dir.join("Vdr.app"), &dir.join("new"), &dir.join(OLD)).expect("swaps");
        assert_eq!(std::fs::read_to_string(dir.join("Vdr.app/marker")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(dir.join(OLD).join("marker")).unwrap(), "old");
        cleanup(&Plan::Swap(Location::MacBundle(dir.join("Vdr.app"))));
        assert!(!dir.join(OLD).exists(), "the next launch removes it");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
