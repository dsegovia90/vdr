//! Our catalogs, written again when a route to us may have changed, and every
//! few seconds regardless.
//!
//! **A reader carried to a new route waits for a group that may never come.**
//! When our offer moves (the call moving between the relay and a host, a
//! spare taking over, a session coming or going), an older relay (one on
//! moq-net before 0.3.24), or a peer on an older build of ours, carries a
//! reader's subscription across by resuming on the new route just past the
//! last group it had. Media has a next group within a second or two; a
//! catalog gets one only when it changes, which may be never. A relay that
//! took such a carried-over subscription then holds a copy of our catalog
//! with nothing in it, and anyone who joins later is attached to that copy
//! and hears nothing until we next change something: their tile for us stays
//! blank (seen for 19 s, until a camera was toggled). That the relay keeps
//! the empty copy for later subscribers is read from its source, not seen
//! inside it.
//!
//! A current library never carries a read across: it ends the read, which
//! is asked for again and starts at the newest group. In a room of current
//! builds on a current relay this is redundant; it stays for the rest.
//!
//! **So a catalog is written again, unchanged in substance.** The library
//! writes nothing for a catalog equal to the last one, so what is written is
//! a count of our own ([`Ext`]) beside the media sections: a root section a
//! reader ignores (the catalog's readers, ours on every build included, drop
//! sections they do not know), there only to make each catalog differ from
//! the last. Readers act on what a rendition opens from, which this never
//! touches, so nobody restarts a decoder over it (`follow.rs`).
//!
//! Written [`SETTLE`] after anything that can move our offer, once the move
//! has had time to carry readers across, and every [`HEARTBEAT`] whatever
//! happens, which bounds the wait for a cause this does not hear of.

use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long after a change of route the catalog is written: past the
/// moment an old offer is withdrawn (`MOVE_OVERLAP` in `sessions.rs`, a
/// second after its replacement is made), with half a second more for that to be passed on, so a reader it
/// carries across is already on the new route to receive the write.
pub(super) const SETTLE: Duration = Duration::from_millis(1500);

/// How often the catalog is written whatever happens: the longest anyone
/// can be left without our catalog by a cause we did not hear of. A few
/// hundred bytes a peer every five seconds is nothing beside the media,
/// and five seconds is about as long as a blank tile goes unremarked.
pub(super) const HEARTBEAT: Duration = Duration::from_secs(5);

/// How often the writer looks at the clock and at the count of route changes.
const TICK: Duration = Duration::from_millis(250);

/// Our root section in the catalog: the count that makes a rewritten
/// catalog a new one. Absent until the first rewrite.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Ext {
    #[serde(default, skip_serializing_if = "is_zero")]
    fresh: u64,
}

impl moq_mux::catalog::hang::CatalogExt for Ext {}

fn is_zero(count: &u64) -> bool {
    *count == 0
}

/// The catalog every broadcast of ours carries.
pub(super) type Catalog = moq_mux::catalog::Producer<Ext>;

/// A video track published into a [`Catalog`].
pub(super) type Video = moq_video::encode::Producer<Ext>;

/// A catalog for `broadcast`, as hang's default but with room for [`Ext`].
pub(super) fn catalog(broadcast: &mut moq_net::broadcast::Producer) -> Result<Catalog, moq_net::Error> {
    let config = moq_mux::catalog::Config::default().with_catalog(moq_mux::catalog::hang::Catalog::<Ext>::default());
    Catalog::new(broadcast, config)
}

/// When to write the catalog again.
#[derive(Debug)]
pub(super) struct Schedule {
    written: Instant,
    /// When the latest change of route was heard, until it is answered.
    asked: Option<Instant>,
    /// The count of route changes last seen.
    seen: u64,
}

impl Schedule {
    pub(super) fn new(now: Instant, routes: u64) -> Self {
        Self {
            written: now,
            asked: None,
            seen: routes,
        }
    }

    /// Whether to write now, with `routes` changes of route counted so far;
    /// if so, counted as written.
    ///
    /// A change heard while one is waiting moves the write later rather
    /// than leaving it where it was: it is the latest move whose readers
    /// need the next group.
    pub(super) fn due(&mut self, now: Instant, routes: u64) -> bool {
        if routes != self.seen {
            self.seen = routes;
            self.asked = Some(now);
        }
        let due = self.asked.is_some_and(|at| now >= at + SETTLE) || now >= self.written + HEARTBEAT;
        if due {
            self.written = now;
            self.asked = None;
        }
        due
    }
}

/// Writes `catalog` again whenever [`Schedule`] says, reading the changes of
/// route off `keyframe`, until aborted.
pub(super) async fn keep(mut catalog: Catalog, keyframe: Arc<super::call::Keyframes>) {
    let mut schedule = Schedule::new(Instant::now(), keyframe.routes());
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if schedule.due(Instant::now(), keyframe.routes()) {
            rewrite(&mut catalog);
        }
    }
}

/// Writes `catalog` again as a new group, if it has been published with
/// something in it; whether it was.
///
/// One with nothing offered yet is left alone: the library holds the first
/// catalog back until there is something to say, and this must not say it
/// early.
pub(super) fn rewrite(catalog: &mut Catalog) -> bool {
    let snapshot = catalog.snapshot();
    if snapshot.video.renditions.is_empty() && snapshot.audio.renditions.is_empty() {
        return false;
    }
    match catalog.mutate(|catalog| catalog.ext.fresh = catalog.ext.fresh.wrapping_add(1)) {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!("could not write our catalog again: {error:#}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_written_between_heartbeats_without_a_change() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start, 0);
        assert!(!schedule.due(start + Duration::from_secs(1), 0));
        assert!(!schedule.due(start + HEARTBEAT - Duration::from_millis(1), 0));
        assert!(schedule.due(start + HEARTBEAT, 0));
        assert!(!schedule.due(start + HEARTBEAT + Duration::from_secs(1), 0));
        assert!(schedule.due(start + HEARTBEAT * 2, 0));
    }

    #[test]
    fn a_change_of_route_is_answered_once_it_has_settled() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start, 3);
        let moved = start + Duration::from_secs(1);
        assert!(!schedule.due(moved, 4));
        assert!(!schedule.due(moved + SETTLE - Duration::from_millis(1), 4));
        assert!(schedule.due(moved + SETTLE, 4));
        // Answered, so not again until the heartbeat, counted from now.
        assert!(!schedule.due(moved + SETTLE + Duration::from_secs(1), 4));
        assert!(schedule.due(moved + SETTLE + HEARTBEAT, 4));
    }

    #[test]
    fn a_later_change_moves_the_write_later() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start, 0);
        assert!(!schedule.due(start, 1));
        let again = start + Duration::from_secs(1);
        assert!(!schedule.due(again, 2));
        assert!(!schedule.due(start + SETTLE, 2), "the first change's time is not the latest's");
        assert!(schedule.due(again + SETTLE, 2));
    }

    #[test]
    fn a_heartbeat_comes_however_often_the_route_changes() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start, 0);
        let mut written = false;
        // A change every half second, each pushing the settled write on.
        for step in 1..=10u64 {
            written |= schedule.due(start + Duration::from_millis(500) * step as u32, step);
        }
        assert!(written, "the heartbeat bounds it");
    }

    /// A broadcast with our catalog and a video rendition in it, encoded by
    /// software, as the camera's is.
    async fn offering() -> (moq_net::origin::Producer, moq_net::broadcast::Producer, Catalog, Video) {
        let origin = moq_tokio::origin::spawn();
        let mut broadcast = origin.create_broadcast("fresh").unwrap();
        let catalog = catalog(&mut broadcast).unwrap();
        let mut config = moq_video::encode::Config::new(160, 90, moq_video::Rate::new(30, 1).unwrap());
        config.kind = moq_video::encode::Kind::Software;
        let rendition = config.probe().await.unwrap();
        let track = broadcast.create_track("video.avc3", catalog.track_info(60)).unwrap();
        let video = Video::with_track(track, catalog.clone(), rendition).unwrap();
        broadcast.announce(moq_net::origin::Route::default()).unwrap();
        (origin, broadcast, catalog, video)
    }

    async fn next(catalog: &mut moq_mux::catalog::Consumer<()>) -> Option<super::super::follow::Catalog> {
        use moq_mux::catalog::Stream as _;
        tokio::time::timeout(Duration::from_secs(2), catalog.next()).await.ok().and_then(|next| next.ok().flatten())
    }

    #[tokio::test]
    async fn an_unchanged_catalog_written_again_is_a_new_one_and_the_same_to_a_reader() {
        let (origin, _broadcast, mut catalog, _video) = offering().await;
        let broadcast = origin.consume().request_broadcast("fresh").await.unwrap();
        let mut reading =
            moq_mux::catalog::Consumer::<()>::new(&broadcast, moq_mux::catalog::CatalogFormat::Hang).await.unwrap();
        let first = next(&mut reading).await.expect("the first catalog");
        assert!(rewrite(&mut catalog));
        let again = next(&mut reading).await.expect("the catalog written again reaches the reader");
        // Read as a build without the count reads it: the media unchanged.
        assert_eq!(first, again);
    }

    #[tokio::test]
    async fn a_catalog_with_nothing_offered_is_not_written_early() {
        let origin = moq_tokio::origin::spawn();
        let mut broadcast = origin.create_broadcast("empty").unwrap();
        let mut catalog = catalog(&mut broadcast).unwrap();
        assert!(!rewrite(&mut catalog));
        assert_eq!(catalog.snapshot().ext, Ext::default());
    }
}
