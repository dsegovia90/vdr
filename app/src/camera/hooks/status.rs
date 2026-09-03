//! What the camera hardware is currently doing.

use std::fmt;

/// Lifecycle of the stream driven by [`use_camera_stream`].
///
/// [`use_camera_stream`]: super::use_camera_stream
#[derive(Clone, Debug)]
pub enum StreamStatus {
    /// Nothing is open. The camera has not failed -- there is simply no source
    /// to stream, because none was found. "The user turned video off" is *not*
    /// this state: intent is held separately, outside `StreamStatus`.
    Idle,
    /// Opening the camera on a worker thread.
    Connecting {
        /// Human-readable name of the source being opened.
        label: String,
    },
    /// Camera is open and delivering frames.
    Streaming {
        /// Human-readable name of the active source.
        label: String,
    },
    /// The last open attempt failed. Match on `error` to branch on the kind --
    /// `PermissionDenied` in particular deserves its own message.
    Failed {
        /// Why the camera could not be opened.
        error: cameras::Error,
    },
}

impl PartialEq for StreamStatus {
    fn eq(&self, other: &Self) -> bool {
        // `cameras::Error` is not PartialEq, so compare its rendered form.
        match (self, other) {
            (Self::Idle, Self::Idle) => true,
            (Self::Connecting { label: a }, Self::Connecting { label: b }) => a == b,
            (Self::Streaming { label: a }, Self::Streaming { label: b }) => a == b,
            (Self::Failed { error: a }, Self::Failed { error: b }) => {
                a.to_string() == b.to_string()
            }
            _ => false,
        }
    }
}

impl fmt::Display for StreamStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => f.write_str("No camera found"),
            Self::Connecting { label } => write!(f, "Connecting to {label}..."),
            Self::Streaming { label } => write!(f, "Streaming: {label}"),
            Self::Failed { error } => write!(f, "Could not open camera: {error}"),
        }
    }
}
