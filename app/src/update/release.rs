//! What a published release is, and which of its files is ours. Pure: no
//! network, no disk, so all of it is under test.
//!
//! **The asset names are an interface with the release workflow.** An
//! installed app looks for exactly these names; renaming a package in CI
//! strands every copy already out there on the release it has.

use semver::Version;
use serde::Deserialize;

/// The signed list of checksums every release carries beside its packages.
pub const SUMS: &str = "SHA256SUMS";

/// `SUMS`'s raw 64-byte ed25519 signature.
pub const SIGNATURE: &str = "SHA256SUMS.sig";

/// The version this build is.
pub fn running() -> Version {
    // Cargo refuses a manifest whose version is not semver, so this cannot
    // fail on a build that compiled; the fallback only keeps it panic-free.
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// The shape of GitHub's "latest release" answer, as far as we read it.
#[derive(Deserialize)]
struct Json {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<JsonAsset>,
}

#[derive(Deserialize)]
struct JsonAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// A release, as the updater needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// As tagged, `v` included: the asset names carry it in that form.
    pub tag: String,
    pub version: Version,
    /// The release's page, with its notes.
    pub page: String,
    pub assets: Vec<Asset>,
}

/// One downloadable file of a release.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// As GitHub states it; `0` when it did not.
    pub size: u64,
}

/// Reads GitHub's release JSON. The error is a sentence for the log and
/// the settings window.
pub fn parse(json: &[u8]) -> Result<Release, String> {
    let json: Json = serde_json::from_slice(json).map_err(|error| format!("the release information was not readable ({error})"))?;
    let version = version_of(&json.tag_name).ok_or_else(|| format!("the latest release's tag {:?} is not a version", json.tag_name))?;
    Ok(Release {
        tag: json.tag_name,
        version,
        page: json.html_url,
        assets: json
            .assets
            .into_iter()
            .map(|asset| Asset {
                name: asset.name,
                url: asset.browser_download_url,
                size: asset.size,
            })
            .collect(),
    })
}

/// The version a tag names: `v1.2.3` is 1.2.3. Only the `v` form, which is
/// what the release workflow is triggered by, so a stray tag is not read as
/// a release.
pub fn version_of(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v')?).ok()
}

/// Whether `candidate` is worth offering to someone running `running`.
///
/// **Strictly newer, by precedence, and never a pre-release.** Build
/// metadata does not count (`cmp_precedence`, not `Ord`, which would order
/// `1.0.0+a` and `1.0.0+b`). A pre-release is never offered: GitHub's
/// "latest" already leaves them out, so one arriving here is a feed that
/// is not the one we think.
pub fn is_update(running: &Version, candidate: &Version) -> bool {
    candidate.pre.is_empty() && candidate.cmp_precedence(running) == std::cmp::Ordering::Greater
}

/// Which package this build installs from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Platform {
    /// Either architecture: the package is universal.
    MacOs,
    LinuxX86_64,
    /// Anything the release workflow builds nothing for.
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Self::LinuxX86_64
        } else {
            Self::Other
        }
    }
}

/// The package's file name for `platform` in the release tagged `tag`.
/// These are the names `deploy/client/package-macos.sh` and
/// `build-linux.sh` write when the workflow passes them the tag.
pub fn package_name(platform: Platform, tag: &str) -> Option<String> {
    match platform {
        Platform::MacOs => Some(format!("vdr-macos-universal-{tag}.zip")),
        Platform::LinuxX86_64 => Some(format!("vdr-linux-x86_64-{tag}.tar.gz")),
        Platform::Other => None,
    }
}

/// The three files an update needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub archive: Asset,
    pub sums: Asset,
    pub signature: Asset,
}

/// Picks this platform's package out of a release, with its checksums and
/// their signature. The error is a sentence for the settings window.
pub fn package(release: &Release, platform: Platform) -> Result<Package, String> {
    let name = package_name(platform, &release.tag).ok_or("There is no package for this platform")?;
    let find = |wanted: &str| release.assets.iter().find(|asset| asset.name == wanted).cloned();
    let archive = find(&name).ok_or_else(|| format!("Release {} has no {name}", release.tag))?;
    // A release made before signing began has packages but no signature.
    // Without one there is nothing to trust the package by, so it is the
    // release page or nothing.
    let sums = find(SUMS).ok_or_else(|| format!("Release {} is not signed", release.tag))?;
    let signature = find(SIGNATURE).ok_or_else(|| format!("Release {} is not signed", release.tag))?;
    Ok(Package { archive, sums, signature })
}

/// Whole percent done, rounded down: what the progress line shows, so a
/// message is sent at most once per step.
pub fn percent(done: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    (u128::from(done.min(total)) * 100 / u128::from(total)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real GitHub answer, with the fields we do not read
    /// left in to show they are ignored.
    const FIXTURE: &str = r#"{
        "url": "https://api.github.com/repos/dsegovia90/vdr/releases/1",
        "html_url": "https://github.com/dsegovia90/vdr/releases/tag/v0.2.0",
        "id": 1,
        "tag_name": "v0.2.0",
        "name": "vdr v0.2.0",
        "draft": false,
        "prerelease": false,
        "assets": [
            {"name": "SHA256SUMS", "size": 200, "browser_download_url": "https://github.com/dsegovia90/vdr/releases/download/v0.2.0/SHA256SUMS", "content_type": "application/octet-stream"},
            {"name": "SHA256SUMS.sig", "size": 64, "browser_download_url": "https://github.com/dsegovia90/vdr/releases/download/v0.2.0/SHA256SUMS.sig"},
            {"name": "vdr-linux-x86_64-v0.2.0.tar.gz", "size": 40000000, "browser_download_url": "https://github.com/dsegovia90/vdr/releases/download/v0.2.0/vdr-linux-x86_64-v0.2.0.tar.gz"},
            {"name": "vdr-macos-universal-v0.2.0.zip", "size": 50000000, "browser_download_url": "https://github.com/dsegovia90/vdr/releases/download/v0.2.0/vdr-macos-universal-v0.2.0.zip"}
        ],
        "body": "notes"
    }"#;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn a_release_is_read_from_githubs_json() {
        let release = parse(FIXTURE.as_bytes()).expect("parses");
        assert_eq!(release.tag, "v0.2.0");
        assert_eq!(release.version, v("0.2.0"));
        assert_eq!(release.page, "https://github.com/dsegovia90/vdr/releases/tag/v0.2.0");
        assert_eq!(release.assets.len(), 4);
        assert_eq!(release.assets[3].size, 50_000_000);
    }

    #[test]
    fn bad_json_and_bad_tags_are_errors_not_panics() {
        assert!(parse(b"").is_err());
        assert!(parse(b"{ not json").is_err());
        assert!(parse(br#"{"message":"API rate limit exceeded"}"#).is_err());
        assert!(parse(br#"{"tag_name":"nightly","html_url":"x"}"#).is_err());
        assert!(parse(br#"{"tag_name":"v0.2.0","html_url":"x"}"#).is_ok(), "no assets is still a release");
    }

    #[test]
    fn tags_are_read_only_in_the_v_form() {
        assert_eq!(version_of("v1.2.3"), Some(v("1.2.3")));
        assert_eq!(version_of("v1.2.3-beta.1"), Some(v("1.2.3-beta.1")));
        for garbage in ["1.2.3", "v1.2", "v", "", "vx.y.z", "release-1.2.3", "v1.2.3.4", " v1.2.3"] {
            assert_eq!(version_of(garbage), None, "{garbage:?}");
        }
    }

    #[test]
    fn only_a_strictly_newer_release_is_an_update() {
        let running = v("0.2.0");
        assert!(is_update(&running, &v("0.2.1")));
        assert!(is_update(&running, &v("0.3.0")));
        assert!(is_update(&running, &v("1.0.0")));
        assert!(!is_update(&running, &v("0.2.0")), "equal");
        assert!(!is_update(&running, &v("0.1.9")), "older");
        assert!(!is_update(&running, &v("0.2.0+build.5")), "build metadata is not newer");
    }

    #[test]
    fn a_pre_release_is_never_offered() {
        assert!(!is_update(&v("0.2.0"), &v("0.3.0-beta.1")));
        // But a release does replace the pre-release it followed.
        assert!(is_update(&v("0.3.0-beta.1"), &v("0.3.0")));
    }

    #[test]
    fn each_platform_finds_its_own_package() {
        let release = parse(FIXTURE.as_bytes()).unwrap();
        let mac = package(&release, Platform::MacOs).expect("mac");
        assert_eq!(mac.archive.name, "vdr-macos-universal-v0.2.0.zip");
        assert_eq!(mac.sums.name, SUMS);
        assert_eq!(mac.signature.name, SIGNATURE);
        let linux = package(&release, Platform::LinuxX86_64).expect("linux");
        assert_eq!(linux.archive.name, "vdr-linux-x86_64-v0.2.0.tar.gz");
        let other = package(&release, Platform::Other).unwrap_err();
        assert_eq!(other, "There is no package for this platform");
    }

    #[test]
    fn an_unsigned_release_offers_no_package() {
        let mut release = parse(FIXTURE.as_bytes()).unwrap();
        release.assets.retain(|asset| asset.name != SIGNATURE);
        assert!(package(&release, Platform::MacOs).unwrap_err().contains("not signed"));
        let mut release = parse(FIXTURE.as_bytes()).unwrap();
        release.assets.retain(|asset| !asset.name.ends_with(".zip"));
        assert!(package(&release, Platform::MacOs).is_err());
        assert!(package(&release, Platform::LinuxX86_64).is_ok());
    }

    #[test]
    fn a_package_named_for_another_tag_is_not_picked() {
        let mut release = parse(FIXTURE.as_bytes()).unwrap();
        for asset in &mut release.assets {
            asset.name = asset.name.replace("v0.2.0", "v0.1.0");
        }
        assert!(package(&release, Platform::MacOs).is_err());
    }

    #[test]
    fn percent_is_whole_steps_and_never_past_a_hundred() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(0, 200), 0);
        assert_eq!(percent(1, 200), 0);
        assert_eq!(percent(2, 200), 1);
        assert_eq!(percent(199, 200), 99);
        assert_eq!(percent(200, 200), 100);
        assert_eq!(percent(300, 200), 100);
        assert_eq!(percent(u64::MAX, u64::MAX), 100);
    }
}
