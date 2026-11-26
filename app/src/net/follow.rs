//! Reading what someone offers -- their video, their voice, their screen --
//! for as long as they are here, whichever way it comes.
//!
//! **A read can end while its path stays announced.** A room reached through
//! several sessions changes the route to a peer whenever the call moves or a
//! session goes, and the library carries a read across that only sometimes:
//! it re-splices at a group boundary when the new route names the same
//! publisher, and otherwise ends the broadcast it served, leaving each track
//! to end with the session it came through ("dropped", "connection error").
//! The announcement only changes route, or does not change at all, so nothing
//! in the attendance loop would ever start a new reader, and whatever the
//! peer did next was lost for the rest of the call: a camera turned on after
//! a move stayed blank. The library says what to do: the next request serves
//! the path again. So a reader that ends asks for the broadcast again, until
//! it is back or the retraction (their leave) aborts it.
//!
//! **The catalog and the media are read apart.** The catalog is followed for
//! the whole call, not read once until something is offered, so a rendition
//! added, removed or changed later (a camera turned on, a new encoder) is
//! acted on. The player of a rendition runs in a task of its own and is left
//! alone while it is healthy: a catalog cut off under a player that is still
//! delivering costs the player nothing, and a player is only replaced when
//! the rendition it plays changes or its read ends. Its read is never
//! cancelled from here, which the decoders do not survive.
//!
//! What a player does with what it reads is its own: video stays newest-wins
//! and audio stays buffered (`audio.rs`), and a player that is replaced
//! starts as a new subscriber of either kind would. A catalog that arrives
//! again unchanged, as a publisher writes it after a change of route
//! (`fresh.rs`), is the same renditions and replaces nobody.
//!
//! **A read that hears no catalog at all is asked for again too.** A
//! catalog's newest group is there for any new reader, so a first one that
//! has not come within [`PATIENCE`] is not coming on this read: the route
//! it was given is waiting on a group that does not exist yet, as one
//! carried across a change of route does. Asking again may be answered by a
//! better route; the publisher writing its catalog again ends the wait
//! either way. Only the first catalog of a read is waited for like this:
//! after it, a catalog that is quiet has simply not changed.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use moq_mux::catalog::Stream as _;

/// How long a reader waits before asking again for a broadcast it lost: the
/// time a route takes to settle after a session goes, near enough.
pub(in crate::net) const AGAIN: Duration = Duration::from_millis(250);

/// How long a picture whose read ended is left up before it is taken down,
/// if no player has taken over by then. Longer than a route change takes to
/// be served again (`sessions.rs` measures it), so a change of route shows as
/// a moment's freeze, not a blank tile; short enough that a picture is never
/// mistaken for live.
const HOLD: Duration = Duration::from_millis(2500);

/// How long a read waits for its first catalog before asking again. A
/// catalog that is there arrives in a round trip, well under a second even
/// through a relay, so three seconds of nothing means it is not there on
/// this route. Shorter than the publisher's own rewrite (`fresh.rs`), so a
/// better route, if asking again finds one, is not left waiting on it.
pub(in crate::net) const PATIENCE: Duration = Duration::from_secs(3);

/// Whether the first catalog of a read is late, and whether that has been
/// said: once an outage, however many reads it takes.
#[derive(Debug, Default)]
struct Overdue {
    /// When the current read is given up on, while it has heard nothing.
    due: Option<tokio::time::Instant>,
    said: bool,
}

impl Overdue {
    /// A read began at `now`.
    fn began(&mut self, now: tokio::time::Instant) {
        self.due = Some(now + PATIENCE);
    }

    /// Its catalog arrived: nothing is due, and a later outage is said anew.
    fn heard(&mut self) {
        self.due = None;
        self.said = false;
    }

    /// The read was given up on: whether to say so.
    fn lapsed(&mut self) -> bool {
        self.due = None;
        !std::mem::replace(&mut self.said, true)
    }
}

/// A peer's catalog, as the library reads it.
pub(in crate::net) type Catalog = moq_mux::catalog::hang::Catalog<()>;

/// One rendition a catalog offers.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::net) struct Rendition {
    pub name: String,
    /// What a decoder is opened from, written out, so a change to any of it
    /// (a new codec, a new size) is a different rendition.
    pub key: String,
}

/// The parts of a rendition's description that a decoder is opened from.
/// Everything else in it (the bitrate, a stall hint, the frame rate) is
/// rewritten as the sender's rate control moves, and replacing a healthy
/// player on each of those would freeze the picture every few seconds.
const OPENED_FROM: [&str; 8] = [
    "broadcast",
    "codec",
    "description",
    "codedWidth",
    "codedHeight",
    "container",
    "sampleRate",
    "channelCount",
];

impl Rendition {
    pub(in crate::net) fn new(name: &str, config: &impl serde::Serialize) -> Self {
        let key = match serde_json::to_value(config) {
            Ok(serde_json::Value::Object(mut fields)) => {
                fields.retain(|field, _| OPENED_FROM.contains(&field.as_str()));
                serde_json::Value::Object(fields).to_string()
            }
            _ => String::new(),
        };
        Self {
            name: name.to_string(),
            key,
        }
    }
}

/// How a player ended.
pub(in crate::net) enum Played {
    /// Its read ended: the broadcast went, or its route did. Worth asking
    /// for again.
    Lost,
    /// It could not play this rendition at all (no decoder for it). Not
    /// tried again until the catalog offers something else.
    Failed,
}

/// A player's run, spawned by [`follow`].
pub(in crate::net) type Playing = Pin<Box<dyn Future<Output = Played> + Send>>;

/// One kind of thing to read out of a peer's broadcast.
pub(in crate::net) trait Reader: Send {
    /// What it is, for the log: "video", "audio".
    const WHAT: &'static str;

    /// The rendition to play out of `catalog`, if it offers one.
    fn pick(&self, catalog: &Catalog) -> Option<Rendition>;

    /// A player for `rendition`, which `catalog` offers, read from `broadcast`.
    fn play(&mut self, broadcast: &moq_net::broadcast::Consumer, catalog: &Catalog, rendition: &Rendition) -> Playing;

    /// A catalog arrived; `first` for the first ever, `offered` whether it
    /// has something to play.
    fn heard(&mut self, _first: bool, _offered: bool) {}

    /// Nothing is playing any more: the rendition was withdrawn, or the read
    /// ended and nothing took over within [`HOLD`].
    fn stopped(&mut self) {}
}

/// Where to ask for a peer's broadcast again.
pub(in crate::net) struct Asking {
    pub inbound: moq_net::origin::Consumer,
    pub path: String,
}

/// Follows `broadcast`'s catalog and plays what `reader` picks from it,
/// until aborted. Without `asking` (the loopback, which has no routes), a
/// read that ends is the end, as before routes could change.
pub(in crate::net) async fn follow<R: Reader>(
    asking: Option<Asking>,
    broadcast: moq_net::broadcast::Consumer,
    peer: String,
    mut reader: R,
) {
    let what = R::WHAT;
    let mut next = Some(broadcast);
    // Whether we are asking again, so an outage is said once and its end too.
    let mut lost = false;
    let mut first = true;
    let mut playing: Option<(Rendition, Task)> = None;
    let mut failed: Option<Rendition> = None;
    // When a picture left up by a player that ended comes down.
    let mut held: Option<tokio::time::Instant> = None;
    // Only with somewhere to ask again: the loopback's catalog is in this
    // process and has no route to wait on.
    let mut overdue = Overdue::default();
    loop {
        let broadcast = match next.take() {
            Some(broadcast) => broadcast,
            None => {
                let Some(asking) = &asking else { return };
                tokio::time::sleep(AGAIN).await;
                if held.is_some_and(|at| at <= tokio::time::Instant::now()) {
                    held = None;
                    if playing.is_none() {
                        reader.stopped();
                    }
                }
                match asking.inbound.request_broadcast(asking.path.as_str()).await {
                    Ok(broadcast) => broadcast,
                    Err(error) => {
                        tracing::debug!("{peer}'s {what} is not there to ask for yet: {error:#}");
                        continue;
                    }
                }
            }
        };
        if asking.is_some() {
            overdue.began(tokio::time::Instant::now());
        }
        // The subscribe is waited on with the same patience as what it
        // brings: a route with nothing behind it can hold either.
        let subscribing = moq_mux::catalog::Consumer::<()>::new(&broadcast, moq_mux::catalog::CatalogFormat::Hang);
        let subscribed = match overdue.due {
            Some(due) => tokio::time::timeout_at(due, subscribing).await.ok(),
            None => Some(subscribing.await),
        };
        let mut catalog = match subscribed {
            Some(Ok(catalog)) => catalog,
            Some(Err(error)) => {
                lose(&mut lost, &peer, what, &format!("no catalog: {error:#}"));
                continue;
            }
            None => {
                late(&mut overdue, &mut lost, &peer, what);
                continue;
            }
        };
        loop {
            let ended = async {
                match playing.as_mut() {
                    Some((_, task)) => (&mut task.0).await,
                    None => std::future::pending().await,
                }
            };
            let hold = async {
                match held {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            let due = async {
                match overdue.due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                snapshot = catalog.next() => match snapshot {
                    Ok(Some(snapshot)) => {
                        overdue.heard();
                        if std::mem::take(&mut lost) {
                            tracing::info!("{peer}'s {what} is back");
                        }
                        let wanted = reader.pick(&snapshot);
                        reader.heard(std::mem::take(&mut first), wanted.is_some());
                        match wanted {
                            None => {
                                failed = None;
                                if playing.take().is_some() || held.take().is_some() {
                                    tracing::info!("{peer} stopped offering {what}");
                                    reader.stopped();
                                }
                            }
                            Some(wanted) => {
                                let same = playing.as_ref().is_some_and(|(rendition, _)| *rendition == wanted);
                                if !same && failed.as_ref() != Some(&wanted) {
                                    if playing.is_some() {
                                        tracing::info!("{peer} changed their {what}; playing the new one");
                                    }
                                    failed = None;
                                    let task = Task(tokio::spawn(reader.play(&broadcast, &snapshot, &wanted)));
                                    playing = Some((wanted, task));
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        lose(&mut lost, &peer, what, "its catalog ended");
                        break;
                    }
                    Err(error) if crate::net::call::catalog_cut_off(&error) => {
                        lose(&mut lost, &peer, what, &format!("catalog cut off: {error:#}"));
                        break;
                    }
                    Err(error) => {
                        tracing::error!("could not read {peer}'s catalog: {error:#}");
                        return;
                    }
                },
                played = ended => {
                    let rendition = playing.take().map(|(rendition, _)| rendition);
                    match played {
                        Ok(Played::Failed) => {
                            failed = rendition;
                            reader.stopped();
                        }
                        // Lost, or the player panicked: either way its read
                        // is gone, and the broadcast with it, most likely.
                        Ok(Played::Lost) | Err(_) => {
                            held.get_or_insert(tokio::time::Instant::now() + HOLD);
                            lose(&mut lost, &peer, what, "its read ended");
                            break;
                        }
                    }
                }
                _ = hold => {
                    held = None;
                    if playing.is_none() {
                        reader.stopped();
                    }
                }
                _ = due => {
                    late(&mut overdue, &mut lost, &peer, what);
                    break;
                }
            }
        }
    }
}

/// Says an outage once: the asking again that follows is quiet.
fn lose(lost: &mut bool, peer: &str, what: &str, why: &str) {
    if !std::mem::replace(lost, true) {
        tracing::info!("lost {peer}'s {what} ({why}); asking again");
    }
}

/// Says, once an outage, that a read's first catalog did not come, and
/// counts it as lost, so its return is said as any other.
fn late(overdue: &mut Overdue, lost: &mut bool, peer: &str, what: &str) {
    if overdue.lapsed() {
        tracing::info!("no catalog for {peer}'s {what} within {}s; asking again", PATIENCE.as_secs());
    }
    *lost = true;
}

/// A player's task, aborted when dropped: replacing or ending the follower
/// ends its player too.
struct Task(tokio::task::JoinHandle<Played>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::{Overdue, Rendition, PATIENCE};
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn a_read_is_due_its_catalog_within_patience_of_beginning() {
        let mut overdue = Overdue::default();
        assert_eq!(overdue.due, None, "nothing is due before a read");
        let start = tokio::time::Instant::now();
        overdue.began(start);
        assert_eq!(overdue.due, Some(start + PATIENCE));
        overdue.heard();
        assert_eq!(overdue.due, None, "a read that has heard is never late");
    }

    #[test]
    fn a_late_catalog_is_said_once_an_outage() {
        let mut overdue = Overdue::default();
        let start = tokio::time::Instant::now();
        overdue.began(start);
        assert!(overdue.lapsed(), "the first lapse is said");
        assert_eq!(overdue.due, None);
        // Asked again, and late again: quiet.
        overdue.began(start + PATIENCE + Duration::from_millis(250));
        assert!(!overdue.lapsed());
        overdue.began(start + PATIENCE * 2);
        assert!(!overdue.lapsed());
        // Back, then lost again later: said again.
        overdue.began(start + PATIENCE * 3);
        overdue.heard();
        overdue.began(start + PATIENCE * 10);
        assert!(overdue.lapsed());
    }

    #[test]
    fn asking_again_is_paced() {
        // The longest a read waits plus the pause before the next ask: what
        // keeps a route with nothing behind it from being asked in a spin.
        assert!(PATIENCE + super::AGAIN >= Duration::from_secs(3));
        assert!(PATIENCE < super::super::fresh::HEARTBEAT, "asked again before a heartbeat would end the wait");
    }

    /// A player's starts and a reader's catalogs, counted.
    #[derive(Clone, Default)]
    struct Counted {
        plays: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        heard: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl super::Reader for Counted {
        const WHAT: &'static str = "video";

        fn pick(&self, catalog: &super::Catalog) -> Option<Rendition> {
            let (name, config) = catalog.video.renditions.iter().next()?;
            Some(Rendition::new(name, config))
        }

        fn play(&mut self, _: &moq_net::broadcast::Consumer, _: &super::Catalog, _: &Rendition) -> super::Playing {
            self.plays.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::pin(std::future::pending())
        }

        fn heard(&mut self, _first: bool, _offered: bool) {
            self.heard.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Our catalog written again after a change of route reaches the
    /// follower each time, and never restarts the player it already has.
    #[tokio::test]
    async fn a_catalog_written_again_restarts_no_player() {
        let origin = moq_tokio::origin::spawn();
        let mut broadcast = origin.create_broadcast("again").unwrap();
        let mut catalog = super::super::fresh::catalog(&mut broadcast).unwrap();
        let mut config = moq_video::encode::Config::new(160, 90, moq_video::Rate::new(30, 1).unwrap());
        config.kind = moq_video::encode::Kind::Software;
        let rendition = config.probe().await.unwrap();
        let track = broadcast.create_track("video.avc3", catalog.track_info(60)).unwrap();
        let _video = super::super::fresh::Video::with_track(track, catalog.clone(), rendition).unwrap();
        broadcast.announce(moq_net::origin::Route::default()).unwrap();

        let reading = origin.consume().request_broadcast("again").await.unwrap();
        let counted = Counted::default();
        let following = tokio::spawn(super::follow(None, reading, "someone".into(), counted.clone()));
        let heard = |count: usize| {
            let counted = counted.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while counted.heard.load(std::sync::atomic::Ordering::Relaxed) < count {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap_or_else(|_| panic!("catalog {count} never reached the follower"));
            }
        };
        heard(1).await;
        for count in 2..=4 {
            assert!(super::super::fresh::rewrite(&mut catalog));
            heard(count).await;
        }
        assert_eq!(counted.plays.load(std::sync::atomic::Ordering::Relaxed), 1, "one player, never replaced");
        following.abort();
    }

    #[test]
    fn what_rate_control_rewrites_is_the_same_rendition() {
        let before = json!({"codec": "avc3.42e01f", "codedWidth": 640, "codedHeight": 360, "bitrate": 900_000, "framerate": 30.0});
        let after = json!({"codec": "avc3.42e01f", "codedWidth": 640, "codedHeight": 360, "bitrate": 400_000, "stalled": true});
        assert_eq!(Rendition::new("video.avc3", &before), Rendition::new("video.avc3", &after));
    }

    #[test]
    fn what_a_decoder_opens_from_is_a_new_rendition() {
        let base = json!({"codec": "avc3.42e01f", "codedWidth": 640, "codedHeight": 360});
        for changed in [
            json!({"codec": "avc3.64001f", "codedWidth": 640, "codedHeight": 360}),
            json!({"codec": "avc3.42e01f", "codedWidth": 1280, "codedHeight": 720}),
            json!({"codec": "avc3.42e01f", "codedWidth": 640, "codedHeight": 360, "description": "0142"}),
        ] {
            assert_ne!(Rendition::new("video.avc3", &base), Rendition::new("video.avc3", &changed), "{changed}");
        }
        assert_ne!(Rendition::new("a", &base), Rendition::new("b", &base), "another track");
    }

    #[test]
    fn audio_is_told_apart_by_its_rate_and_channels() {
        let mono = json!({"codec": "opus", "sampleRate": 48_000, "channelCount": 1, "bitrate": 32_000});
        let stereo = json!({"codec": "opus", "sampleRate": 48_000, "channelCount": 2, "bitrate": 32_000});
        assert_ne!(Rendition::new("audio", &mono), Rendition::new("audio", &stereo));
    }
}
