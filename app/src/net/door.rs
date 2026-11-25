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
        let mut bytes = [0u8; BYTES];
        rustls::crypto::aws_lc_rs::default_provider()
            .secure_random
            .fill(&mut bytes)
            .map_err(|_| "could not make a door: no random source".to_string())?;
        Ok(Self::with_door(room, hex(&bytes)))
    }

    fn with_door(room: String, door: String) -> Self {
        Self { room, door }
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
