//! What the second tile's stream is currently doing.

use std::fmt;

/// Lifecycle of whatever is filling the remote tile.
///
/// Deliberately one enum for both the loopback and a real call: the UI shows
/// one tile either way, and the difference between them is exactly what this
/// says. Mirrors `camera::StreamStatus` in shape -- descriptive of the
/// connection, never of what the user asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteStatus {
    /// No peer configured, so the tile shows our own camera round-tripped
    /// through encode and decode.
    Loopback,
    /// Binding the iroh endpoint.
    Binding,
    /// Endpoint is up; waiting for the camera to produce a frame, because the
    /// encoder cannot be built without one to size it from.
    AwaitingCamera,
    /// Listening for an incoming call. `me` is the key the peer needs.
    Listening {
        /// Our own endpoint id.
        me: String,
    },
    /// Dialing a peer.
    Dialing {
        /// The peer's endpoint id.
        peer: String,
    },
    /// Connected, and the peer's video is arriving.
    Connected {
        /// The peer's endpoint id.
        peer: String,
    },
    /// The call ended. Terminal: nothing here redials.
    Ended,
    /// Something failed. Terminal for the same reason.
    Failed {
        /// What went wrong, already formatted -- the transport's error types
        /// do not survive crossing to the UI thread usefully.
        error: String,
    },
}

impl RemoteStatus {
    /// Whether this state should read as a problem rather than as progress.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

impl fmt::Display for RemoteStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loopback => write!(f, "Loopback: your own camera, encoded and decoded"),
            Self::Binding => write!(f, "Starting the endpoint..."),
            Self::AwaitingCamera => write!(f, "Waiting for the camera..."),
            Self::Listening { me } => write!(f, "Waiting for a call. Your key: {me}"),
            Self::Dialing { peer } => write!(f, "Dialing {peer}..."),
            Self::Connected { peer } => write!(f, "Connected to {peer}"),
            Self::Ended => write!(f, "Call ended"),
            Self::Failed { error } => write!(f, "Call failed: {error}"),
        }
    }
}
