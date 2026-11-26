//! The door of a room hosted here: which room it is, and the secret a guest
//! presents to come in.
//!
//! **Why a secret in the URL, and not identities.** A hosting app accepts
//! sessions over iroh, and anyone who knows its endpoint id can dial it. The
//! request's path and query reach us before we accept (moq-tokio's `Request`
//! reads them out of the SETUP, or out of the URL over HTTP/3), so a guest
//! presents `?door=<secret>` the way a relay's guest presents `?jwt=`, through
//! the same `--relay iroh://<host>/<room>` path, and needs no new code. Checking
//! endpoint ids instead would need our own iroh endpoint setup.
//!
//! **A new door every launch**, printed once to stdout and never logged. Anyone
//! holding the invite can pass it on; that is accepted for now.
//!
//! **The room is the whole origin.** A relay roots each session at the path it
//! dialled, so its guests see `<id>/camera.hang`. A host serves exactly one
//! room, so its origin is left unscoped: it *is* that room, the paths match a
//! relay's without a prefix, and the room's name only matters here, at the
//! door.
//!
//! **What the door does not check: who publishes under which id.** A guest
//! inside can publish under a path that is not their own endpoint id, and
//! everything downstream reads the id off the path. A relay room has the same
//! property: its tokens are minted with an empty publish pattern, so they
//! grant the whole room, and `participant_of` says the id is self-declared.
//! (Observed in the token script and in our code; not exercised.)
//!
//! Pure, so the check is tested without a socket.

/// The query parameter a guest's door is presented under.
pub(crate) const PARAM: &str = "door";

/// The query parameter an invite lists a room's hosts under, one
/// `<endpoint id>.<door>` each. Carries doors, so it is a secret wherever a
/// link is shown or logged.
pub(crate) const HOST_PARAM: &str = "host";

/// How many random bytes a door is: 128 bits, far past guessing over a dial
/// that costs a handshake each.
const BYTES: usize = 16;

/// A room's name, or why it cannot be hosted under it.
///
/// One plain segment, because the guest's path is compared with it as text: a
/// name that a URL would percent-encode or split could be written two ways and
/// refuse someone holding the right invite.
pub(crate) fn room_name(text: &str) -> Result<String, String> {
    let name = text.trim().trim_matches('/');
    if name.is_empty() {
        return Err("a room needs a name".to_string());
    }
    if let Some(bad) = name.chars().find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))) {
        return Err(format!("a room name is letters, digits, '-', '_' and '.', not {bad:?}"));
    }
    Ok(name.to_string())
}

/// A room hosted here, with its door.
#[derive(Clone)]
pub(super) struct Hosting {
    room: String,
    door: String,
}

/// By hand, so that a stray `{:?}` cannot write the door into a log.
impl std::fmt::Debug for Hosting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hosting").field("room", &self.room).finish_non_exhaustive()
    }
}

/// Why a guest was turned away. Says which check failed, never what was
/// presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// They asked for a room this app is not hosting.
    WrongRoom,
    /// They presented no door.
    NoDoor,
    /// They presented one, and it is not this room's.
    WrongDoor,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WrongRoom => "asked for another room",
            Self::NoDoor => "presented no door",
            Self::WrongDoor => "presented the wrong door",
        })
    }
}

impl Hosting {
    /// Hosts `room` behind a fresh random door.
    ///
    /// From the crypto provider moq-tokio already builds its TLS on, rather
    /// than a dependency of our own for sixteen bytes.
    pub(super) fn open(room: String) -> Result<Self, String> {
        Ok(Self::with_door(room, fresh()?))
    }

    /// Hosts `room` behind a door it already has, so that an invite saved by
    /// a member keeps working across our launches.
    ///
    /// `room` is a relay room's path as the URL writes it, which may be
    /// several segments: guests dial us with that same path, written by the
    /// same URL parser, so the comparison in [`Self::admits`] still holds.
    pub(super) fn with_door(room: String, door: String) -> Self {
        Self { room, door }
    }

    /// What this room's host says in presence: the door and our origin's hop.
    pub(super) fn offer(&self, hop: u64) -> Offer {
        Offer {
            door: self.door.clone(),
            hop,
        }
    }

    pub(super) fn room(&self) -> &str {
        &self.room
    }

    /// The link a guest joins with. Carries the door: for the person hosting,
    /// never for a log.
    pub(super) fn invite(&self, me: &str) -> String {
        format!("iroh://{me}/{}?{PARAM}={}", self.room, self.door)
    }

    /// Whether a request for `path` with `query` may come in.
    pub(super) fn admits(&self, path: &str, query: Option<&str>) -> Result<(), Refusal> {
        if path.trim_matches('/') != self.room {
            return Err(Refusal::WrongRoom);
        }
        let presented = query
            .into_iter()
            .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
            .find(|(key, _)| key == PARAM)
            .map(|(_, value)| value.into_owned())
            .ok_or(Refusal::NoDoor)?;
        if same(presented.as_bytes(), self.door.as_bytes()) {
            Ok(())
        } else {
            Err(Refusal::WrongDoor)
        }
    }
}

/// A fresh random door.
///
/// From the crypto provider moq-tokio already builds its TLS on, rather than a
/// dependency of our own for sixteen bytes.
pub(crate) fn fresh() -> Result<String, String> {
    let mut bytes = [0u8; BYTES];
    rustls::crypto::aws_lc_rs::default_provider()
        .secure_random
        .fill(&mut bytes)
        .map_err(|_| "could not make a door: no random source".to_string())?;
    Ok(hex(&bytes))
}

/// Whether `text` is a door as [`fresh`] makes them. A door off the wire or
/// out of a file is checked against this, so nothing else is ever dialled
/// with or written down as one.
pub(crate) fn is_door(text: &str) -> bool {
    text.len() == BYTES * 2 && text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// What a member hosting the room says about it in their presence: the door
/// to present, and the hop their origin stamps on what it forwards.
///
/// **The hop is per attempt** (a fresh random one each time the call starts),
/// so it is said, never saved: it is how a viewer can later tell a broadcast
/// arriving through this host from one arriving through the relay. The door
/// is saved by everyone who hears it, with the room.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Offer {
    pub(crate) door: String,
    pub(crate) hop: u64,
}

/// By hand, so that a stray `{:?}` of a presence document cannot write the
/// door into a log.
impl std::fmt::Debug for Offer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offer").field("hop", &self.hop).finish_non_exhaustive()
    }
}

impl Offer {
    /// The offer as we send or keep it, or `None` when it is not one: a door
    /// of the wrong shape, or a hop no origin could have (zero names nobody,
    /// and a generated hop stays under 2^53).
    pub(crate) fn clean(self) -> Option<Self> {
        (is_door(&self.door) && self.hop != 0 && self.hop < 1 << 53).then_some(self)
    }
}

/// `host` as it comes off the wire: anything that does not read as an offer
/// is no offer, never a failed document, because one parse error ends
/// listening to that peer. Through a `Value`, so no serde message can quote
/// what was sent.
pub(crate) fn lenient_offer<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Offer>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    Ok(serde_json::from_value::<Offer>(value).ok().and_then(Offer::clean))
}

/// The doors this run has issued for the rooms it joins while hosting is on,
/// keyed by a room's identity (`recent::identity`).
///
/// **The interface issues them; the call reads them.** The interface owns
/// `rooms.json`, where a room's door is kept so that the invites members
/// saved keep working across launches, and it is the only writer of that
/// file. It puts the door here before it sends the decision to join, so the
/// call finds it waiting.
///
/// **It also carries the room's hosts the same way**: the ones `rooms.json`
/// keeps for it and the ones its invite listed, put here by the interface
/// with the decision, so a join can dial them beside the relay without the
/// call ever reading the file.
#[derive(Default)]
pub(crate) struct Doors {
    ours: std::sync::Mutex<std::collections::HashMap<String, String>>,
    hosts: std::sync::Mutex<std::collections::HashMap<String, Vec<Known>>>,
}

/// A host the room can meet at: its endpoint id and the door to present.
#[derive(Clone, PartialEq)]
pub(crate) struct Known {
    pub(crate) id: String,
    pub(crate) door: String,
}

/// By hand: the door must not reach a log through a stray `{:?}`.
impl std::fmt::Debug for Known {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Known").field("id", &self.id).finish_non_exhaustive()
    }
}

/// By hand: the values are doors.
impl std::fmt::Debug for Doors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Doors").finish_non_exhaustive()
    }
}

impl Doors {
    pub(crate) fn issue(&self, room: String, door: String) {
        if let Ok(mut doors) = self.ours.lock() {
            doors.insert(room, door);
        }
    }

    /// Takes back a room's door: joined with hosting off, it must not be
    /// kept with the room, and the call will not serve it.
    pub(crate) fn withdraw(&self, room: &str) {
        if let Ok(mut doors) = self.ours.lock() {
            doors.remove(room);
        }
    }

    pub(crate) fn get(&self, room: &str) -> Option<String> {
        self.ours.lock().ok().and_then(|doors| doors.get(room).cloned())
    }

    /// Replaces what the room's hosts are, for the join about to start.
    pub(crate) fn set_hosts(&self, room: String, hosts: Vec<Known>) {
        if let Ok(mut known) = self.hosts.lock() {
            known.insert(room, hosts);
        }
    }

    /// The room's hosts, as the interface last put them here.
    pub(crate) fn hosts(&self, room: &str) -> Vec<Known> {
        self.hosts.lock().ok().and_then(|known| known.get(room).cloned()).unwrap_or_default()
    }
}

/// Byte-for-byte equality that takes as long whichever byte differs, so the
/// time a refusal takes says nothing about how much of a guess was right. The
/// length is not secret: every door is the same length.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |differ, (x, y)| differ | (x ^ y)) == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOOR: &str = "00112233445566778899aabbccddeeff";

    fn team() -> Hosting {
        Hosting::with_door("team".to_string(), DOOR.to_string())
    }

    #[test]
    fn the_right_room_and_door_come_in() {
        assert_eq!(team().admits("/team", Some(&format!("door={DOOR}"))), Ok(()));
        // However the path was written, and among other parameters.
        assert_eq!(team().admits("team/", Some(&format!("x=1&door={DOOR}&y=2"))), Ok(()));
    }

    #[test]
    fn another_room_is_refused_before_the_door_is_looked_at() {
        assert_eq!(team().admits("/other", Some(&format!("door={DOOR}"))), Err(Refusal::WrongRoom));
        assert_eq!(team().admits("", Some(&format!("door={DOOR}"))), Err(Refusal::WrongRoom));
        assert_eq!(team().admits("/team/extra", Some(&format!("door={DOOR}"))), Err(Refusal::WrongRoom));
    }

    #[test]
    fn no_door_is_refused() {
        assert_eq!(team().admits("/team", None), Err(Refusal::NoDoor));
        assert_eq!(team().admits("/team", Some("")), Err(Refusal::NoDoor));
        assert_eq!(team().admits("/team", Some(&format!("jwt={DOOR}"))), Err(Refusal::NoDoor));
    }

    #[test]
    fn a_wrong_door_is_refused() {
        let wrong = DOOR.replace('f', "e");
        assert_eq!(team().admits("/team", Some(&format!("door={wrong}"))), Err(Refusal::WrongDoor));
        assert_eq!(team().admits("/team", Some(&format!("door={}", &DOOR[1..]))), Err(Refusal::WrongDoor));
        assert_eq!(team().admits("/team", Some("door=")), Err(Refusal::WrongDoor));
    }

    #[test]
    fn equality_is_exact() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"ab"));
        assert!(same(b"", b""));
    }

    #[test]
    fn a_door_is_random_hex_of_a_fixed_length() {
        let one = Hosting::open("team".to_string()).unwrap();
        let two = Hosting::open("team".to_string()).unwrap();
        assert_eq!(one.door.len(), BYTES * 2);
        assert!(one.door.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(one.door, two.door);
    }

    #[test]
    fn the_invite_is_what_the_door_admits() {
        let host = team();
        let invite: url::Url = host.invite(&"a".repeat(64)).parse().unwrap();
        assert_eq!(host.admits(invite.path(), invite.query()), Ok(()));
        assert_eq!(crate::args::relay_url(invite.as_str()), Ok(invite));
    }

    #[test]
    fn the_door_stays_out_of_debug_output() {
        let shown = format!("{:?}", team());
        assert!(!shown.contains(DOOR), "{shown}");
        assert!(shown.contains("team"), "{shown}");
    }

    #[test]
    fn a_room_of_several_segments_admits_its_own_path() {
        let host = Hosting::with_door("team/standup".to_string(), DOOR.to_string());
        let invite: url::Url = host.invite(&"a".repeat(64)).parse().unwrap();
        assert_eq!(invite.path(), "/team/standup");
        assert_eq!(host.admits(invite.path(), invite.query()), Ok(()));
        assert_eq!(host.admits("/team", invite.query()), Err(Refusal::WrongRoom));
    }

    #[test]
    fn a_fresh_door_is_a_door() {
        assert!(is_door(&fresh().unwrap()));
        assert!(is_door(DOOR));
        assert!(!is_door(&DOOR.to_uppercase()));
        assert!(!is_door(&DOOR[1..]));
        assert!(!is_door(&format!("{DOOR}0")));
        assert!(!is_door(&DOOR.replace('0', "g")));
    }

    #[test]
    fn an_offer_is_read_leniently_and_cleaned() {
        #[derive(serde::Deserialize)]
        struct Doc {
            #[serde(default, deserialize_with = "lenient_offer")]
            host: Option<Offer>,
        }
        let read = |json: &str| serde_json::from_str::<Doc>(json).expect("the document survives").host;
        assert_eq!(read(&format!(r#"{{"host":{{"door":"{DOOR}","hop":7}}}}"#)), Some(team().offer(7)));
        assert_eq!(read(&format!(r#"{{"host":{{"door":"{DOOR}","hop":7,"later":true}}}}"#)), Some(team().offer(7)));
        for bad in [
            r#"{"host":"yes"}"#.to_string(),
            r#"{"host":null}"#.to_string(),
            format!(r#"{{"host":{{"door":"{DOOR}"}}}}"#),
            format!(r#"{{"host":{{"door":"{DOOR}","hop":0}}}}"#),
            format!(r#"{{"host":{{"door":"{DOOR}","hop":9007199254740993}}}}"#),
            format!(r#"{{"host":{{"door":"{DOOR}","hop":-1}}}}"#),
            r#"{"host":{"door":"short","hop":7}}"#.to_string(),
        ] {
            assert_eq!(read(&bad), None, "{bad}");
        }
        assert_eq!(read("{}"), None);
    }

    #[test]
    fn an_offer_and_the_doors_keep_the_door_out_of_debug_output() {
        let shown = format!("{:?}", team().offer(7));
        assert!(!shown.contains(DOOR) && shown.contains('7'), "{shown}");
        let doors = Doors::default();
        doors.issue("room".to_string(), DOOR.to_string());
        assert_eq!(doors.get("room").as_deref(), Some(DOOR));
        assert!(!format!("{doors:?}").contains(DOOR));
        doors.withdraw("room");
        assert_eq!(doors.get("room"), None);
    }

    #[test]
    fn a_room_name_is_one_plain_segment() {
        assert_eq!(room_name("team"), Ok("team".to_string()));
        assert_eq!(room_name(" /team-1_a.b/ "), Ok("team-1_a.b".to_string()));
        assert!(room_name("").is_err());
        assert!(room_name("/").is_err());
        assert!(room_name("a/b").is_err());
        assert!(room_name("a b").is_err());
        assert!(room_name("a?door=x").is_err());
        assert!(room_name("caf\u{e9}").is_err());
    }
}
