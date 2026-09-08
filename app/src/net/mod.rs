//! The path frames take once they leave the local preview.
//!
//! A sibling of `camera`, never chained to it: capture publishes into a
//! [`FrameTap`] and everything here reads from that, so neither side can
//! starve the other. The direction of travel is
//!
//! ```text
//!                        |-> FrameSlot -> VideoPaintSource -> local preview
//! cameras::pump --frame--|
//!                        |-> FrameTap -> encoder -> moq -> a peer
//! ```
//!
//! and the right-hand branch has two implementations that differ only in the
//! middle: [`call`] carries the packets over QUIC to another process, and
//! [`loopback`] hands them straight back to a decoder in this one. Both end at
//! the same [`FrameSlot`], which is why the renderer needs to know nothing
//! about either.

mod call;
mod loopback;
mod runtime;
mod status;

use dioxus_native::prelude::*;

pub use status::RemoteStatus;

use crate::args::{Args, Peering};
use crate::camera::{frame_tap, FrameSlot, FrameTap};
use crate::channel::use_channel;

/// The second tile: a peer's video, or our own if there is no peer.
#[derive(Clone)]
pub struct Remote {
    /// Hand this to `use_camera_stream` so captured frames reach the encoder.
    pub tap: FrameTap,
    /// Decoded frames, ready for a `ParticipantVideo`.
    pub slot: FrameSlot,
    /// What the connection is doing, for the UI to show.
    pub status: Signal<RemoteStatus>,
}

/// Starts whichever remote path the command line asked for.
///
/// One hook and one tile for both, because the difference is a matter of where
/// the packets went, and every stage on either side of that is shared. The
/// loopback is not a mock: it runs the same encoder, the same decoder and the
/// same drain, so what it proves carries over.
///
/// Runs once. Redialling, hanging up and calling a second peer all need a UI
/// that does not exist yet, and each is a decision rather than a detail.
pub fn use_remote() -> Remote {
    let mut status = use_signal(|| RemoteStatus::Binding);
    // Status crosses from the call thread, which cannot touch a signal.
    let updates = use_channel(move |next: RemoteStatus| status.set(next));

    use_hook(|| {
        let (tap, feed) = frame_tap();
        let slot = FrameSlot::default();
        match Args::get().peering.clone() {
            Peering::Loopback => {
                status.set(RemoteStatus::Loopback);
                loopback::spawn(feed, slot.clone());
            }
            peering => call::spawn(peering, feed, slot.clone(), updates),
        }
        Remote { tap, slot, status }
    })
}
