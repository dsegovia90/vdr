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
mod forwarded;
pub(crate) mod door;
mod metrics;
mod loopback;
pub(crate) mod moves;
pub(crate) mod quality;
mod report;
#[cfg(test)]
mod routes;
pub(crate) mod runtime;
mod share;
mod status;

use std::sync::Arc;

use dioxus_native::prelude::*;

pub use metrics::{Ask, Metrics, Path, PeerStats};
pub use share::{ShareOption, ShareQuality, ShareSource, ShareWish, available as share_available, lists as share_lists, sources as share_sources};
pub use status::RemoteStatus;
pub(crate) use call::redacted;
use call::HostNews;

use crate::args::{Args, Peering};
use crate::audio::Controls;
use crate::camera::{frame_tap, FrameSlot, FrameTap};
use crate::channel::use_channel;
use crate::recent::RecentRooms;

use std::cell::RefCell;
use std::rc::Rc;

/// How long a room has to hold, with no drop, before it is remembered: long
/// enough to outlast a relay turning the session away, short enough that a
/// quick visit still counts.
const SETTLED: std::time::Duration = std::time::Duration::from_secs(3);

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
        crate::name::short(&self.id)
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
    /// connected, stopping is a hang-up, which the peer has to be told about;
    /// that goes through Leave instead.
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
    /// Rooms joined before, for the idle screen to offer again. Recorded here,
    /// on arriving in the room, and changed by the list's own buttons.
    pub recent: Signal<RecentRooms>,
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
    let mut recent = use_signal(RecentRooms::load);
    // The room last asked for, until it is reached. Kept on this side so the
    // token never travels in a status, which is shown and compared.
    let mut chosen = use_signal(|| Option::<url::Url>::None);
    // **Remembered once the room has held, not when it is first reached.** A
    // relay that refuses a token still lets the session report connected for
    // an instant before closing it, over and over as the session redials, so
    // "reached the room" alone would remember a link that never worked. Each
    // arrival starts a short wait on a thread of its own; any status that is
    // not "in the room" in the meantime bumps the count, and the wait's
    // message then finds it stale.
    let mut arrival = use_signal(|| 0u64);
    // The doors issued for rooms joined with hosting on; see `door::Doors`.
    let doors = use_hook(|| Arc::new(door::Doors::default()));
    // Hosts heard, or listed by an invite, for a room not remembered yet,
    // keyed by the room's identity: kept when it is, dropped if it never is.
    // Not a signal, because nothing on screen reads it.
    let waiting: Rc<RefCell<Vec<(String, crate::recent::Host)>>> = use_hook(|| Rc::new(RefCell::new(Vec::new())));
    let settled = use_channel({
        let doors = doors.clone();
        let waiting = waiting.clone();
        move |waited_for: u64| {
            if *arrival.peek() != waited_for {
                return;
            }
            let pending = chosen.peek().clone();
            if let Some(url) = pending {
                // Cleared, so a reconnect later in the call does not record it
                // again.
                chosen.set(None);
                tracing::info!("remembering {}", redacted(&url));
                let key = crate::recent::identity(&url);
                let now = crate::recent::now();
                let door = doors.get(&key);
                let hosts: Vec<crate::recent::Host> = {
                    let mut waiting = waiting.borrow_mut();
                    let (ours, rest) = waiting.drain(..).partition(|(room, _)| *room == key);
                    *waiting = rest;
                    ours.into_iter().map(|(_, host)| host).collect()
                };
                crate::recent::change(&mut recent, |list| {
                    list.joined(&url, now);
                    if let Some(door) = door {
                        list.keep_door(&key, &door);
                    }
                    for host in hosts {
                        list.heard(&key, host, now);
                    }
                });
            }
        }
    });
    // What the call hears about the room's hosts. Written to the list only
    // when the room is in it; the call sends only changes, so this is rare.
    let heard = use_channel({
        let waiting = waiting.clone();
        move |news: HostNews| match news {
            HostNews::Heard { room, id, door, told_by } => {
                let now = crate::recent::now();
                let Some(host) = crate::recent::Host::new(&id, &door, Some(&told_by), now) else { return };
                let known = recent.peek().rooms().iter().any(|entry| entry.key() == room);
                if known {
                    crate::recent::change(&mut recent, |list| {
                        list.heard(&room, host, now);
                    });
                } else {
                    waiting.borrow_mut().push((room, host));
                }
            }
            HostNews::Stopped { room, id } => {
                waiting.borrow_mut().retain(|(waiting_room, host)| !(*waiting_room == room && host.id() == id));
                let known = recent.peek().rooms().iter().any(|entry| entry.key() == room);
                if known {
                    crate::recent::change(&mut recent, |list| list.forget(&room, &id));
                }
            }
        }
    });
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
        // Here, at the status's source, rather than from what the page
        // renders: the updater's worker must not restart the app under a call.
        crate::update::set_call_active(next.is_cancellable());
        // The key arrives inside a status rather than on its own channel: the
        // call thread already reports it that way, and a second channel would
        // be a second thing to keep in step.
        if let RemoteStatus::Idle { me: bound } = &next {
            me.set(Some(bound.clone()));
        }
        // The label check drops a late arrival from a room already cancelled
        // in favour of another.
        let count = *arrival.peek() + 1;
        arrival.set(count);
        if let RemoteStatus::InRoom { room, .. } = &next {
            let pending = chosen.peek().clone();
            if pending.is_some_and(|url| call::room_label(&url) == *room) {
                let settled = settled.clone();
                let spawned = std::thread::Builder::new().name("room-settled".into()).spawn(move || {
                    std::thread::sleep(SETTLED);
                    let _ = settled.unbounded_send(count);
                });
                if let Err(error) = spawned {
                    tracing::warn!("could not start the wait that remembers a room: {error}");
                }
            }
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
        // Our name, as last saved; the Profile tab changes it from here on.
        local.set_name(saved.name.as_deref().and_then(crate::name::clean));
        // Whether we report our connection and show others'; the Experimental
        // tab changes it from here on, and the call reads it as it goes.
        local.set_connection_report(saved.connection_report_on());
        // Whether we host the rooms we join; read by each join as it starts.
        local.set_host_rooms(saved.hosts_rooms() || crate::args::Args::get().host_rooms);
        // Whether we follow moves of the call; the Experimental tab changes it.
        local.set_move_calls(saved.moves_calls() || crate::args::Args::get().move_calls);
        // The camera and microphone as launched. Our entry starts "on" for
        // both, and only a click changes it, so without this a camera started
        // off was announced as on, and so was a microphone that `--no-audio`
        // never opened: the room waited on a frame or a voice that never came.
        local.set_camera(!crate::args::Args::get().camera_off);
        if crate::args::Args::get().no_audio {
            local.set_mic(false);
        }
        // What every decision to join goes through: the hosts an invite
        // lists come off the link before it goes anywhere, and with hosting
        // on the room gets its door, the one it already has or a new one.
        let prepare = {
            let doors = doors.clone();
            let waiting = waiting.clone();
            let local = local.clone();
            move |peering: Peering| -> Peering {
                let Peering::Join(url) = peering else { return peering };
                let (url, invited) = crate::recent::split_invite(&url, crate::recent::now());
                let key = crate::recent::identity(&url);
                // Where else the room can meet, for the join to dial beside the
                // relay: what the room has heard, then what the invite lists,
                // each host once.
                let mut hosts: Vec<door::Known> = Vec::new();
                for host in recent.peek().hosts_for(&key, crate::recent::now()).iter().chain(&invited) {
                    if !hosts.iter().any(|known| known.id == host.id()) {
                        hosts.push(door::Known {
                            id: host.id().to_string(),
                            door: host.door().to_string(),
                        });
                    }
                }
                doors.set_hosts(key.clone(), hosts);
                {
                    let mut waiting = waiting.borrow_mut();
                    // A new decision: what an earlier attempt left is stale.
                    waiting.clear();
                    waiting.extend(invited.into_iter().map(|host| (key.clone(), host)));
                }
                if local.host_rooms() {
                    let door = recent.peek().door_for(&key).map(Ok).unwrap_or_else(door::fresh);
                    match door {
                        Ok(door) => doors.issue(key, door),
                        Err(error) => tracing::warn!("not hosting this room: {error}"),
                    }
                } else {
                    // Only a room joined with hosting on is given a door.
                    doors.withdraw(&key);
                }
                Peering::Join(url)
            }
        };
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
        // which the peer has to be told about, so it goes through Leave.
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
                    doors.clone(),
                    heard,
                );
                // `--listen` / `--dial` decide immediately; `Idle` waits for the
                // UI. The CLI path is deliberately unchanged: it is how two
                // instances get driven from a script.
                if peering != Peering::Idle {
                    crate::update::set_call_active(true);
                    let peering = prepare.clone()(peering);
                    if let Peering::Join(url) = &peering {
                        chosen.set(Some(url.clone()));
                    }
                    let _ = decide.unbounded_send(peering);
                }
            }
        }

        let connect = Callback::new({
            let decide = decide.clone();
            move |peering: Peering| {
                let peering = prepare(peering);
                // At once, before the call's first status arrives: an update
                // must not restart the app in that gap.
                if peering != Peering::Idle {
                    crate::update::set_call_active(true);
                }
                chosen.set(match &peering {
                    Peering::Join(url) => Some(url.clone()),
                    _ => None,
                });
                // Failure means the call thread is gone, which the status line
                // will already be saying.
                let _ = decide.unbounded_send(peering);
            }
        });
        // Takes back a choice that has not become a call yet. Same channel, so
        // the call thread sees the two in the order they were pressed.
        let cancel = Callback::new(move |()| {
            chosen.set(None);
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
            recent,
        }
    })
}
