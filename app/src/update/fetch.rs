//! The network half: asking GitHub what the latest release is, and
//! downloading its files. Every failure is a sentence, never a panic.
//!
//! **rustls, chosen explicitly.** reqwest is already in the tree with both
//! its TLS backends compiled (blitz-net turns on native-tls, the moq stack
//! rustls), and with both present its default is native-tls. Naming rustls
//! keeps this on the same stack and the process crypto provider `main.rs`
//! installs, with the platform's certificate verifier.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use super::release::{self, Asset, Release};
use super::verify::{self, Hasher};

/// GitHub's answer for the newest non-draft, non-pre-release release.
const LATEST: &str = "https://api.github.com/repos/dsegovia90/vdr/releases/latest";

/// GitHub refuses a request without one. Generic on purpose: it names the
/// program, not the person.
const USER_AGENT: &str = "vdr-updater";

/// Bigger than this is not a package of ours, whatever the server says.
const MAX_PACKAGE: u64 = 1 << 30;

/// `SHA256SUMS` lists a handful of files; the signature is 64 bytes.
const MAX_SMALL: u64 = 64 * 1024;

/// Where the release information comes from.
///
/// `VDR_UPDATE_FEED` replaces GitHub only in a build made with a test key
/// (see `verify`): a URL answering with the same JSON as GitHub's latest
/// release. A release build never reads the variable.
fn feed() -> String {
    if verify::is_test_build() {
        if let Ok(url) = std::env::var("VDR_UPDATE_FEED") {
            tracing::info!("update: test build, reading releases from {url}");
            return url;
        }
    }
    LATEST.to_string()
}

pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .tls_backend_rustls()
        // A test feed may be a plain local server; a release build only
        // talks https, redirects included.
        .https_only(!verify::is_test_build())
        .connect_timeout(Duration::from_secs(15))
        // Between reads, not overall: a slow link may take minutes for a
        // package, but a stalled one should not hang forever.
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("could not set up the connection ({error})"))
}

/// The latest release, as GitHub describes it.
pub async fn latest(client: &reqwest::Client) -> Result<Release, String> {
    let response = client
        .get(feed())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| format!("could not reach GitHub ({error})"))?;
    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err("GitHub is limiting requests from this network; it will be tried again later".into());
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err("no release has been published yet".into());
    }
    if !status.is_success() {
        return Err(format!("GitHub answered {status}"));
    }
    let body = read_small(response, MAX_SMALL * 16).await?;
    release::parse(&body)
}

/// A small file, whole, in memory: the checksums and their signature.
pub async fn small(client: &reqwest::Client, asset: &Asset) -> Result<Vec<u8>, String> {
    let response = get(client, asset).await?;
    read_small(response, MAX_SMALL).await
}

async fn get(client: &reqwest::Client, asset: &Asset) -> Result<reqwest::Response, String> {
    let response = client
        .get(&asset.url)
        .send()
        .await
        .map_err(|error| format!("could not download {} ({error})", asset.name))?;
    if !response.status().is_success() {
        return Err(format!("downloading {} answered {}", asset.name, response.status()));
    }
    Ok(response)
}

async fn read_small(mut response: reqwest::Response, limit: u64) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| format!("the download broke off ({error})"))? {
        body.extend_from_slice(&chunk);
        if body.len() as u64 > limit {
            return Err("the server sent far more than expected".into());
        }
    }
    Ok(body)
}

/// Downloads `asset` to `path`, hashing as it goes, and returns the hash.
/// `progress` hears bytes done and the total after each piece.
///
/// Written to `<path>.part` and renamed at the end, so a file at `path` is
/// always a whole one.
pub async fn download(
    client: &reqwest::Client,
    asset: &Asset,
    path: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<[u8; 32], String> {
    let mut response = get(client, asset).await?;
    let total = response.content_length().filter(|&n| n > 0).unwrap_or(asset.size);
    if total > MAX_PACKAGE {
        return Err(format!("{} is too large to be a vdr package", asset.name));
    }
    let part = path.with_file_name(format!("{}.part", asset.name));
    let mut file = std::fs::File::create(&part).map_err(|error| format!("could not create {} ({error})", part.display()))?;
    let mut hasher = Hasher::default();
    let mut done = 0u64;
    let result = async {
        while let Some(chunk) = response.chunk().await.map_err(|error| format!("the download broke off ({error})"))? {
            done += chunk.len() as u64;
            if done > MAX_PACKAGE || (total > 0 && done > total) {
                return Err(format!("{} is larger than announced", asset.name));
            }
            hasher.update(&chunk);
            file.write_all(&chunk).map_err(|error| format!("could not write {} ({error})", part.display()))?;
            progress(done, total);
        }
        if total > 0 && done != total {
            return Err(format!("{} arrived incomplete", asset.name));
        }
        file.sync_all().map_err(|error| format!("could not write {} ({error})", part.display()))?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let _ = std::fs::remove_file(&part);
        return Err(error);
    }
    std::fs::rename(&part, path).map_err(|error| format!("could not save {} ({error})", path.display()))?;
    Ok(hasher.finish())
}
