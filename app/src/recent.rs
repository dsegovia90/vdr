//! Rooms joined before, so the idle screen can offer them again.
//!
//! **The file holds credentials.** A room link can carry the relay's token
//! (`?jwt=…`), and the whole link is kept so that rejoining is one click. So it
//! is its own file, `rooms.json`, rather than a field in `settings.json`, which
//! people paste into bug reports; it is written owner-only from the moment it
//! exists; and the token never leaves it except to dial: what is shown is the
//! room's name and its relay's host, and what is logged is the redacted link.
//!
//! **A room is remembered when it is joined, not when it is tried.** The caller
//! records it on reaching the room, so a typo or a refused token never enters
//! the list.
//!
//! Nothing here panics, for the reasons [`crate::settings`] gives: a missing or
//! corrupt file is an empty list, and a failed write is a log line.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use dioxus_native::prelude::*;
use serde::{Deserialize, Serialize};

/// The file, beside the settings.
const FILE: &str = "rooms.json";

/// How many unpinned rooms are kept. Pinned ones do not count and are never
/// dropped.
pub const KEPT: usize = 10;

/// A room joined before.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    /// The link as it was last joined, token and all.
    link: String,
    /// When it was last joined, in seconds since the Unix epoch.
    last_joined: u64,
    /// Kept whatever the cap, and listed first.
    #[serde(default)]
    pinned: bool,
}

/// By hand, so that a stray `{:?}` cannot write the token into a log.
impl std::fmt::Debug for Room {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Room")
            .field("link", &self.log_form())
            .field("last_joined", &self.last_joined)
            .field("pinned", &self.pinned)
            .finish()
    }
}

impl Room {
    fn url(&self) -> Option<url::Url> {
        crate::args::relay_url(&self.link).ok()
    }

    /// The full link, for dialling only: it carries the token.
    pub fn link(&self) -> &str {
        &self.link
    }

    /// What makes two entries the same room; see [`identity`].
    pub fn key(&self) -> String {
        self.url().map_or_else(String::new, |url| identity(&url))
    }

    pub fn pinned(&self) -> bool {
        self.pinned
    }

    pub fn last_joined(&self) -> u64 {
        self.last_joined
    }

    /// The room as the list names it: its path, or its relay when the path is
    /// empty, since the relay then roots the session at `/` and that is the
    /// room.
    pub fn name(&self) -> String {
        let Some(url) = self.url() else { return String::new() };
        let path = url.path().trim_matches('/');
        if path.is_empty() { self.host() } else { path.to_string() }
    }

    /// The relay, as the list shows it beside the name: host, and port where
    /// one was written. An iroh relay's host is a 64-character key, of which
    /// the front tells two apart.
    pub fn host(&self) -> String {
        let Some(url) = self.url() else { return String::new() };
        let host = url.host_str().unwrap_or_default();
        let host = if url.scheme() == "iroh" && host.len() > 12 { &host[..12] } else { host };
        match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        }
    }

    /// The link for a log line, with the token blanked.
    pub fn log_form(&self) -> String {
        self.url().map_or_else(|| "an unreadable link".to_string(), |url| crate::net::redacted(&url))
    }
}

/// A room's identity: scheme, relay host, port and path. Not the query, so a
/// fresh token for the same room replaces the old entry rather than adding a
/// second one.
pub fn identity(url: &url::Url) -> String {
    let port = url.port_or_known_default().map(|port| format!(":{port}")).unwrap_or_default();
    format!("{}://{}{port}{}", url.scheme(), url.host_str().unwrap_or_default(), url.path())
}

/// Every room remembered, in the order the list shows them: pinned first,
/// then most recently joined first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecentRooms {
    rooms: Vec<Room>,
}

impl RecentRooms {
    pub fn rooms(&self) -> &[Room] {
        &self.rooms
    }

    /// Records a join at `now`: the room moves to the top, takes this link (a
    /// newer token wins), and keeps its pin.
    pub fn joined(&mut self, url: &url::Url, now: u64) {
        let key = identity(url);
        let pinned = self.rooms.iter().any(|room| room.pinned && room.key() == key);
        self.rooms.retain(|room| room.key() != key);
        self.rooms.push(Room {
            link: url.to_string(),
            last_joined: now,
            pinned,
        });
        self.tidy();
    }

    pub fn set_pinned(&mut self, key: &str, pinned: bool) {
        for room in self.rooms.iter_mut().filter(|room| room.key() == key) {
            room.pinned = pinned;
        }
        self.tidy();
    }

    pub fn remove(&mut self, key: &str) {
        self.rooms.retain(|room| room.key() != key);
    }

    /// Sorts, and drops the oldest unpinned rooms past [`KEPT`]. Run after
    /// every change, so the cap also applies to a room just unpinned.
    fn tidy(&mut self) {
        self.rooms
            .sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.last_joined.cmp(&a.last_joined)));
        let mut unpinned = 0;
        self.rooms.retain(|room| {
            if room.pinned {
                return true;
            }
            unpinned += 1;
            unpinned <= KEPT
        });
    }

    /// Reads the file, or an empty list. Synchronous, once at startup, as
    /// [`crate::settings::Settings::load`] is.
    pub fn load() -> Self {
        match path() {
            Some(path) => Self::load_from(&path),
            None => Self::default(),
        }
    }

    fn load_from(path: &Path) -> Self {
        // A file from before it was written private, or loosened by hand.
        crate::settings::private(path);
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                tracing::warn!("could not read {}: {error}", path.display());
                return Self::default();
            }
        };
        let mut list: Self = match serde_json::from_str(&text) {
            Ok(list) => list,
            Err(error) => {
                // serde_json's message names a line and column, never the
                // text, so it cannot carry a token out.
                tracing::warn!("{} is not a list of rooms ({error}); ignoring it", path.display());
                return Self::default();
            }
        };
        let before = list.rooms.len();
        list.rooms.retain(|room| room.url().is_some());
        if list.rooms.len() < before {
            tracing::warn!("ignoring {} remembered rooms whose link is not a relay url", before - list.rooms.len());
        }
        list.tidy();
        list
    }

    /// Writes the list off the UI thread, as [`crate::settings::Settings::update`]
    /// does. Writes can finish out of order, so each carries a number and one
    /// older than what is already on disk is skipped.
    pub fn save(&self) {
        static ISSUED: AtomicU64 = AtomicU64::new(0);
        static WRITTEN: Mutex<u64> = Mutex::new(0);
        let Some(path) = path() else {
            tracing::warn!("no local-data directory on this platform; not saving recent rooms");
            return;
        };
        let number = ISSUED.fetch_add(1, Ordering::Relaxed) + 1;
        let list = self.clone();
        let spawned = std::thread::Builder::new().name("rooms-save".into()).spawn(move || {
            let Ok(mut written) = WRITTEN.lock() else { return };
            if *written < number {
                list.write_to(&path);
                *written = number;
            }
        });
        if let Err(error) = spawned {
            tracing::warn!("could not spawn the thread that saves recent rooms: {error}");
        }
    }

    /// The blocking write: to a fresh owner-only file beside the real one, then
    /// renamed over it, so the tokens are never readable by anyone else, not
    /// even for the moment between creating the file and tightening it.
    fn write_to(&self, path: &Path) {
        let Some(dir) = path.parent() else { return };
        if let Err(error) = std::fs::create_dir_all(dir) {
            tracing::warn!("could not create {}: {error}", dir.display());
            return;
        }
        let json = match serde_json::to_string_pretty(self) {
            Ok(json) => json,
            Err(error) => {
                tracing::warn!("could not serialise recent rooms: {error}");
                return;
            }
        };
        let partial = path.with_extension("json.partial");
        // A leftover from a crash keeps whatever mode it had; start afresh.
        let _ = std::fs::remove_file(&partial);
        let result = create_private(&partial)
            .and_then(|mut file| std::io::Write::write_all(&mut file, json.as_bytes()))
            .and_then(|()| std::fs::rename(&partial, path));
        match result {
            Ok(()) => tracing::debug!("saved {} recent rooms to {}", self.rooms.len(), path.display()),
            Err(error) => {
                let _ = std::fs::remove_file(&partial);
                tracing::warn!("could not write {}: {error}", path.display());
            }
        }
    }
}

/// Changes the list on screen and saves it.
pub fn change(rooms: &mut Signal<RecentRooms>, edit: impl FnOnce(&mut RecentRooms)) {
    // The guard is dropped before saving, which reads the list again.
    let snapshot = {
        let mut list = rooms.write();
        edit(&mut list);
        list.clone()
    };
    snapshot.save();
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(path)
}

/// Where the list lives: the data folder, so an `--ephemeral` run has its own.
fn path() -> Option<PathBuf> {
    Some(crate::settings::data_dir()?.join(FILE))
}

/// Seconds since the Unix epoch; zero on a clock set before 1970.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// How long ago `then` was, as the list says it. Counted in elapsed time, not
/// calendar days: "yesterday" is 24 to 48 hours ago. A week or more is a date
/// (UTC), since "5 weeks ago" is harder to place than the day itself.
pub fn relative(then: u64, now: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    // A clock moved backwards reads as now rather than as the future.
    let ago = now.saturating_sub(then);
    if ago < MINUTE {
        "just now".to_string()
    } else if ago < HOUR {
        format!("{} min ago", ago / MINUTE)
    } else if ago < DAY {
        match ago / HOUR {
            1 => "1 hour ago".to_string(),
            hours => format!("{hours} hours ago"),
        }
    } else if ago < 2 * DAY {
        "yesterday".to_string()
    } else if ago < 7 * DAY {
        format!("{} days ago", ago / DAY)
    } else {
        date(then)
    }
}

/// `YYYY-MM-DD` in UTC, by Howard Hinnant's `civil_from_days`, which saves a
/// date crate for one string.
fn date(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "eyJhbGciOiJFUzI1NiJ9.secret-payload.signature";

    fn url(text: &str) -> url::Url {
        text.parse().expect("a test url")
    }

    fn keys(list: &RecentRooms) -> Vec<String> {
        list.rooms().iter().map(Room::name).collect()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vdr-rooms-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join(FILE)
    }

    #[test]
    fn the_same_room_with_another_token_is_one_entry_and_the_newer_link_wins() {
        let mut list = RecentRooms::default();
        list.joined(&url("https://relay.example.com/team?jwt=old"), 100);
        list.joined(&url("https://relay.example.com:443/team?jwt=new"), 200);
        assert_eq!(list.rooms().len(), 1, "query and default port do not make a new room");
        assert_eq!(list.rooms()[0].link(), "https://relay.example.com/team?jwt=new");
        assert_eq!(list.rooms()[0].last_joined(), 200);
    }

    #[test]
    fn host_port_scheme_and_path_each_make_a_different_room() {
        let mut list = RecentRooms::default();
        for (at, link) in [
            "https://relay.example.com/team",
            "https://relay.example.com:4443/team",
            "http://relay.example.com/team",
            "https://other.example.com/team",
            "https://relay.example.com/other",
        ]
        .into_iter()
        .enumerate()
        {
            list.joined(&url(link), at as u64);
        }
        assert_eq!(list.rooms().len(), 5);
    }

    #[test]
    fn joining_again_moves_a_room_to_the_top() {
        let mut list = RecentRooms::default();
        list.joined(&url("https://relay.example.com/a"), 1);
        list.joined(&url("https://relay.example.com/b"), 2);
        assert_eq!(keys(&list), ["b", "a"]);
        list.joined(&url("https://relay.example.com/a"), 3);
        assert_eq!(keys(&list), ["a", "b"]);
    }

    #[test]
    fn pinned_rooms_come_first_and_survive_the_cap_and_a_rejoin() {
        let mut list = RecentRooms::default();
        list.joined(&url("https://relay.example.com/kept"), 0);
        let kept = list.rooms()[0].key();
        list.set_pinned(&kept, true);
        for at in 1..=(KEPT as u64 + 5) {
            list.joined(&url(&format!("https://relay.example.com/room{at}")), at);
        }
        assert_eq!(list.rooms().len(), KEPT + 1, "ten unpinned and the pinned one");
        assert_eq!(list.rooms()[0].name(), "kept", "pinned first, though oldest");
        assert_eq!(list.rooms()[1].name(), "room15", "then newest first");
        assert_eq!(list.rooms()[KEPT].name(), "room6", "the oldest unpinned went");

        list.joined(&url("https://relay.example.com/kept?jwt=fresh"), 99);
        assert!(list.rooms()[0].pinned(), "a rejoin keeps the pin");
    }

    #[test]
    fn the_eleventh_room_drops_the_oldest_unpinned() {
        let mut list = RecentRooms::default();
        for at in 0..=(KEPT as u64) {
            list.joined(&url(&format!("https://relay.example.com/room{at}")), at);
        }
        assert_eq!(list.rooms().len(), KEPT);
        assert!(!keys(&list).contains(&"room0".to_string()));
    }

    #[test]
    fn unpinning_past_the_cap_drops_the_oldest_and_remove_removes() {
        let mut list = RecentRooms::default();
        list.joined(&url("https://relay.example.com/old"), 0);
        let old = list.rooms()[0].key();
        list.set_pinned(&old, true);
        for at in 1..=(KEPT as u64) {
            list.joined(&url(&format!("https://relay.example.com/room{at}")), at);
        }
        list.set_pinned(&old, false);
        assert_eq!(list.rooms().len(), KEPT);
        assert!(!keys(&list).contains(&"old".to_string()));

        let first = list.rooms()[0].key();
        list.remove(&first);
        assert_eq!(list.rooms().len(), KEPT - 1);
        assert!(list.rooms().iter().all(|room| room.key() != first));
    }

    #[test]
    fn names_and_hosts() {
        let mut list = RecentRooms::default();
        list.joined(&url("http://localhost:4443/team/standup"), 0);
        assert_eq!(list.rooms()[0].name(), "team/standup");
        assert_eq!(list.rooms()[0].host(), "localhost:4443");

        let mut list = RecentRooms::default();
        list.joined(&url("https://relay.example.com/"), 0);
        assert_eq!(list.rooms()[0].name(), "relay.example.com", "an empty path is named by its relay");

        let key = "a".repeat(64);
        let mut list = RecentRooms::default();
        list.joined(&url(&format!("iroh://{key}/team")), 0);
        assert_eq!(list.rooms()[0].host(), "a".repeat(12));
    }

    #[test]
    fn the_token_is_in_no_display_or_log_string() {
        let mut list = RecentRooms::default();
        list.joined(&url(&format!("https://relay.example.com:4443/team?jwt={TOKEN}")), 0);
        let room = &list.rooms()[0];
        assert!(room.link().contains(TOKEN), "the link itself keeps it, for rejoining");
        for shown in [room.name(), room.host(), room.log_form(), room.key(), format!("{room:?}"), format!("{list:?}")] {
            assert!(!shown.contains(TOKEN), "{shown} carries the token");
            assert!(!shown.contains("secret-payload"), "{shown} carries part of the token");
        }
    }

    #[test]
    fn relative_times() {
        let now = 1_000_000_000;
        assert_eq!(relative(now, now), "just now");
        assert_eq!(relative(now - 59, now), "just now");
        assert_eq!(relative(now + 500, now), "just now", "a clock moved back");
        assert_eq!(relative(now - 5 * 60, now), "5 min ago");
        assert_eq!(relative(now - 3600, now), "1 hour ago");
        assert_eq!(relative(now - 5 * 3600, now), "5 hours ago");
        assert_eq!(relative(now - 30 * 3600, now), "yesterday");
        assert_eq!(relative(now - 3 * 86_400, now), "3 days ago");
        assert_eq!(relative(now - 7 * 86_400, now), "2001-09-02");
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(1_700_000_000), "2023-11-14");
        assert_eq!(date(1_798_675_200), "2026-12-31");
    }

    #[test]
    fn a_saved_list_comes_back_and_a_missing_or_corrupt_file_is_empty() {
        let path = temp("roundtrip");
        assert_eq!(RecentRooms::load_from(&path), RecentRooms::default(), "missing");

        let mut list = RecentRooms::default();
        list.joined(&url(&format!("https://relay.example.com/a?jwt={TOKEN}")), 1);
        list.joined(&url("https://relay.example.com/b"), 2);
        let first = list.rooms()[1].key();
        list.set_pinned(&first, true);
        list.write_to(&path);
        assert_eq!(RecentRooms::load_from(&path), list);

        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(RecentRooms::load_from(&path), RecentRooms::default(), "corrupt");

        std::fs::write(&path, r#"{"rooms":[{"link":"ftp://x/y","last_joined":1},{"link":"https://relay.example.com/ok","last_joined":2}]}"#)
            .unwrap();
        let loaded = RecentRooms::load_from(&path);
        assert_eq!(keys(&loaded), ["ok"], "a bad entry costs only itself");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only_after_a_save_and_a_loose_one_is_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp("mode");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

        let mut list = RecentRooms::default();
        list.joined(&url(&format!("https://relay.example.com/a?jwt={TOKEN}")), 1);
        list.write_to(&path);
        assert_eq!(mode(&path), 0o600, "new file");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        list.write_to(&path);
        assert_eq!(mode(&path), 0o600, "rewritten over a loose one");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _ = RecentRooms::load_from(&path);
        assert_eq!(mode(&path), 0o600, "tightened on load");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
