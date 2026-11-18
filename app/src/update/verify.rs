//! Whether a downloaded package is one we published. Pure: bytes in, a
//! verdict out.
//!
//! **The chain.** CI signs `SHA256SUMS` with the release key
//! (`openssl pkeyutl -sign -rawin`, plain ed25519 over the file's exact
//! bytes, a raw 64-byte signature in `SHA256SUMS.sig`). The app checks that
//! signature with the key compiled in below, then looks up the package's own
//! line in the list and compares its SHA-256 with the file's. Nothing is
//! unpacked or run before all of it passes.
//!
//! **Replay.** A validly signed list from an *older* release is still
//! validly signed. What stops it being served as a newer one is that the
//! package's name carries its tag: the name we look up is built from the tag
//! being installed, and that tag must be newer than the running version, so
//! an old list never has the line we ask for.

use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use sha2::{Digest, Sha256};

use super::release::{self, Platform};

/// The release signing key's public half.
///
/// **Never change this casually.** Every installed copy trusts exactly this
/// key and no other: a release signed with a new one is refused by all of
/// them, and the only way to move them to a new key is a release signed with
/// this one that carries the new key.
const PRODUCTION_KEY: [u8; 32] = hex32("3037bc237610cf020580fdd0851508f79935c31056187e56167d70b3a5bfa191");

/// A key for testing the whole path against a feed of our own, set at build
/// time (`VDR_UPDATE_TEST_PUBKEY=<64 hex> dx build ...`). Release builds do
/// not set it, so in them this is `None` and the override does not exist.
/// Malformed hex fails the build rather than falling back to anything.
const TEST_KEY: Option<[u8; 32]> = match option_env!("VDR_UPDATE_TEST_PUBKEY") {
    Some(hex) => Some(hex32(hex)),
    None => None,
};

/// Whether this build trusts a test key instead of the release key. Only
/// such a build reads `VDR_UPDATE_FEED`.
pub const fn is_test_build() -> bool {
    TEST_KEY.is_some()
}

/// The key a release must be signed with in this build.
pub const fn trusted_key() -> [u8; 32] {
    match TEST_KEY {
        Some(key) => key,
        None => PRODUCTION_KEY,
    }
}

/// 64 hex characters as 32 bytes, at compile time: a typo is a build error.
const fn hex32(hex: &str) -> [u8; 32] {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => panic!("not a hex digit"),
        }
    }
    let bytes = hex.as_bytes();
    assert!(bytes.len() == 64, "a key is 64 hex characters");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = nibble(bytes[2 * i]) << 4 | nibble(bytes[2 * i + 1]);
        i += 1;
    }
    out
}

/// Why a package was refused. Each says what failed, never how to get round
/// it.
#[derive(Clone, Debug, PartialEq)]
pub enum Refused {
    /// The signature is not 64 bytes.
    SignatureShape,
    /// The key itself is not a valid ed25519 point. Only a broken build.
    Key,
    /// The signature does not match the list under our key.
    Signature,
    /// The list is not text, or has no line for the package.
    NotListed(String),
    /// The list names the package twice, differently.
    Ambiguous(String),
    /// The file's hash is not the one listed.
    Checksum,
    /// The package's name is not the one this tag and platform call for.
    WrongName { expected: String, got: String },
    /// The tag is not newer than what is running.
    NotNewer(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SignatureShape => write!(f, "the signature file is not a signature"),
            Self::Key => write!(f, "this build's update key is not valid"),
            Self::Signature => write!(f, "the release's signature does not match"),
            Self::NotListed(name) => write!(f, "{name} is not in the signed checksums"),
            Self::Ambiguous(name) => write!(f, "{name} is listed twice in the signed checksums"),
            Self::Checksum => write!(f, "the download does not match its signed checksum"),
            Self::WrongName { expected, got } => write!(f, "expected {expected}, got {got}"),
            Self::NotNewer(tag) => write!(f, "{tag} is not newer than this version"),
        }
    }
}

/// Checks `signature` over the exact bytes of `sums`.
///
/// `verify_strict`: it refuses the small-order keys and non-canonical
/// encodings plain `verify` lets through. An honest openssl signature passes
/// either.
pub fn signed(sums: &[u8], signature: &[u8], key: &[u8; 32]) -> Result<(), Refused> {
    let signature: [u8; 64] = signature.try_into().map_err(|_| Refused::SignatureShape)?;
    let key = VerifyingKey::from_bytes(key).map_err(|_| Refused::Key)?;
    key.verify_strict(sums, &Signature::from_bytes(&signature))
        .map_err(|_| Refused::Signature)
}

/// The SHA-256 the list gives for `name`, in `sha256sum`'s format:
/// `<64 hex>  <name>`, or `<64 hex> *<name>` in its binary mode.
pub fn listed(sums: &[u8], name: &str) -> Result<[u8; 32], Refused> {
    let text = std::str::from_utf8(sums).map_err(|_| Refused::NotListed(name.to_string()))?;
    let mut found: Option<[u8; 32]> = None;
    for line in text.lines() {
        let Some((hex, rest)) = line.split_once(' ') else { continue };
        let listed_name = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('*'));
        if listed_name != Some(name) {
            continue;
        }
        let Some(digest) = parse_hex32(hex) else { continue };
        match found {
            Some(earlier) if earlier != digest => return Err(Refused::Ambiguous(name.to_string())),
            _ => found = Some(digest),
        }
    }
    found.ok_or_else(|| Refused::NotListed(name.to_string()))
}

fn parse_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// SHA-256 of bytes in memory. The download hashes as it writes instead.
#[cfg(test)]
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// A running hash, for a file arriving in pieces.
#[derive(Default)]
pub struct Hasher(Sha256);

impl Hasher {
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

/// Everything that must hold before a package is opened.
pub struct Claim<'a> {
    /// The tag being installed.
    pub tag: &'a str,
    /// What is running now.
    pub running: &'a Version,
    pub platform: Platform,
    /// The downloaded package's name, and its SHA-256 as downloaded.
    pub name: &'a str,
    pub digest: [u8; 32],
    /// `SHA256SUMS` and `SHA256SUMS.sig`, as downloaded.
    pub sums: &'a [u8],
    pub signature: &'a [u8],
}

/// The whole check, in the order that fails cheapest first.
pub fn package(claim: &Claim, key: &[u8; 32]) -> Result<(), Refused> {
    let newer = release::version_of(claim.tag).is_some_and(|version| release::is_update(claim.running, &version));
    if !newer {
        return Err(Refused::NotNewer(claim.tag.to_string()));
    }
    let expected = release::package_name(claim.platform, claim.tag).unwrap_or_default();
    if claim.name != expected {
        return Err(Refused::WrongName {
            expected,
            got: claim.name.to_string(),
        });
    }
    signed(claim.sums, claim.signature, key)?;
    if listed(claim.sums, &expected)? != claim.digest {
        return Err(Refused::Checksum);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    /// A test keypair, from a fixed seed: no randomness needed, and the
    /// failures are reproducible.
    fn keypair(seed: u8) -> (SigningKey, [u8; 32]) {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let public = signing.verifying_key().to_bytes();
        (signing, public)
    }

    const MAC: &str = "vdr-macos-universal-v0.9.0.zip";
    const LINUX: &str = "vdr-linux-x86_64-v0.9.0.tar.gz";

    /// A release's worth of signed files, as CI would make them.
    struct Fixture {
        archive: Vec<u8>,
        sums: Vec<u8>,
        signature: Vec<u8>,
        key: [u8; 32],
    }

    fn fixture() -> Fixture {
        let archive = b"pretend this is a zip".to_vec();
        let other = b"pretend this is a tarball".to_vec();
        let hex = |bytes: &[u8]| sha256(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
        let sums = format!("{}  {LINUX}\n{}  {MAC}\n", hex(&other), hex(&archive)).into_bytes();
        let (signing, key) = keypair(7);
        let signature = signing.sign(&sums).to_bytes().to_vec();
        Fixture { archive, sums, signature, key }
    }

    fn claim<'a>(f: &'a Fixture, running: &'a Version) -> Claim<'a> {
        Claim {
            tag: "v0.9.0",
            running,
            platform: Platform::MacOs,
            name: MAC,
            digest: sha256(&f.archive),
            sums: &f.sums,
            signature: &f.signature,
        }
    }

    fn running() -> Version {
        Version::new(0, 1, 0)
    }

    #[test]
    fn the_production_key_is_the_published_one() {
        let hex: String = PRODUCTION_KEY.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "3037bc237610cf020580fdd0851508f79935c31056187e56167d70b3a5bfa191");
        assert!(VerifyingKey::from_bytes(&PRODUCTION_KEY).is_ok(), "a valid point");
    }

    #[test]
    fn a_good_signature_and_checksum_pass() {
        let f = fixture();
        let running = running();
        assert_eq!(package(&claim(&f, &running), &f.key), Ok(()));
    }

    #[test]
    fn a_tampered_list_fails() {
        let mut f = fixture();
        // One character of the list changed, the signature as it was.
        let last = f.sums.len() - 2;
        f.sums[last] ^= 1;
        assert_eq!(signed(&f.sums, &f.signature, &f.key), Err(Refused::Signature));
        let running = running();
        assert!(package(&claim(&f, &running), &f.key).is_err());
    }

    #[test]
    fn the_wrong_key_fails() {
        let f = fixture();
        let (_, other) = keypair(8);
        assert_eq!(signed(&f.sums, &f.signature, &other), Err(Refused::Signature));
        assert_eq!(signed(&f.sums, &f.signature, &PRODUCTION_KEY), Err(Refused::Signature));
    }

    #[test]
    fn a_truncated_or_padded_signature_fails() {
        let f = fixture();
        assert_eq!(signed(&f.sums, &f.signature[..63], &f.key), Err(Refused::SignatureShape));
        assert_eq!(signed(&f.sums, &[], &f.key), Err(Refused::SignatureShape));
        let mut long = f.signature.clone();
        long.push(0);
        assert_eq!(signed(&f.sums, &long, &f.key), Err(Refused::SignatureShape));
        // Hex or base64 text in the file instead of raw bytes is refused too.
        let text: String = f.signature.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(signed(&f.sums, text.as_bytes(), &f.key), Err(Refused::SignatureShape));
    }

    #[test]
    fn a_checksum_mismatch_fails() {
        let f = fixture();
        let running = running();
        let mut c = claim(&f, &running);
        c.digest = sha256(b"something else");
        assert_eq!(package(&c, &f.key), Err(Refused::Checksum));
    }

    #[test]
    fn an_older_signed_release_cannot_be_replayed_as_a_newer_one() {
        // The fixture is a genuine, signed v0.9.0. Served under the tag
        // v1.0.0, the name we look up is v1.0.0's, which the old list lacks.
        let f = fixture();
        let running = running();
        let mut c = claim(&f, &running);
        c.tag = "v1.0.0";
        assert!(matches!(package(&c, &f.key), Err(Refused::WrongName { .. })));
        // And named honestly, the v1.0.0 line is not in the v0.9.0 list.
        c.name = "vdr-macos-universal-v1.0.0.zip";
        assert_eq!(package(&c, &f.key), Err(Refused::NotListed(c.name.to_string())));
    }

    #[test]
    fn a_release_that_is_not_newer_is_refused_even_when_signed() {
        let f = fixture();
        let running = Version::new(0, 9, 0);
        assert_eq!(package(&claim(&f, &running), &f.key), Err(Refused::NotNewer("v0.9.0".into())));
        let running = Version::new(1, 0, 0);
        assert!(matches!(package(&claim(&f, &running), &f.key), Err(Refused::NotNewer(_))));
    }

    #[test]
    fn another_platforms_package_is_refused() {
        let f = fixture();
        let running = running();
        let mut c = claim(&f, &running);
        c.platform = Platform::LinuxX86_64;
        assert!(matches!(package(&c, &f.key), Err(Refused::WrongName { .. })));
        c.platform = Platform::Other;
        assert!(package(&c, &f.key).is_err());
    }

    #[test]
    fn the_list_is_read_as_sha256sum_writes_it() {
        let digest = [0xab; 32];
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(listed(format!("{hex}  {MAC}\n").as_bytes(), MAC), Ok(digest));
        assert_eq!(listed(format!("{hex} *{MAC}\n").as_bytes(), MAC), Ok(digest), "binary mode");
        assert_eq!(listed(format!("{}  {MAC}", hex.to_uppercase()).as_bytes(), MAC), Ok(digest));
        // A name that only contains ours, or a line with a short hash, is not ours.
        assert!(listed(format!("{hex}  x{MAC}\n").as_bytes(), MAC).is_err());
        assert!(listed(format!("{hex}  {MAC}.old\n").as_bytes(), MAC).is_err());
        assert!(listed(format!("{}  {MAC}\n", &hex[..62]).as_bytes(), MAC).is_err());
        assert!(listed(b"\xff\xfe", MAC).is_err());
        let other: String = [0xcd_u8; 32].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            listed(format!("{hex}  {MAC}\n{other}  {MAC}\n").as_bytes(), MAC),
            Err(Refused::Ambiguous(MAC.into()))
        );
        assert_eq!(listed(format!("{hex}  {MAC}\n{hex}  {MAC}\n").as_bytes(), MAC), Ok(digest));
    }

    #[test]
    fn the_running_hash_matches_the_one_shot() {
        let mut hasher = Hasher::default();
        hasher.update(b"pretend ");
        hasher.update(b"this is a zip");
        assert_eq!(hasher.finish(), sha256(b"pretend this is a zip"));
    }
}
