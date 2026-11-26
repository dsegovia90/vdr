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
    /// A destination is chosen and the call's own broadcasts are being set up,
    /// before listening, dialing or joining. Brief: it no longer waits on the
    /// camera, whose video joins the call whenever its first frame arrives.
    Preparing,
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
    /// Connecting to a relay to join a room.
    Joining {
        /// Which room, and on which relay.
        room: String,
    },
    /// In a room on a relay, whoever else is or is not there.
    ///
    /// Its own state rather than [`Connected`](Self::Connected), because a room
    /// is not one peer: being in it is true before anyone else arrives and
    /// stays true after they all leave, and the tiles say who is there.
    InRoom {
        /// Which room, and on which relay.
        room: String,
        /// Which ways in are up, for a join that holds several; `None` for
        /// one that holds just the one.
        via: Option<Via>,
    },
    /// Connected: the peer's catalog has arrived (their video, if any, follows).
    Connected {
        /// The peer's endpoint id.
        ///
        /// Known on both sides now: iroh exposes no client identity to a
        /// server, so a listener cannot learn it from the transport -- it reads
        /// it off the path the peer publishes under instead. `None` only where
        /// nothing has said yet.
        peer: Option<String>,
    },
    /// The session dropped and the stack is trying to get it back.
    ///
    /// Not an error: the transport redials on its own, the peer's broadcast is
    /// held open meanwhile, and a call that comes back inside the window resumes
    /// where it left off rather than starting again. It becomes
    /// [`Failed`](Self::Failed) only when the retry budget runs out.
    Reconnecting,
    /// The call ended.
    ///
    /// No longer terminal: the endpoint stays bound and our key stays the same,
    /// so the next call is a choice away rather than a restart.
    Ended,
    /// Something failed. Recoverable in the same way.
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

    /// Whether a peer is on the other end, which is what makes stopping a
    /// hang-up rather than a cancel.
    ///
    /// A room counts even when empty: leaving it is still said on the control
    /// channel, and someone may arrive between the label and the click.
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. } | Self::InRoom { .. })
    }

    /// Whether the user still has a decision to make.
    ///
    /// A mistyped key leaves the endpoint bound and the choice open, so it
    /// counts as waiting rather than as an ending.
    ///
    /// So do [`Ended`](Self::Ended) and [`Failed`](Self::Failed), which is what
    /// makes the controls come back after a call drops for good. They keep their
    /// own text while they are up, so the status line still says why.
    pub fn is_waiting(&self) -> bool {
        matches!(
            self,
            Self::Idle { .. } | Self::BadKey { .. } | Self::Ended | Self::Failed { .. }
        )
    }

    /// Whether the choice already made can still be taken back.
    ///
    /// [`Connected`](Self::Connected) counts now: stopping then is a hang-up,
    /// which needed a channel to tell the peer on, and there is one. The button
    /// says so rather than saying "cancel".
    ///
    /// [`Preparing`](Self::Preparing) counts, for two reasons. It is the state
    /// between choosing and listening, and leaving it out made the controls
    /// vanish and come back, which read as a flash. It is also **genuinely** cancellable: a
    /// cancel sent during it waits in the decision channel and is taken by the
    /// `select!` as soon as that is reached, so the button is not a decoration.
    ///
    /// [`Reconnecting`](Self::Reconnecting) counts for a simpler reason than
    /// `Connected`: there is nobody reachable to tell, so giving up on a call
    /// that is already down costs no explanation at all.
    pub fn is_cancellable(&self) -> bool {
        matches!(
            self,
            Self::Preparing
                | Self::Listening { .. }
                | Self::Dialing { .. }
                | Self::Joining { .. }
                | Self::InRoom { .. }
                | Self::Reconnecting
                | Self::Connected { .. }
        )
    }
}

/// Which ways into a room are up: the relay, how many hosts, and whether we
/// serve it ourselves. Only counts and ids-free words, so nothing secret can
/// reach the status line through it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Via {
    pub relay: bool,
    pub hosts: usize,
    pub hosting: bool,
}

/// Said after the room's name: ", on the relay and 1 host, hosting".
impl fmt::Display for Via {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hosts = match self.hosts {
            1 => "1 host".to_string(),
            n => format!("{n} hosts"),
        };
        match (self.relay, self.hosts) {
            (true, 0) => write!(f, ", on the relay")?,
            (true, _) => write!(f, ", on the relay and {hosts}")?,
            (false, 0) => write!(f, "; the relay is unreachable")?,
            (false, 1) => write!(f, ", through a host; the relay is unreachable")?,
            (false, _) => write!(f, ", through {hosts}; the relay is unreachable")?,
        }
        if self.hosting {
            write!(f, ", hosting")?;
        }
        Ok(())
    }
}

impl fmt::Display for RemoteStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loopback => write!(f, "Loopback: your own camera, encoded and decoded"),
            Self::Binding => write!(f, "Starting the endpoint..."),
            Self::Idle { me } => write!(f, "Ready. Your key: {me}"),
            Self::BadKey { reason } => write!(f, "That is not a key or a room URL: {reason}"),
            Self::Preparing => write!(f, "Starting the call..."),
            Self::Listening { me } => write!(f, "Waiting for a call. Your key: {me}"),
            Self::Dialing { peer } => write!(f, "Dialing {peer}..."),
            Self::Joining { room } => write!(f, "Joining {room}..."),
            Self::InRoom { room, via: None } => write!(f, "In {room}"),
            Self::InRoom { room, via: Some(via) } => write!(f, "In {room}{via}"),
            Self::Connected { peer: Some(peer) } => write!(f, "Connected to {peer}"),
            Self::Connected { peer: None } => write!(f, "Connected"),
            Self::Reconnecting => write!(f, "Connection lost. Reconnecting..."),
            Self::Ended => write!(f, "Call ended"),
            Self::Failed { error } => write!(f, "Call failed: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(relay: bool, hosts: usize, hosting: bool) -> String {
        RemoteStatus::InRoom {
            room: "room team on localhost".to_string(),
            via: Some(Via { relay, hosts, hosting }),
        }
        .to_string()
    }

    #[test]
    fn a_room_says_which_ways_in_are_up() {
        assert_eq!(said(true, 0, false), "In room team on localhost, on the relay");
        assert_eq!(said(true, 2, true), "In room team on localhost, on the relay and 2 hosts, hosting");
        assert_eq!(said(false, 1, false), "In room team on localhost, through a host; the relay is unreachable");
        assert_eq!(said(false, 0, true), "In room team on localhost; the relay is unreachable, hosting");
        let one = RemoteStatus::InRoom { room: "room team on localhost".to_string(), via: None };
        assert_eq!(one.to_string(), "In room team on localhost", "a single session reads as it always has");
    }
}
