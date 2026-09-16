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

mod adapt;
mod audio;
mod call;
mod metrics;
mod loopback;
pub(crate) mod runtime;
mod status;

use std::sync::Arc;

use dioxus_native::prelude::*;

pub use metrics::Metrics;
pub use status::RemoteStatus;

use crate::args::{Args, Peering};
use crate::audio::Controls;
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
    /// Our own endpoint id once bound, for the UI to show and copy.
    ///
    /// `None` until the endpoint is up, and forever in `--loopback`, which
    /// binds nothing.
    pub me: Signal<Option<String>>,
    /// Chooses who to talk to.
    ///
    pub connect: Callback<Peering>,
    /// Takes back a choice that has not become a call yet, returning to idle.
    ///
    /// Only meaningful while [`RemoteStatus::is_cancellable`] -- once a peer is
    /// connected, stopping is a hang-up and there is no channel on which to
    /// tell them. Ticket 29.
    pub cancel: Callback<()>,
    /// Live numbers for the call: rtt, loss, bitrate, incoming frame rate.
    pub metrics: Arc<Metrics>,
    /// The microphone, and whether the peer is talking.
    ///
    /// Shared rather than messaged: the publication's controls are `Send +
    /// Sync` and only set a flag the capture driver reads, so this thread can
    /// call them directly. It outlives any one call -- muting while idle stays
    /// true when the next call starts.
    pub audio: Arc<Controls>,
}

/// Starts whichever remote path the command line asked for.
///
/// One hook and one tile for both, because the difference is a matter of where
/// the packets went, and every stage on either side of that is shared. The
/// loopback is not a mock: it runs the same encoder, the same decoder and the
/// same drain, so what it proves carries over.
///
/// Runs once, and the call thread it starts outlives any one call: an ending
/// puts the controls back rather than closing the app down. Hanging up on a
/// live peer and calling a second one still need decisions nothing here makes.
pub fn use_remote() -> Remote {
    let mut status = use_signal(|| RemoteStatus::Binding);
    let mut me = use_signal(|| Option::<String>::None);
    // Status crosses from the call thread, which cannot touch a signal.
    let updates = use_channel(move |next: RemoteStatus| {
        // The key arrives inside a status rather than on its own channel: the
        // call thread already reports it that way, and a second channel would
        // be a second thing to keep in step.
        if let RemoteStatus::Idle { me: bound } = &next {
            me.set(Some(bound.clone()));
        }
        status.set(next);
    });

    use_hook(|| {
        let audio = Arc::new(Controls::default());
        let metrics = Arc::new(Metrics::default());
        let (tap, feed) = frame_tap();
        let slot = FrameSlot::default();
        // The decision, not the feed, is what gets deferred. The call thread
        // takes the feed at startup and parks on this receiver after binding,
        // so there is no window where two pipelines could own one feed --
        // `frame_tap` has a single slot and would let them steal from each
        // other.
        //
        // A stream rather than a one-shot: a choice can be taken back while it
        // is still only a choice (`Peering::Idle` means "cancel"). Once a peer
        // is connected the UI stops offering it -- stopping then is a hang-up,
        // which the peer would have to be told about. Ticket 29.
        let (decide, decided) = futures_channel::mpsc::unbounded::<Peering>();

        match Args::get().peering.clone() {
            Peering::Loopback => {
                status.set(RemoteStatus::Loopback);
                loopback::spawn(feed, slot.clone(), audio.clone());
            }
            peering => {
                call::spawn(
                    decided,
                    feed,
                    slot.clone(),
                    updates,
                    audio.clone(),
                    metrics.clone(),
                );
                // `--listen` / `--dial` decide immediately; `Idle` waits for the
                // UI. The CLI path is deliberately unchanged: it is how two
                // instances get driven from a script.
                if peering != Peering::Idle {
                    let _ = decide.unbounded_send(peering);
                }
            }
        }

        let connect = Callback::new({
            let decide = decide.clone();
            move |peering: Peering| {
                // Failure means the call thread is gone, which the status line
                // will already be saying.
                let _ = decide.unbounded_send(peering);
            }
        });
        // Takes back a choice that has not become a call yet. Same channel, so
        // the call thread sees the two in the order they were pressed.
        let cancel = Callback::new(move |()| {
            let _ = decide.unbounded_send(Peering::Idle);
        });

        Remote {
            tap,
            slot,
            status,
            me,
            connect,
            cancel,
            audio,
            metrics,
        }
    })
}
