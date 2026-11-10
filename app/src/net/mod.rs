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
//! [`loopback`] hands them straight back to a decoder in this one. Both end in
//! a [`FrameSlot`] -- one per participant on a call, one for the round trip in
//! loopback -- which is why the renderer needs to know nothing about either.

mod adapt;
mod audio;
mod call;
mod codec_check;
mod metrics;
mod loopback;
pub(crate) mod runtime;
mod share;
mod status;

use std::sync::Arc;

use dioxus_native::prelude::*;

pub use metrics::{Ask, Metrics, PeerStats};
pub use share::{ShareOption, ShareSource, ShareWish, available as share_available, lists as share_lists, sources as share_sources};
pub use status::RemoteStatus;

use crate::args::{Args, Peering};
use crate::audio::Controls;
use crate::camera::{frame_tap, FrameSlot, FrameTap};
use crate::channel::use_channel;

/// Somebody whose video we are showing, and the name that keys their tile.
///
/// The id is their iroh endpoint id, read off the path they publish under. It
/// is what `ParticipantVideo` has asked callers for since it was written: a key
/// that survives the list reordering, so two people's video cannot swap tiles.
#[derive(Clone)]
pub struct Participant {
    /// Their endpoint id.
    pub id: String,
    /// Their decoded frames.
    pub slot: FrameSlot,
    /// What their tile says about them: frame rate, and whether they talk.
    /// A screen carries its sharer's entry, so its tile reads their room.
    pub stats: Arc<PeerStats>,
    /// Which of their sources this is: their camera, or a screen they share.
    pub feed: Feed,
    /// Which arrival of this source this is, unique for the run. Part of the
    /// tile's key: a tile's video widget is built once, from the first slot it
    /// is given, so the same source arriving again with a new slot needs a new
    /// tile, or it would paint the old slot's last frame forever.
    pub generation: u64,
}

/// One of a person's sources. Someone sharing a screen is one person with two
/// entries, told apart by this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feed {
    Camera,
    Screen,
}

impl Participant {
    /// Enough of the id to tell two people apart in a log or a label, without
    /// carrying 64 hex characters around.
    pub fn short(&self) -> String {
        self.id.chars().take(8).collect()
    }
}

/// What the call thread tells the interface about who is here.
///
/// Membership crosses as messages rather than as shared state because it is
/// discovered on a worker thread and signals are `!Send` -- the same reason
/// status does.
pub(super) enum ParticipantEvent {
    /// Somebody is publishing; here is where their frames will arrive.
    Joined {
        id: String,
        slot: FrameSlot,
        stats: Arc<PeerStats>,
        feed: Feed,
    },
    /// They stopped publishing that source.
    Left { id: String, feed: Feed },
    /// The call is over; everyone goes.
    Cleared,
}

/// The call, as the interface sees it: who is here and what it is doing.
#[derive(Clone)]
pub struct Remote {
    /// Hand this to `use_camera_stream` so captured frames reach the encoder.
    pub tap: FrameTap,
    /// Everyone else on the call, in the order they arrived.
    ///
    /// In `--loopback` this holds exactly one entry, our own round-tripped
    /// video, so the interface has one way to render a tile rather than two.
    pub participants: Signal<Vec<Participant>>,
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
    /// Ourselves, as our own tile shows us and as the room is told: whether
    /// our camera and microphone are on. The controls write it; the call
    /// announces it in our presence document.
    pub local: Arc<PeerStats>,
    /// What we want to share. The bar writes it; each call attempt serves it.
    pub share: ShareWish,
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
    let mut participants = use_signal(Vec::<Participant>::new);
    let mut arrivals = 0u64;
    let joined = use_channel(move |event: ParticipantEvent| match event {
        ParticipantEvent::Joined { id, slot, stats, feed } => {
            arrivals += 1;
            let mut list = participants.write();
            // A second announce of the same source replaces the first: two
            // entries would be two tiles under one key.
            list.retain(|p| !(p.id == id && p.feed == feed));
            list.push(Participant {
                id,
                slot,
                stats,
                feed,
                generation: arrivals,
            })
        }
        ParticipantEvent::Left { id, feed } => {
            participants.write().retain(|p| !(p.id == id && p.feed == feed))
        }
        ParticipantEvent::Cleared => participants.write().clear(),
    });
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
        // The out-of-room volume, as last set. Read once here; the subroom
        // menu saves it as it changes.
        let saved = crate::settings::Settings::load();
        if let Some(volume) = saved.crosstalk_volume {
            audio.set_crosstalk_volume(volume);
        }
        if let Some(muted) = saved.crosstalk_muted {
            audio.set_crosstalk_muted(muted);
        }
        let metrics = Arc::new(Metrics::default());
        let local = Arc::new(PeerStats::default());
        let share = ShareWish::new();
        let (tap, feed) = frame_tap();
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
                let slot = FrameSlot::default();
                let stats = Arc::new(PeerStats::default());
                loopback::spawn(feed, slot.clone(), stats.clone(), audio.clone());
                // A participant like any other, so the tile below is rendered by
                // the same code a real peer's is.
                participants.set(vec![Participant {
                    id: "loopback".to_string(),
                    slot,
                    stats,
                    feed: Feed::Camera,
                    generation: 0,
                }]);
            }
            peering => {
                call::spawn(
                    decided,
                    feed,
                    updates,
                    joined,
                    audio.clone(),
                    metrics.clone(),
                    local.clone(),
                    share.clone(),
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
            participants,
            status,
            me,
            connect,
            cancel,
            audio,
            metrics,
            local,
            share,
        }
    })
}
