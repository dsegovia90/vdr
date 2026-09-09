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
    /// Bound, and waiting for the user to dial or to choose to wait for a call.
    /// `me` is the key to hand whoever should dial us.
    Idle {
        /// Our own endpoint id.
        me: String,
    },
    /// The key typed into the connect field was not an endpoint id.
    ///
    /// Recoverable, unlike [`Failed`](Self::Failed): the endpoint is still bound
    /// and the user can simply try again.
    BadKey {
        /// What was wrong with it.
        reason: String,
    },
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
        /// The peer's endpoint id, when we know it.
        ///
        /// `None` on the accepting side: iroh exposes no client identity to a
        /// server, so `--listen` genuinely does not learn who called until
        /// something above the transport says so. Naming it anyway would be
        /// inventing information.
        peer: Option<String>,
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
        matches!(self, Self::Failed { .. } | Self::BadKey { .. })
    }

    /// Whether the user still has a decision to make.
    ///
    /// A mistyped key leaves the endpoint bound and the choice open, so it
    /// counts as waiting rather than as an ending.
    pub fn is_waiting(&self) -> bool {
        matches!(self, Self::Idle { .. } | Self::BadKey { .. })
    }

    /// Whether the choice already made can still be taken back.
    ///
    /// Only *before* a peer is on the other end. Once
    /// [`Connected`](Self::Connected), stopping is a hang-up: the peer has to be
    /// told, and there is no channel to tell them on — that is ticket 29, and
    /// deliberately not this.
    ///
    /// [`AwaitingCamera`](Self::AwaitingCamera) counts, for two reasons. It is
    /// the state between choosing and listening — a second or two of camera wait
    /// and encoder probe — and leaving it out made the controls vanish and come
    /// back, which read as a flash. It is also **genuinely** cancellable: a
    /// cancel sent during it waits in the decision channel and is taken by the
    /// `select!` as soon as that is reached, so the button is not a decoration.
    pub fn is_cancellable(&self) -> bool {
        matches!(
            self,
            Self::AwaitingCamera | Self::Listening { .. } | Self::Dialing { .. }
        )
    }
}

impl fmt::Display for RemoteStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loopback => write!(f, "Loopback: your own camera, encoded and decoded"),
            Self::Binding => write!(f, "Starting the endpoint..."),
            Self::Idle { me } => write!(f, "Ready. Your key: {me}"),
            Self::BadKey { reason } => write!(f, "That is not a key: {reason}"),
            Self::AwaitingCamera => write!(f, "Waiting for the camera..."),
            Self::Listening { me } => write!(f, "Waiting for a call. Your key: {me}"),
            Self::Dialing { peer } => write!(f, "Dialing {peer}..."),
            Self::Connected { peer: Some(peer) } => write!(f, "Connected to {peer}"),
            Self::Connected { peer: None } => write!(f, "Connected"),
            Self::Ended => write!(f, "Call ended"),
            Self::Failed { error } => write!(f, "Call failed: {error}"),
        }
    }
}
