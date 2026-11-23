//! How good a connection is, as peers tell each other: the two presence
//! fields that carry it, what "good", "held" and "poor" mean, and the
//! arithmetic the grid in the Network tab is drawn from.
//!
//! **A claim, like a display name.** Every figure here is self-reported by a
//! peer we cannot check, so it is rounded, clamped and cleaned on the way out
//! *and* on the way in ([`Link::clean`], [`clean_seen`]), keyed only on endpoint
//! ids, and a malformed field costs only itself: [`lenient_link`] and
//! [`lenient_seen`] turn anything they cannot read into "not reporting", since
//! one parse error in a presence document ends listening to that peer.
//!
//! **Rounded so wobble does not republish.** The document goes out with every
//! clock refresh anyway, but only changed fields go on the wire, and a figure
//! that moves by a unit every second would make every patch carry it. Two
//! significant figures for rates, 10 ms for a round trip, coarse steps for loss,
//! 5 for a frame rate, 50 ms for lateness.
//!
//! **The quality state is what moves the page, so it alone has hysteresis**
//! ([`Judge`]): worse at once, better only after [`BETTER_AFTER`] steady. The
//! bars on a tile are read from it, and a tile wakes the page only when what
//! it reads changes.
//!
//! The thresholds are first guesses, to be tuned against real bad calls.
//!
//! Pure: no clock of its own (times are passed in), no atomics, no Dioxus.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use crate::rates::{self, Point};

/// One summary of a link, or of what one receiver gets from one sender.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Good,
    /// The link cannot carry everything asked of it, or is losing a little.
    Held,
    Poor,
    /// Nothing to judge by; also any word from a later build.
    #[default]
    #[serde(other)]
    Unknown,
}

impl Quality {
    /// Worse is larger; `None` for unknown, which is no position at all.
    fn rank(self) -> Option<u8> {
        match self {
            Quality::Good => Some(0),
            Quality::Held => Some(1),
            Quality::Poor => Some(2),
            Quality::Unknown => None,
        }
    }

    /// The worse of two, where unknown yields to anything known.
    fn worst(self, other: Quality) -> Quality {
        match (self.rank(), other.rank()) {
            (None, _) => other,
            (_, None) => self,
            (Some(a), Some(b)) => {
                if b > a {
                    other
                } else {
                    self
                }
            }
        }
    }

    /// In words, for a sentence.
    pub fn words(self) -> &'static str {
        match self {
            Quality::Good => "good",
            Quality::Held => "held by the link",
            Quality::Poor => "poor",
            Quality::Unknown => "unknown",
        }
    }
}

/// One outgoing stream against its cap, as its own rate loop judges it. Detail
/// for the Network tab only: since the send budget is shared, a camera is
/// routinely held just because a screen share is served first, which says
/// nothing about the connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamState {
    /// Sending at (or within a few percent of) its cap.
    Cap,
    Held,
    #[default]
    #[serde(other)]
    Unknown,
}

/// Which way an iroh call's traffic goes, as the wire says it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    Direct,
    Relayed,
    Mixed,
    #[default]
    #[serde(other)]
    Unknown,
}

/// A peer's own report on its link: the `link` field of its presence.
///
/// Rates in kbps, times in ms, loss in tenths of a percent. Every field is
/// optional on the wire and absent when unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Link {
    pub q: Quality,
    /// The transport's estimate of what the link carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub est: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtt: Option<u32>,
    /// Loss over the last few seconds, in tenths of a percent, floored to a
    /// step of [`LOSS_STEPS`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loss: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cam: Option<StreamState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share: Option<StreamState>,
    /// The camera frame rate we send.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Route>,
    /// What each video stream sent, averaged since the last report, and its
    /// cap: enough for a receiver to chart this sender over time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cam_sent: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cam_cap: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_sent: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_cap: Option<u32>,
}

/// What one receiver gets from one sender: an entry of the `seen` field.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Seen {
    pub q: Quality,
    /// Frames a second received.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// How much later than its earliest their video arrives: clock-free, so
    /// it is what the quality is judged on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u32>,
    /// Capture to arrival, by both machines' wall clocks: only as good as
    /// their NTP, so shown and never judged on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<u32>,
}

/// Loss steps, in tenths of a percent: 0, 0.5, 1, 2, 3, 5, 10, 20, 50 %.
pub const LOSS_STEPS: [u32; 9] = [0, 5, 10, 20, 30, 50, 100, 200, 500];

/// At most this many entries in a `seen` map, ours or a peer's.
pub const SEEN_MAX: usize = 16;

/// Bounds no real link reaches: anything above is a broken or hostile sender.
const MAX_KBPS: u32 = 10_000_000;
const MAX_MS: u32 = 60_000;
const MAX_FPS: u32 = 240;

fn round_to(value: u32, step: u32) -> u32 {
    let step = u64::from(step.max(1));
    (((u64::from(value) + step / 2) / step) * step).min(u64::from(u32::MAX)) as u32
}

/// Two significant figures: 2345 is 2300, 87 stays 87.
pub fn sig2(value: u32) -> u32 {
    let mut unit = 1u32;
    while value / unit >= 100 {
        unit = unit.saturating_mul(10);
    }
    round_to(value, unit)
}

/// A round trip to 10 ms, never rounded down to nothing: a measured 4 ms is
/// not "no round trip".
fn round_rtt(ms: u32) -> u32 {
    match ms {
        0 => 0,
        ms => round_to(ms.min(MAX_MS), 10).max(10),
    }
}

/// Loss floored to its step, so "at least 5%" survives the rounding.
pub fn loss_step(tenths: u32) -> u32 {
    LOSS_STEPS.iter().copied().rfind(|step| *step <= tenths).unwrap_or(0)
}

impl Link {
    /// Rounded and clamped: what we send, and what we accept.
    pub fn clean(self) -> Self {
        let kbps = |value: Option<u32>| value.map(|value| sig2(value.min(MAX_KBPS)).min(MAX_KBPS));
        Self {
            q: self.q,
            est: kbps(self.est),
            rtt: self.rtt.map(round_rtt),
            loss: self.loss.map(|loss| loss_step(loss.min(1000))),
            cam: self.cam,
            share: self.share,
            fps: self.fps.map(|fps| round_to(fps.min(MAX_FPS), 5)),
            path: self.path.filter(|path| *path != Route::Unknown),
            cam_sent: kbps(self.cam_sent),
            cam_cap: kbps(self.cam_cap),
            share_sent: kbps(self.share_sent),
            share_cap: kbps(self.share_cap),
        }
    }
}

impl Seen {
    pub fn clean(self) -> Self {
        let ms = |value: Option<u32>| value.map(|value| round_to(value.min(MAX_MS), 50));
        Self {
            q: self.q,
            fps: self.fps.map(|fps| round_to(fps.min(MAX_FPS), 5)),
            ms: ms(self.ms),
            delay: ms(self.delay),
        }
    }
}

/// Whether `id` is an endpoint id as iroh writes one: 64 lowercase hex.
pub fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// A `seen` map as we keep or send it: well-formed ids other than the
/// sender's own, each entry cleaned, at most [`SEEN_MAX`].
pub fn clean_seen(seen: BTreeMap<String, Seen>, sender: &str) -> BTreeMap<String, Seen> {
    seen.into_iter()
        .filter(|(id, _)| valid_id(id) && id != sender)
        .take(SEEN_MAX)
        .map(|(id, seen)| (id, seen.clean()))
        .collect()
}

/// `link` as it comes off the wire: anything unreadable is "not reporting",
/// never a failed document.
pub fn lenient_link<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Link>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    Ok(serde_json::from_value::<Link>(value).ok().map(Link::clean))
}

/// `seen` as it comes off the wire: entries that do not read are dropped one
/// by one, and the rest are cleaned.
pub fn lenient_seen<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<BTreeMap<String, Seen>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    let serde_json::Value::Object(entries) = value else {
        return Ok(BTreeMap::new());
    };
    let parsed = entries
        .into_iter()
        .filter(|(id, _)| valid_id(id))
        .filter_map(|(id, entry)| Some((id, serde_json::from_value::<Seen>(entry).ok()?)))
        .collect();
    Ok(clean_seen(parsed, ""))
}

/// What a link's quality is judged from: rates in kbps, loss in tenths of a
/// percent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Inputs {
    pub rtt: Option<u32>,
    /// Over the last few seconds, not since the connection opened.
    pub loss: Option<u32>,
    pub estimate: Option<u32>,
    /// What we ask the link to carry: the caps of the video streams we are
    /// sending, added up. Audio is left out; it is small and always served
    /// first.
    pub ask: u32,
}

/// Poor from this much recent loss (5 %), held from [`LOSS_HELD`] (2 %).
const LOSS_POOR: u32 = 50;
const LOSS_HELD: u32 = 20;
/// Poor from this round trip.
const RTT_POOR: u32 = 400;
/// Poor under this estimate, while sending video.
const ESTIMATE_POOR: u32 = 300;
/// Poor when the link carries this share of what we ask or less.
const CARRIES_POOR: f64 = 0.30;
/// Held under this share of what we ask: the rate loop's own 5 % deadband,
/// so an estimate a hair under the caps is not called held.
const CARRIES_HELD: f64 = 0.95;

/// A link's quality now, before hysteresis.
///
/// **Held is judged on the whole link**, the estimate against everything we
/// ask to send, and never on one stream's state: since the budget is shared
/// a camera is held whenever a share is served first.
///
/// The estimate tests apply only while video is being sent. A link carrying
/// nothing but audio has no reason to have measured a large estimate
/// (inferred: an estimator that is application-limited reports roughly what
/// it is given), and that is not a poor link.
pub fn classify(inputs: Inputs) -> Quality {
    let Inputs { rtt, loss, estimate, ask } = inputs;
    let sending = ask > 0;
    let carries = |share: f64| {
        sending && estimate.is_some_and(|estimate| f64::from(estimate) < f64::from(ask) * share)
    };
    let poor = loss.is_some_and(|loss| loss >= LOSS_POOR)
        || rtt.is_some_and(|rtt| rtt >= RTT_POOR)
        || (sending && estimate.is_some_and(|estimate| estimate < ESTIMATE_POOR))
        || (sending && estimate.is_some_and(|estimate| f64::from(estimate) <= f64::from(ask) * CARRIES_POOR));
    if poor {
        return Quality::Poor;
    }
    if loss.is_some_and(|loss| loss >= LOSS_HELD) || carries(CARRIES_HELD) {
        return Quality::Held;
    }
    if rtt.is_some() { Quality::Good } else { Quality::Unknown }
}

/// What one receiver gets from one sender, before hysteresis.
///
/// The frame rate is judged against the rate the sender says it sends, so a
/// camera that only gives 15 fps in a dim room is not a bad link; a sender
/// that does not say is judged on lateness alone.
pub fn cell(received_fps: Option<f32>, sender_fps: Option<u32>, late_ms: Option<u32>, camera_on: bool) -> Quality {
    if !camera_on {
        return Quality::Unknown;
    }
    let by_rate = match (received_fps, sender_fps) {
        (Some(got), Some(sent)) if sent > 0 => {
            let share = got / sent as f32;
            if share < 0.5 {
                Quality::Poor
            } else if share < 0.8 {
                Quality::Held
            } else {
                Quality::Good
            }
        }
        _ => Quality::Unknown,
    };
    let by_lateness = match late_ms {
        Some(ms) if ms >= 400 => Quality::Poor,
        Some(ms) if ms >= 200 => Quality::Held,
        Some(_) => Quality::Good,
        None => Quality::Unknown,
    };
    by_rate.worst(by_lateness)
}

/// How long a better reading has to hold before it is shown.
pub const BETTER_AFTER: Duration = Duration::from_secs(5);

/// A quality with hysteresis: worse is shown at once, better only once it has
/// held for [`BETTER_AFTER`]. Unknown, either way, is taken at once: it is
/// not a position on the scale.
#[derive(Clone, Copy, Debug, Default)]
pub struct Judge {
    shown: Quality,
    better_since: Option<Instant>,
}

impl Judge {
    pub fn shown(&self) -> Quality {
        self.shown
    }

    pub fn observe(&mut self, raw: Quality, now: Instant) -> Quality {
        match (raw.rank(), self.shown.rank()) {
            (Some(next), Some(shown)) if next < shown => {
                let since = *self.better_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= BETTER_AFTER {
                    self.shown = raw;
                    self.better_since = None;
                }
            }
            _ => {
                self.shown = raw;
                self.better_since = None;
            }
        }
        self.shown
    }
}

/// Loss over the last few seconds, from the transport's running totals.
///
/// QUIC counts packets lost and sent since the connection opened, so a call
/// that struggled once keeps its figure for good. This keeps the last
/// [`LOSS_SAMPLES`] totals, one a second, and reports the difference across
/// them: about six seconds, a few reports' worth, so one lost packet in a
/// quiet couple of seconds is not a spike.
#[derive(Clone, Debug, Default)]
pub struct LossWindow {
    samples: VecDeque<(u64, u64)>,
}

/// Seven samples a second apart span six seconds.
const LOSS_SAMPLES: usize = 7;

impl LossWindow {
    /// The latest `(lost, sent)` totals; `None` when the transport has none.
    pub fn push(&mut self, totals: Option<(u64, u64)>) {
        let Some((lost, sent)) = totals else {
            self.samples.clear();
            return;
        };
        // Totals that went backwards are a new connection: start again.
        if self.samples.back().is_some_and(|&(was_lost, was_sent)| lost < was_lost || sent < was_sent) {
            self.samples.clear();
        }
        if self.samples.len() == LOSS_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back((lost, sent));
    }

    /// Loss across the window in tenths of a percent, unstepped; `None`
    /// until two samples apart, or with nothing sent between them.
    pub fn recent(&self) -> Option<u32> {
        let (&(lost0, sent0), &(lost1, sent1)) = (self.samples.front()?, self.samples.back()?);
        let sent = sent1.checked_sub(sent0).filter(|sent| *sent > 0)?;
        let lost = lost1.saturating_sub(lost0);
        Some((lost.saturating_mul(1000) / sent).min(1000) as u32)
    }
}

/// Which rows ("their upload?") and columns ("their download?") of a grid
/// look like one person's fault. `grid[sender][receiver]`; the diagonal and
/// missing cells are `None` and do not count.
///
/// Half or more of the known cells poor, and at least two: a single bad cell
/// stays a single cell, since one poor reading between two people says
/// nothing about which end it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blame {
    pub rows: Vec<bool>,
    pub columns: Vec<bool>,
}

pub fn blame(grid: &[Vec<Option<Quality>>]) -> Blame {
    let flagged = |cells: Vec<Quality>| {
        let known = cells.iter().filter(|q| q.rank().is_some()).count();
        let poor = cells.iter().filter(|q| **q == Quality::Poor).count();
        poor >= 2 && poor * 2 >= known
    };
    let size = grid.len();
    let at = |row: usize, column: usize| {
        if row == column {
            None
        } else {
            grid.get(row).and_then(|cells| cells.get(column)).copied().flatten()
        }
    };
    Blame {
        rows: (0..size).map(|row| flagged((0..size).filter_map(|column| at(row, column)).collect())).collect(),
        columns: (0..size)
            .map(|column| flagged((0..size).filter_map(|row| at(row, column)).collect()))
            .collect(),
    }
}

/// How many reports a peer's history keeps: two minutes at one every two
/// seconds.
pub const HISTORY: usize = 60;

/// How often a report is taken: the presence document's own clock refresh,
/// which goes out regardless.
pub const REPORT_EVERY: Duration = Duration::from_secs(2);

/// The last [`HISTORY`] reports from one peer, oldest first, one point per
/// report time. A point where they were not reporting is `None`, a gap, never
/// a repeat of the last one.
#[derive(Clone, Debug, Default)]
pub struct Reports {
    points: VecDeque<Option<Link>>,
}

impl Reports {
    /// Notes their report as it stands. Gaps before the first report are not
    /// kept: an empty history says "nothing to chart".
    pub fn push(&mut self, link: Option<Link>) {
        if self.points.is_empty() && link.is_none() {
            return;
        }
        if self.points.len() == HISTORY {
            self.points.pop_front();
        }
        self.points.push_back(link);
    }

    pub fn clear(&mut self) {
        self.points.clear();
    }

    pub fn points(&self) -> Vec<Option<Link>> {
        self.points.iter().cloned().collect()
    }
}

fn add(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
    }
}

/// One report as a chart point: everything sent, against the caps added up,
/// with the link's estimate as the dashed line.
pub fn chart_point(link: Option<&Link>) -> Point {
    match link {
        None => Point::default(),
        Some(link) => Point {
            sent: add(link.cam_sent, link.share_sent),
            cap: add(link.cam_cap, link.share_cap),
            target: None,
            link: link.est,
        },
    }
}

/// Loss in words, from its step: `under 0.5%` for nothing, a range above.
pub fn loss_words(tenths: u32) -> String {
    let step = loss_step(tenths);
    let percent = |tenths: u32| {
        if tenths % 10 == 0 { format!("{}", tenths / 10) } else { format!("{}.{}", tenths / 10, tenths % 10) }
    };
    match LOSS_STEPS.iter().position(|value| *value == step) {
        Some(0) => "recent loss under 0.5%".to_string(),
        Some(index) if index + 1 < LOSS_STEPS.len() => {
            format!("recent loss {}\u{2013}{}%", percent(step), percent(LOSS_STEPS[index + 1]))
        }
        _ => format!("recent loss {}% or more", percent(step)),
    }
}

/// The round trip and loss of one report, for the line under a peer's chart.
pub fn note(link: &Link) -> String {
    let mut parts = Vec::new();
    if let Some(rtt) = link.rtt {
        parts.push(format!("round trip {rtt} ms"));
    }
    if let Some(loss) = link.loss {
        parts.push(loss_words(loss));
    }
    parts.join("  \u{b7}  ")
}

fn stream_words(what: &str, state: StreamState) -> String {
    match state {
        StreamState::Cap => format!("{what} at its cap"),
        StreamState::Held => format!("{what} held by the link"),
        StreamState::Unknown => format!("{what} not yet judged"),
    }
}

/// A report's figures in words, each with its unit.
pub fn figures(link: &Link) -> Vec<String> {
    let mut parts = Vec::new();
    match link.est {
        Some(est) => parts.push(format!("link estimate {}", rates::kbps_words(est))),
        None => parts.push("no link estimate".to_string()),
    }
    match link.rtt {
        Some(rtt) => parts.push(format!("round trip {rtt} ms")),
        None => parts.push("round trip unknown".to_string()),
    }
    if let Some(loss) = link.loss {
        parts.push(loss_words(loss));
    }
    match link.fps {
        Some(fps) => parts.push(format!("sending {fps} fps")),
        None => parts.push("camera not sending".to_string()),
    }
    if let Some(state) = link.cam {
        parts.push(stream_words("camera", state));
    }
    if let Some(state) = link.share {
        parts.push(stream_words("screen share", state));
    }
    match link.path {
        Some(Route::Direct) => parts.push("direct path".to_string()),
        Some(Route::Relayed) => parts.push("through an iroh relay".to_string()),
        Some(Route::Mixed) => parts.push("direct and relayed".to_string()),
        Some(Route::Unknown) | None => {}
    }
    parts
}

/// The tile menu's line about a peer: their own report, and what we get from
/// them, e.g. "Connection: held by the link · you get 28 fps, about 150 ms
/// behind". `None` when they are not reporting.
pub fn connection_line(theirs: Option<&Link>, ours_of_them: Option<&Seen>) -> Option<String> {
    let theirs = theirs?;
    let mut line = format!("Connection: {}", theirs.q.words());
    if let Some(seen) = ours_of_them {
        let mut got = Vec::new();
        if let Some(fps) = seen.fps {
            got.push(format!("you get {fps} fps"));
        }
        if let Some(delay) = seen.delay {
            got.push(format!("about {delay} ms behind"));
        }
        if !got.is_empty() {
            line.push_str(&format!(" \u{b7} {}", got.join(", ")));
        }
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    #[test]
    fn rounding_keeps_two_figures_and_coarse_steps() {
        assert_eq!(sig2(2345), 2300);
        assert_eq!(sig2(2351), 2400);
        assert_eq!(sig2(87), 87);
        assert_eq!(sig2(38_961), 39_000);
        assert_eq!(round_rtt(4), 10);
        assert_eq!(round_rtt(44), 40);
        assert_eq!(round_rtt(45), 50);
        assert_eq!(loss_step(0), 0);
        assert_eq!(loss_step(4), 0);
        assert_eq!(loss_step(49), 30);
        assert_eq!(loss_step(50), 50);
        assert_eq!(loss_step(1000), 500);
    }

    #[test]
    fn a_report_is_clamped_and_rounded_both_ways() {
        let wild = Link {
            q: Quality::Good,
            est: Some(u32::MAX),
            rtt: Some(u32::MAX),
            loss: Some(u32::MAX),
            fps: Some(9999),
            path: Some(Route::Unknown),
            cam_sent: Some(1234),
            ..Link::default()
        };
        let clean = wild.clean();
        assert_eq!(clean.est, Some(MAX_KBPS));
        assert_eq!(clean.rtt, Some(MAX_MS));
        assert_eq!(clean.loss, Some(500));
        assert_eq!(clean.fps, Some(240));
        assert_eq!(clean.path, None, "an unknown path is not said");
        assert_eq!(clean.cam_sent, Some(1200));
        assert_eq!(clean.clone().clean(), clean, "cleaning twice changes nothing");
        let seen = Seen { fps: Some(28), ms: Some(170), delay: Some(u32::MAX), ..Seen::default() }.clean();
        assert_eq!((seen.fps, seen.ms, seen.delay), (Some(30), Some(150), Some(MAX_MS)));
    }

    #[test]
    fn only_well_formed_ids_of_others_are_kept_and_at_most_sixteen() {
        let mut seen = BTreeMap::new();
        seen.insert(id('a'), Seen::default());
        seen.insert("A".repeat(64), Seen::default());
        seen.insert("short".to_string(), Seen::default());
        seen.insert(id('b'), Seen::default());
        let kept = clean_seen(seen, &id('b'));
        assert_eq!(kept.keys().cloned().collect::<Vec<_>>(), vec![id('a')], "uppercase, short and the sender's own go");

        let many: BTreeMap<String, Seen> = (0..40u32).map(|n| (format!("{n:064x}"), Seen::default())).collect();
        assert_eq!(clean_seen(many, "").len(), SEEN_MAX);
    }

    #[test]
    fn unknown_words_are_unknown_not_errors() {
        let link: Link = serde_json::from_str(r#"{"q":"splendid","cam":"turbo","path":"carrier-pigeon","est":900}"#).unwrap();
        assert_eq!((link.q, link.cam, link.path, link.est), (Quality::Unknown, Some(StreamState::Unknown), Some(Route::Unknown), Some(900)));
        let seen: Seen = serde_json::from_str(r#"{"q":"meh","extra":1}"#).unwrap();
        assert_eq!(seen.q, Quality::Unknown);
    }

    #[test]
    fn nothing_known_is_unknown() {
        assert_eq!(classify(Inputs::default()), Quality::Unknown);
        assert_eq!(classify(Inputs { estimate: Some(5000), ..Inputs::default() }), Quality::Unknown, "good needs a round trip");
        assert_eq!(classify(Inputs { rtt: Some(30), ..Inputs::default() }), Quality::Good);
    }

    #[test]
    fn the_boundaries_are_where_they_say() {
        let base = Inputs { rtt: Some(30), loss: Some(0), estimate: Some(5000), ask: 2500 };
        assert_eq!(classify(base), Quality::Good);
        assert_eq!(classify(Inputs { loss: Some(19), ..base }), Quality::Good);
        assert_eq!(classify(Inputs { loss: Some(20), ..base }), Quality::Held);
        assert_eq!(classify(Inputs { loss: Some(49), ..base }), Quality::Held);
        assert_eq!(classify(Inputs { loss: Some(50), ..base }), Quality::Poor);
        assert_eq!(classify(Inputs { rtt: Some(399), ..base }), Quality::Good);
        assert_eq!(classify(Inputs { rtt: Some(400), ..base }), Quality::Poor);
        // The link against everything asked of it.
        assert_eq!(classify(Inputs { estimate: Some(2400), ..base }), Quality::Good, "within the 5% deadband");
        assert_eq!(classify(Inputs { estimate: Some(2300), ..base }), Quality::Held);
        assert_eq!(classify(Inputs { estimate: Some(751), ..base }), Quality::Held);
        assert_eq!(classify(Inputs { estimate: Some(750), ..base }), Quality::Poor, "30% of the ask");
        assert_eq!(classify(Inputs { estimate: Some(299), ask: 400, ..base }), Quality::Poor);
        // Not sending video: a small estimate is no news.
        assert_eq!(classify(Inputs { estimate: Some(100), ask: 0, ..base }), Quality::Good);
    }

    #[test]
    fn a_camera_held_only_by_its_own_share_is_not_held() {
        // Camera cap 2500, share cap 6000; the link carries 9000. The
        // camera's own rate loop may be holding it while the share is served
        // first, but the link has room for everything asked: good.
        let shared = Inputs { rtt: Some(30), loss: Some(0), estimate: Some(9000), ask: 2500 + 6000 };
        assert_eq!(classify(shared), Quality::Good);
        // The same two streams on a link that cannot carry both: held.
        assert_eq!(classify(Inputs { estimate: Some(7000), ..shared }), Quality::Held);
    }

    #[test]
    fn worse_at_once_better_after_five_steady_seconds() {
        let start = Instant::now();
        let at = |s: u64| start + Duration::from_secs(s);
        let mut judge = Judge::default();
        assert_eq!(judge.observe(Quality::Good, at(0)), Quality::Good, "out of unknown at once");
        assert_eq!(judge.observe(Quality::Poor, at(1)), Quality::Poor, "worse at once");
        assert_eq!(judge.observe(Quality::Good, at(2)), Quality::Poor);
        assert_eq!(judge.observe(Quality::Held, at(4)), Quality::Poor, "still better, the clock keeps running");
        assert_eq!(judge.observe(Quality::Good, at(7)), Quality::Good, "five seconds of better");
        // A relapse restarts the wait.
        assert_eq!(judge.observe(Quality::Held, at(8)), Quality::Held);
        assert_eq!(judge.observe(Quality::Good, at(9)), Quality::Held);
        assert_eq!(judge.observe(Quality::Held, at(10)), Quality::Held);
        assert_eq!(judge.observe(Quality::Good, at(11)), Quality::Held);
        assert_eq!(judge.observe(Quality::Good, at(15)), Quality::Held);
        assert_eq!(judge.observe(Quality::Good, at(16)), Quality::Good);
        assert_eq!(judge.observe(Quality::Unknown, at(17)), Quality::Unknown);
    }

    #[test]
    fn a_cell_is_rate_against_what_was_sent_and_lateness() {
        assert_eq!(cell(Some(29.0), Some(30), Some(0), true), Quality::Good);
        assert_eq!(cell(Some(23.0), Some(30), Some(0), true), Quality::Held, "under 80%");
        assert_eq!(cell(Some(14.0), Some(30), Some(0), true), Quality::Poor, "under 50%");
        assert_eq!(cell(Some(15.0), Some(15), Some(0), true), Quality::Good, "a slow camera is not a slow link");
        assert_eq!(cell(Some(30.0), Some(30), Some(200), true), Quality::Held);
        assert_eq!(cell(Some(30.0), Some(30), Some(400), true), Quality::Poor);
        assert_eq!(cell(Some(30.0), None, Some(50), true), Quality::Good, "an older sender: lateness alone");
        assert_eq!(cell(None, None, None, true), Quality::Unknown);
        assert_eq!(cell(Some(0.0), Some(30), Some(900), false), Quality::Unknown, "camera off");
    }

    #[test]
    fn recent_loss_is_windowed_and_survives_a_new_connection() {
        let mut window = LossWindow::default();
        assert_eq!(window.recent(), None);
        window.push(Some((100, 10_000)));
        assert_eq!(window.recent(), None, "one sample is no window");
        window.push(Some((110, 11_000)));
        assert_eq!(window.recent(), Some(10), "10 of 1000: 1%");
        for second in 2..=10u64 {
            window.push(Some((110, 10_000 + second * 1000)));
        }
        assert_eq!(window.recent(), Some(0), "the old loss has left the window");
        // A reconnect restarts the totals.
        window.push(Some((0, 50)));
        assert_eq!(window.recent(), None);
        window.push(Some((5, 150)));
        assert_eq!(window.recent(), Some(50));
        window.push(None);
        assert_eq!(window.recent(), None);
    }

    fn grid(cells: &[&[Option<Quality>]]) -> Vec<Vec<Option<Quality>>> {
        cells.iter().map(|row| row.to_vec()).collect()
    }

    #[test]
    fn a_bad_row_is_their_upload_and_a_bad_column_their_download() {
        use Quality::{Good as G, Poor as P};
        // Person 0's video is poor for everyone: their upload.
        let rows = grid(&[
            &[None, Some(P), Some(P), Some(P)],
            &[Some(G), None, Some(G), Some(G)],
            &[Some(G), Some(G), None, Some(G)],
            &[Some(G), Some(G), Some(G), None],
        ]);
        assert_eq!(blame(&rows), Blame { rows: vec![true, false, false, false], columns: vec![false; 4] });
        // Person 3 gets everyone poorly: their download.
        let column = grid(&[
            &[None, Some(G), Some(G), Some(P)],
            &[Some(G), None, Some(G), Some(P)],
            &[Some(G), Some(G), None, Some(P)],
            &[Some(G), Some(G), Some(G), None],
        ]);
        assert_eq!(blame(&column), Blame { rows: vec![false; 4], columns: vec![false, false, false, true] });
    }

    #[test]
    fn a_single_bad_cell_stays_a_single_cell() {
        use Quality::{Good as G, Poor as P};
        let one = grid(&[&[None, Some(P), Some(G)], &[Some(G), None, Some(G)], &[Some(G), Some(G), None]]);
        assert_eq!(blame(&one), Blame { rows: vec![false; 3], columns: vec![false; 3] });
        // Two people: one cell each way, never enough to blame anyone.
        let two = grid(&[&[None, Some(P)], &[Some(P), None]]);
        assert_eq!(blame(&two), Blame { rows: vec![false; 2], columns: vec![false; 2] });
        // Unknown cells do not dilute: two poor of two known is a row.
        let unknown = grid(&[&[None, Some(P), Some(P), None], &[None; 4], &[None; 4], &[None; 4]]);
        assert!(blame(&unknown).rows[0]);
    }

    #[test]
    fn a_history_keeps_gaps_and_its_length() {
        let mut reports = Reports::default();
        reports.push(None);
        assert!(reports.points().is_empty(), "nothing before the first report");
        let report = Link { est: Some(2000), ..Link::default() };
        reports.push(Some(report.clone()));
        reports.push(None);
        reports.push(Some(report.clone()));
        assert_eq!(reports.points(), vec![Some(report.clone()), None, Some(report.clone())], "a missing report is a gap");
        for _ in 0..HISTORY * 2 {
            reports.push(Some(report.clone()));
        }
        assert_eq!(reports.points().len(), HISTORY);
        reports.clear();
        assert!(reports.points().is_empty());
    }

    #[test]
    fn a_report_is_charted_as_its_totals_against_the_link() {
        let link = Link {
            est: Some(9000),
            cam_sent: Some(1800),
            cam_cap: Some(2500),
            share_sent: Some(3000),
            share_cap: Some(6000),
            ..Link::default()
        };
        assert_eq!(chart_point(Some(&link)), Point { sent: Some(4800), cap: Some(8500), target: None, link: Some(9000) });
        let camera_only = Link { cam_sent: Some(900), cam_cap: Some(2500), ..Link::default() };
        assert_eq!(chart_point(Some(&camera_only)), Point { sent: Some(900), cap: Some(2500), target: None, link: None });
        assert_eq!(chart_point(None), Point::default(), "no report is a gap");
    }

    #[test]
    fn figures_are_words_with_units() {
        let link = Link {
            q: Quality::Held,
            est: Some(2400),
            rtt: Some(40),
            loss: Some(20),
            cam: Some(StreamState::Held),
            share: Some(StreamState::Cap),
            fps: Some(30),
            path: Some(Route::Relayed),
            ..Link::default()
        };
        assert_eq!(
            figures(&link),
            vec![
                "link estimate 2400 kbps",
                "round trip 40 ms",
                "recent loss 2\u{2013}3%",
                "sending 30 fps",
                "camera held by the link",
                "screen share at its cap",
                "through an iroh relay",
            ]
        );
        assert_eq!(loss_words(0), "recent loss under 0.5%");
        assert_eq!(loss_words(5), "recent loss 0.5\u{2013}1%");
        assert_eq!(loss_words(700), "recent loss 50% or more");
        assert_eq!(note(&link), "round trip 40 ms  \u{b7}  recent loss 2\u{2013}3%");
    }

    #[test]
    fn the_tile_menu_line_says_both_ends() {
        let theirs = Link { q: Quality::Held, ..Link::default() };
        let ours = Seen { fps: Some(28), delay: Some(150), ..Seen::default() };
        assert_eq!(
            connection_line(Some(&theirs), Some(&ours)).as_deref(),
            Some("Connection: held by the link \u{b7} you get 28 fps, about 150 ms behind")
        );
        assert_eq!(connection_line(Some(&theirs), None).as_deref(), Some("Connection: held by the link"));
        assert_eq!(connection_line(None, Some(&ours)), None, "not reporting: no line");
    }
}
