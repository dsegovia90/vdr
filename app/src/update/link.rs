//! Opening a release's page in the browser.
//!
//! Blitz cannot open a link itself, so the system's opener does: `open` on
//! macOS, `xdg-open` on Linux, started directly, never through a shell, so
//! nothing in the URL is ever interpreted. And only a URL that is plainly
//! our release page: the URL comes from the network, and an opener will
//! happily hand a `file:` path or an app's custom scheme to whatever claims it.

use std::process::Command;

use super::verify;

/// Every page the release feed should ever point at starts with this.
const RELEASES: &str = "https://github.com/dsegovia90/vdr/";

/// Whether `url` may be handed to the system opener. A test-key build, whose
/// feed is a local server, may open any `http` or `https` URL.
pub fn openable(url: &str, test_build: bool) -> bool {
    // A leading `-` could read as an option, and whitespace or control
    // characters have no place in a URL we would show.
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    if test_build {
        return url.starts_with("https://") || url.starts_with("http://");
    }
    url.starts_with(RELEASES) && url.len() > RELEASES.len()
}

/// Opens the release page, or says why not, in which case the caller shows
/// the URL as text instead. Returns as soon as the opener has started; a
/// thread of its own waits for it, so it leaves no zombie behind.
pub fn open_release_page(url: &str) -> Result<(), String> {
    if !openable(url, verify::is_test_build()) {
        tracing::warn!("update: not opening {url:?}: not a release page");
        return Err("not a release page".into());
    }
    let opener = if cfg!(target_os = "macos") {
        "/usr/bin/open"
    } else if cfg!(target_os = "linux") {
        "xdg-open"
    } else {
        return Err("no way to open links on this platform".into());
    };
    let mut child = Command::new(opener)
        .arg(url)
        .spawn()
        .map_err(|error| format!("could not run {opener}: {error}"))?;
    let spawned = std::thread::Builder::new().name("open-link".into()).spawn(move || match child.wait() {
        Ok(status) if !status.success() => tracing::warn!("update: {opener} exited with {status}"),
        Ok(_) => {}
        Err(error) => tracing::warn!("update: waiting for {opener}: {error}"),
    });
    if let Err(error) = spawned {
        tracing::debug!("could not start the thread that waits for the opener: {error}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_build_opens_only_our_release_pages() {
        assert!(openable("https://github.com/dsegovia90/vdr/releases/tag/v0.2.0", false));
        for url in [
            "http://github.com/dsegovia90/vdr/releases/tag/v0.2.0",
            "https://github.com/dsegovia90/vdr",
            "https://github.com/dsegovia90/vdr/",
            "https://github.com/dsegovia90/vdrevil/releases",
            "https://github.com/someone/vdr/releases/tag/v0.2.0",
            "https://github.com.evil.example/dsegovia90/vdr/x",
            "https://example.com/https://github.com/dsegovia90/vdr/x",
            "file:///etc/passwd",
            "-a Calculator",
            "https://github.com/dsegovia90/vdr/releases tag",
            "https://github.com/dsegovia90/vdr/releases\n",
            "",
        ] {
            assert!(!openable(url, false), "{url:?}");
        }
    }

    #[test]
    fn a_test_build_opens_any_web_url_but_nothing_else() {
        assert!(openable("http://127.0.0.1:8000/release", true));
        assert!(openable("https://example.com/notes", true));
        assert!(!openable("file:///etc/passwd", true));
        assert!(!openable("-a Calculator", true));
        assert!(!openable("http://x y", true));
    }
}
