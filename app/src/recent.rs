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
//! **It also holds where else a room can meet.** A member who hosts the rooms
//! they join says so in presence, with a door; everyone who hears it keeps
//! that host with the room ([`Host`]), and a room joined with hosting on keeps
//! its own door, so the invites members saved keep working across launches.
//! These are secrets like the token, kept here for the day the relay is down,
//! and never displayed or logged.
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

/// How many hosts a room keeps, the most recently heard. Enough for a team
/// in which several people opted in; a host is only worth keeping while it
/// might still answer.
pub const HOSTS_KEPT: usize = 8;

/// How long a host is kept without being heard again: a month. Long enough
/// to outlast a holiday, short enough that someone who stopped hosting long
/// ago is not dialled for ever.
pub const HOST_EXPIRES: u64 = 30 * 24 * 60 * 60;

/// Somewhere else this room can meet: a member's app that hosts it.
///
/// **Kept when the host leaves or goes quiet.** Hosts exist for the day the
/// relay is down, which is exactly when nobody can hear them, so silence
/// cannot mean "forget". A host is forgotten when it says, live, that it has
/// stopped hosting, when it has not been heard for [`HOST_EXPIRES`], or when
/// newer hosts push it past [`HOSTS_KEPT`].
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Host {
    /// The host's endpoint id: what to dial.
    id: String,
    /// The door it said, presented when dialling it.
    door: String,
    /// Whose presence told us, or `None` when it came in an invite. Today a
    /// host is only ever announced by itself, so this is its own id; kept so
    /// that a host vouched for by someone else would need no new format.
    #[serde(default)]
    told_by: Option<String>,
    /// When it was last heard, in seconds since the Unix epoch.
    heard: u64,
}

/// By hand: the door must not reach a log through a stray `{:?}`.
impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("id", &crate::name::short(&self.id))
            .field("heard", &self.heard)
            .finish_non_exhaustive()
    }
}

impl Host {
    /// A host as heard in presence or read from an invite, or `None` when
    /// the id or the door is not the shape they always have.
    pub fn new(id: &str, door: &str, told_by: Option<&str>, heard: u64) -> Option<Self> {
        let told_by = match told_by {
            Some(by) if crate::net::quality::valid_id(by) => Some(by.to_string()),
            Some(_) => return None,
            None => None,
        };
        (crate::net::quality::valid_id(id) && crate::net::door::is_door(door)).then(|| Self {
            id: id.to_string(),
            door: door.to_string(),
            told_by,
            heard,
        })
    }

    /// The host's endpoint id.
    pub fn id(&self) -> &str {
        &self.id
    }

}

/// Reads one `host=` value of an invite.
fn invited(value: &str, now: u64) -> Option<Host> {
    let (id, door) = value.split_once('.')?;
    Host::new(id, door, None, now)
}

/// Splits an invite into the link to dial and the hosts it lists.
///
/// **The hosts never go to the relay.** They ride in the link as repeated
/// `host=<endpoint id>.<door>` parameters, which an older build passes to the
/// relay untouched (a relay reads only `jwt`), but this one takes them out
/// before dialling and before remembering the link. A malformed entry is
/// dropped; the rest of the link is unaffected.
pub fn split_invite(url: &url::Url, now: u64) -> (url::Url, Vec<Host>) {
    let param = crate::net::door::HOST_PARAM;
    if !url.query_pairs().any(|(key, _)| key == param) {
        return (url.clone(), Vec::new());
    }
    let mut hosts = Vec::new();
    let mut kept = Vec::new();
    for (key, value) in url.query_pairs() {
        if key == param {
            match invited(&value, now) {
                Some(host) if !hosts.iter().any(|known: &Host| known.id == host.id) => hosts.push(host),
                Some(_) => {}
                None => tracing::warn!("ignoring a host in this link that is not an id and a door"),
            }
        } else {
            kept.push((key.into_owned(), value.into_owned()));
        }
    }
    let mut clean = url.clone();
    if kept.is_empty() {
        clean.set_query(None);
    } else {
        clean.query_pairs_mut().clear().extend_pairs(kept);
    }
    (clean, hosts)
}

/// A room's own door, read from the file: anything that is not a door is
/// none, costing only itself.
fn lenient_door<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    Ok(value.as_str().filter(|door| crate::net::door::is_door(door)).map(str::to_string))
}

/// A room's hosts, read from the file one by one: an entry that does not
/// read, or whose id or door is malformed, costs only itself.
fn lenient_hosts<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Host>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    let serde_json::Value::Array(entries) = value else { return Ok(Vec::new()) };
    Ok(entries
        .into_iter()
        .filter_map(|entry| serde_json::from_value::<Host>(entry).ok())
        .filter_map(|host| Host::new(&host.id, &host.door, host.told_by.as_deref(), host.heard))
        .collect())
}

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
    /// Our own door for this room, made the first time we joined it with
    /// hosting on. Absent for a room never hosted.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient_door")]
    door: Option<String>,
    /// Where else it can meet, most recently heard first.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient_hosts")]
    hosts: Vec<Host>,
}

/// By hand, so that a stray `{:?}` cannot write the token into a log.
impl std::fmt::Debug for Room {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Room")
            .field("link", &self.log_form())
            .field("last_joined", &self.last_joined)
            .field("pinned", &self.pinned)
            .field("door", &self.door.is_some())
            .field("hosts", &self.hosts)
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

    /// Takes in a host heard now: a known one is refreshed, its door
    /// replaced if it changed; then the list is kept to [`HOSTS_KEPT`], the
    /// most recently heard.
    fn heard(&mut self, host: Host) {
        match self.hosts.iter_mut().find(|known| known.id == host.id) {
            Some(known) => {
                known.heard = known.heard.max(host.heard);
                known.door = host.door;
                if host.told_by.is_some() {
                    known.told_by = host.told_by;
                }
            }
            None => self.hosts.push(host),
        }
        self.hosts.sort_by_key(|host| std::cmp::Reverse(host.heard));
        self.hosts.truncate(HOSTS_KEPT);
    }

    /// Drops hosts not heard for [`HOST_EXPIRES`].
    fn expire(&mut self, now: u64) {
        self.hosts.retain(|host| now.saturating_sub(host.heard) < HOST_EXPIRES);
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
        // A rejoin keeps what the room has learned: its door and its hosts.
        let kept = self.rooms.iter().find(|room| room.key() == key);
        let door = kept.and_then(|room| room.door.clone());
        let hosts = kept.map(|room| room.hosts.clone()).unwrap_or_default();
        self.rooms.retain(|room| room.key() != key);
        self.rooms.push(Room {
            link: url.to_string(),
            last_joined: now,
            pinned,
            door,
            hosts,
        });
        self.tidy();
    }

    /// The door of the room with this key, if it has one.
    pub fn door_for(&self, key: &str) -> Option<String> {
        self.rooms.iter().find(|room| room.key() == key).and_then(|room| room.door.clone())
    }

    /// Gives the room `door` unless it already has one: a door handed out is
    /// never replaced, or every invite carrying it would stop working.
    pub fn keep_door(&mut self, key: &str, door: &str) {
        if !crate::net::door::is_door(door) {
            return;
        }
        for room in self.rooms.iter_mut().filter(|room| room.key() == key && room.door.is_none()) {
            room.door = Some(door.to_string());
        }
    }

    /// Records a host heard for the room with this key. Returns whether the
    /// room is in the list; one not remembered yet keeps nothing.
    pub fn heard(&mut self, key: &str, host: Host, now: u64) -> bool {
        let mut found = false;
        for room in self.rooms.iter_mut().filter(|room| room.key() == key) {
            room.heard(host.clone());
            room.expire(now);
            found = true;
        }
        found
    }

    /// Forgets a host that said it stopped hosting this room.
    pub fn forget(&mut self, key: &str, id: &str) {
        for room in self.rooms.iter_mut().filter(|room| room.key() == key) {
            room.hosts.retain(|host| host.id != id);
        }
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
        let now = now();
        for room in &mut list.rooms {
            room.expire(now);
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
    fn a_hosted_room_keeps_its_door_out_of_sight() {
        const DOOR: &str = "00112233445566778899aabbccddeeff";
        let mut list = RecentRooms::default();
        list.joined(&url(&format!("iroh://{}/team?door={DOOR}", "a".repeat(64))), 1);
        let room = &list.rooms()[0];
        for shown in [room.name(), room.host(), room.log_form(), room.key(), format!("{room:?}")] {
            assert!(!shown.contains(DOOR), "{shown}");
        }
        assert!(room.link().contains(DOOR), "the link itself keeps it, for rejoining");
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

    const DOOR: &str = "00112233445566778899aabbccddeeff";
    const OTHER_DOOR: &str = "ffeeddccbbaa99887766554433221100";

    fn hex(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    fn host(c: char, door: &str, heard: u64) -> Host {
        Host::new(&hex(c), door, Some(&hex(c)), heard).expect("a well-formed host")
    }

    fn team() -> (RecentRooms, String) {
        let mut list = RecentRooms::default();
        list.joined(&url(&format!("https://relay.example.com/team?jwt={TOKEN}")), 1);
        let key = list.rooms()[0].key();
        (list, key)
    }

    #[test]
    fn a_host_is_kept_with_its_room_refreshed_and_its_door_replaced() {
        let (mut list, key) = team();
        assert!(list.heard(&key, host('a', DOOR, 10), 10));
        assert!(list.heard(&key, host('a', OTHER_DOOR, 20), 20));
        let room = &list.rooms()[0];
        assert_eq!(room.hosts.len(), 1);
        assert_eq!(room.hosts[0].door, OTHER_DOOR);
        assert_eq!(room.hosts[0].heard, 20);
        assert!(!list.heard("https://relay.example.com:443/other", host('b', DOOR, 20), 20), "a room not remembered keeps nothing");
    }

    #[test]
    fn hosts_are_capped_by_the_most_recently_heard_and_expire() {
        let (mut list, key) = team();
        let ids = "abcdef0123";
        for (at, c) in ids.chars().enumerate() {
            list.heard(&key, host(c, DOOR, 100 + at as u64), 100 + at as u64);
        }
        let kept: Vec<String> = list.rooms()[0].hosts.iter().map(|host| host.id[..1].to_string()).collect();
        assert_eq!(kept, ["3", "2", "1", "0", "f", "e", "d", "c"], "the eight most recently heard");

        let later = 109 + HOST_EXPIRES - 1;
        list.heard(&key, host('9', DOOR, later), later);
        let kept: Vec<String> = list.rooms()[0].hosts.iter().map(|host| host.id[..1].to_string()).collect();
        assert_eq!(kept, ["9", "3"], "only the ones heard within the month");
    }

    #[test]
    fn a_host_that_stops_is_forgotten_and_a_rejoin_keeps_the_rest() {
        let (mut list, key) = team();
        list.heard(&key, host('a', DOOR, 10), 10);
        list.heard(&key, host('b', DOOR, 10), 10);
        list.forget(&key, &hex('a'));
        list.keep_door(&key, DOOR);
        list.joined(&url("https://relay.example.com/team?jwt=fresh"), 50);
        let room = &list.rooms()[0];
        assert_eq!(room.hosts.len(), 1);
        assert_eq!(room.hosts[0].id, hex('b'));
        assert_eq!(room.door.as_deref(), Some(DOOR));
    }

    #[test]
    fn a_door_handed_out_is_never_replaced() {
        let (mut list, key) = team();
        assert_eq!(list.door_for(&key), None);
        list.keep_door(&key, "not a door");
        assert_eq!(list.door_for(&key), None);
        list.keep_door(&key, DOOR);
        list.keep_door(&key, OTHER_DOOR);
        assert_eq!(list.door_for(&key).as_deref(), Some(DOOR));
    }

    #[test]
    fn a_link_with_only_hosts_loses_its_query_and_a_bad_host_costs_only_itself() {
        let a = hex('a');
        let text = format!("iroh://{b}/team?host={a}.{DOOR}&host=nonsense&host={a}.{OTHER_DOOR}&host={a}", b = hex('b'));
        let (link, hosts) = split_invite(&url(&text), 0);
        assert_eq!(link.as_str(), format!("iroh://{}/team", hex('b')));
        assert_eq!(hosts.len(), 1, "one well-formed host, once");
        assert_eq!(hosts[0].door, DOOR);
        let plain = url("https://relay.example.com/team?jwt=x");
        assert_eq!(split_invite(&plain, 0).0, plain);
    }

    #[test]
    fn a_saved_room_keeps_its_hosts_and_door_and_bad_entries_cost_only_themselves() {
        let path = temp("hosts");
        let now = now();
        let (mut list, key) = team();
        list.keep_door(&key, DOOR);
        list.heard(&key, host('b', OTHER_DOOR, now), now);
        list.write_to(&path);
        assert_eq!(RecentRooms::load_from(&path), list);

        let b = hex('b');
        std::fs::write(
            &path,
            format!(
                r#"{{"rooms":[{{"link":"https://relay.example.com/team","last_joined":1,"door":42,
                    "hosts":[{{"id":"{b}","door":"{DOOR}","heard":{now}}},{{"id":"short","door":"{DOOR}","heard":{now}}},
                             {{"id":"{b}"}},7,{{"id":"{b}","door":"{DOOR}","heard":1}}]}},
                   {{"link":"https://relay.example.com/other","last_joined":2,"hosts":"x"}}]}}"#
            ),
        )
        .unwrap();
        let loaded = RecentRooms::load_from(&path);
        assert_eq!(keys(&loaded), ["other", "team"]);
        let team = &loaded.rooms()[1];
        assert_eq!(team.door, None, "a door that is not one is none");
        assert_eq!(team.hosts.len(), 1, "one good entry; the expired and malformed ones go");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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
