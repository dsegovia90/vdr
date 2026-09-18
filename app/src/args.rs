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
//! vdr --quality high                      # 720p rung; low | mid | high, mid by default
//! vdr --ephemeral                         # a throwaway key, not the saved one
//! vdr --encode-width 1920                 # encode at capture size, for A/B tests
//! ```
//!
//! **The command line is not going away.** In-app key entry (19c) is a second
//! way in, not a replacement: driving two instances from a script is how the
//! network path gets tested, and a headless session has no other way to do it.
//!
//! **The endpoint id is stable across launches on its own now**, because the
//! secret is kept in a file beside the settings — a key someone wrote down last
//! week still dials. `--secret` overrides that file, and `--ephemeral` skips it
//! for a throwaway key.
//!
//! Two instances on one machine therefore need one of those two, or they are
//! literally the same endpoint. `--ephemeral` is the easy one.
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

/// How big we encode, as a rung rather than a number.
///
/// Named by height, because that is what "360p" means and because it keeps a
/// 4:3 camera sensible: a tier caps the lines and the width follows whatever
/// the camera's aspect is.
///
/// **A ceiling, never a target.** The scaler takes integer factors only, so a
/// capture that cannot divide onto a rung exactly lands under it rather than
/// over. The two captures that actually happen both divide cleanly onto the
/// lower two rungs -- 1080/3 and 720/2 are both 360 -- which is why the default
/// is the one rung everybody can hit exactly.
///
/// It is also not a floor: rate control can only move the encoder *down* from
/// the bitrate it opened at, so a poor link still spends less than its tier.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Quality {
    /// 180 lines: 320x180 from a 16:9 camera, about 86 kbps.
    ///
    /// 180 rather than 270 for one reason worth writing down: 270 divides
    /// cleanly out of 1080p but not out of 720p, so it would be the one rung
    /// that meant something different on each platform.
    Low,
    /// 360 lines, the default: 640x360, about 346 kbps.
    #[default]
    Mid,
    /// 720 lines: 1280x720, about 1.4 Mbps.
    ///
    /// **The one rung that is not the same everywhere.** 1080/720 is 1.5 and
    /// the scaler takes integer factors, so a camera that hands us 1080p --
    /// which is every macOS camera, whatever resolution is requested of it --
    /// lands at 960x540 here. Fixing that means the capture honouring the size
    /// we ask for, not a resampler.
    High,
}

impl Quality {
    /// The tallest frame this tier encodes.
    pub fn height(self) -> u32 {
        match self {
            Self::Low => 180,
            Self::Mid => 360,
            Self::High => 720,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.to_lowercase().as_str() {
            "low" => Some(Self::Low),
            "mid" | "medium" => Some(Self::Mid),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

impl std::fmt::Display for Quality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Low => "low",
            Self::Mid => "mid",
            Self::High => "high",
        };
        f.write_str(name)
    }
}

/// Everything this app takes from the command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    /// Who to connect to, if anyone.
    pub peering: Peering,
    /// Case-insensitive substring of the camera to start on.
    pub camera: Option<String>,
    /// Which rung the encoder runs at. Everyone defaults to the same one.
    pub quality: Quality,
    /// Start with a throwaway key instead of the saved one.
    ///
    /// The identity is normally persisted, so our key is an address that keeps
    /// working. Two instances on one machine would then be the *same* endpoint,
    /// which is the project's own test loop — so the second one runs with this.
    pub ephemeral: bool,
    /// Widest the encoder may run, overriding the tier.
    ///
    /// For comparing sizes on a real link, which is the only place the
    /// difference shows: locally there is nothing to run out of, so every size
    /// looks fine. The bitrate follows the size, so this moves both.
    pub encode_width: Option<u32>,
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
        let mut quality = Quality::default();
        let mut ephemeral = false;
        let mut encode_width = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--listen" => peering = Peering::Listen,
                "--loopback" => peering = Peering::Loopback,
                "--ephemeral" => ephemeral = true,
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
                "--quality" => match args.next().as_deref().map(Quality::parse) {
                    Some(Some(chosen)) => quality = chosen,
                    Some(None) => tracing::error!("--quality is one of low, mid, high"),
                    None => tracing::error!("--quality needs a level"),
                },
                "--encode-width" => match args.next().as_deref().map(str::parse::<u32>) {
                    Some(Ok(width)) if width >= 2 => encode_width = Some(width),
                    Some(Ok(width)) => tracing::error!("--encode-width {width} is too small"),
                    Some(Err(error)) => tracing::error!("--encode-width is not a number: {error}"),
                    None => tracing::error!("--encode-width needs a pixel count"),
                },
                other => tracing::warn!("ignoring unknown argument {other}"),
            }
        }
        Self {
            peering,
            camera,
            quality,
            ephemeral,
            encode_width,
        }
    }
}
