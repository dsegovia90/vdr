//! Everything the command line can say, parsed once.
//!
//! **A public key, not a ticket.** iroh's own examples pass a `LiveTicket`,
//! which is a serialized endpoint id *plus* its current direct socket
//! addresses and relay url. Nothing here needs those: `EndpointId` implements
//! `Into<EndpointAddr>` with an empty address list, and an address-less dial is
//! resolved by the address lookup the `N0` endpoint preset already installs --
//! it publishes our addresses to n0's pkarr/DNS service on bind, and resolves
//! the peer's the same way, with the relay as the fallback path.
//!
//! The cost of the leaner form, stated plainly: a bare key is only dialable
//! while that lookup service is reachable. A ticket carries the addresses
//! inline and so also works on a LAN with no internet at all. If offline
//! testing ever matters, that is the reason to add addresses back -- not
//! anything about the key itself.
//!
//! ```sh
//! vdr                                     # idle: shows your key, waits for you
//! vdr --secret <hex> --listen             # prints its endpoint id and waits
//! vdr --secret <hex> --dial <peer-id>     # dials it
//! vdr --loopback                          # no network; the codec round trip
//! vdr --camera facetime                   # picks a camera by name
//! ```
//!
//! **The command line is not going away.** In-app key entry (19c) is a second
//! way in, not a replacement: driving two instances from a script is how the
//! network path gets tested, and a headless session has no other way to do it.
//!
//! `--secret` makes the endpoint id stable across launches, so the peer's id
//! can be written down once instead of copied every run.
//!
//! `--camera` is for pointing two instances on one machine at different
//! devices, so it is obvious on screen which window is which. macOS does let
//! several processes open the same camera at once, so this is convenience, not
//! necessity. It matches on the device's name rather than its position, for
//! the reason task 17 gives -- enumeration order is not stable across launches,
//! so an index would select a different camera on different runs.

use std::sync::OnceLock;

use iroh::EndpointId;

/// Who this instance is trying to reach.
#[derive(Clone, Debug, PartialEq)]
pub enum Peering {
    /// No decision yet: bind an endpoint, show our key, and wait for the user
    /// to dial someone or choose to wait for a call. The default (ticket 19c).
    Idle,
    /// No peer: the second tile shows our own camera round-tripped through the
    /// codec instead. See [`super::loopback`].
    ///
    /// Was the default until 19c. Kept behind `--loopback` because it is the
    /// diagnostic that tells a broken pipeline apart from a broken connection --
    /// 20d-1 was measured on it.
    Loopback,
    /// Wait for someone to dial us.
    Listen,
    /// Dial this peer by public key.
    Dial(EndpointId),
}

/// Everything this app takes from the command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    /// Who to connect to, if anyone.
    pub peering: Peering,
    /// Case-insensitive substring of the camera to start on.
    pub camera: Option<String>,
}

impl Args {
    /// Parses the command line once, and installs `--secret` in the
    /// environment.
    ///
    /// `--secret` is forwarded into `IROH_SECRET` rather than parsed here, and
    /// `net::call` reads it back when it builds the endpoint. That keeps one
    /// code path for the key -- the env var keeps working on its own, and there
    /// is no second place for the two to disagree.
    ///
    /// Bad input is reported and then ignored: a mistyped key should leave a
    /// video app running in loopback with a line in the log, not refuse to
    /// start.
    ///
    /// Cached, because both the camera component and the network path read it
    /// and neither should be the one that owns it.
    pub fn get() -> &'static Self {
        static ARGS: OnceLock<Args> = OnceLock::new();
        ARGS.get_or_init(Self::parse)
    }

    fn parse() -> Self {
        let mut args = std::env::args().skip(1);
        let mut peering = Peering::Idle;
        let mut camera = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--listen" => peering = Peering::Listen,
                "--loopback" => peering = Peering::Loopback,
                "--dial" => match args.next().as_deref().map(str::parse::<EndpointId>) {
                    Some(Ok(peer)) => peering = Peering::Dial(peer),
                    Some(Err(error)) => tracing::error!("--dial is not an endpoint id: {error}"),
                    None => tracing::error!("--dial needs an endpoint id"),
                },
                "--secret" => match args.next() {
                    // Read back by `net::call` on the call thread, which has
                    // not started yet -- this runs before the first render.
                    Some(secret) => std::env::set_var("IROH_SECRET", secret),
                    None => tracing::error!("--secret needs a hex-encoded key"),
                },
                "--camera" => match args.next() {
                    Some(wanted) => camera = Some(wanted.to_lowercase()),
                    None => tracing::error!("--camera needs part of a device name"),
                },
                other => tracing::warn!("ignoring unknown argument {other}"),
            }
        }
        Self { peering, camera }
    }
}
