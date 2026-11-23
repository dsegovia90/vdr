//! The last minute of what each outgoing stream sent, and the arithmetic that
//! turns it into a chart: which value sits at the top, and how many pixels a
//! rate is.
//!
//! **Kept off the interface, filled where the sent rates are measured.** The
//! once-a-second meter that turns byte counts into rates also appends a
//! [`Second`] here, so opening the Network tab mid-call shows the minute that
//! has already gone by rather than starting from nothing.
//!
//! Pure: no clock, no atomics, no Dioxus.

use std::collections::VecDeque;

/// How many seconds a chart shows.
pub const SECONDS: usize = 60;

/// How many the history keeps: the minute shown, and the few seconds before
/// it that the oldest shown second is averaged with. Without them the left
/// edge of a chart is one raw second, then an average of two, and so on, and
/// it lurches every second as a different raw value scrolls into that place.
const KEPT: usize = SECONDS + SMOOTH - 1;

/// One second of the outgoing streams, in kbps. `None` is "not known", which
/// is not the same as zero: a stalled encoder sends `Some(0)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Second {
    pub camera_sent: Option<u32>,
    pub camera_cap: Option<u32>,
    /// What the rate loop asked the camera's encoder for.
    pub camera_target: Option<u32>,
    pub share_sent: Option<u32>,
    pub share_cap: Option<u32>,
    /// The transport's estimate for the whole connection.
    pub estimate: Option<u32>,
}

impl Second {
    fn is_empty(&self) -> bool {
        *self == Second::default()
    }

    /// Everything sent this second, or `None` when neither stream measured.
    pub fn total_sent(&self) -> Option<u32> {
        add(self.camera_sent, self.share_sent)
    }

    /// The streams' caps added up, or `None` with no stream.
    pub fn total_cap(&self) -> Option<u32> {
        add(self.camera_cap, self.share_cap)
    }
}

fn add(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
    }
}

/// The last [`KEPT`] seconds, oldest first.
#[derive(Debug, Default)]
pub struct History {
    seconds: VecDeque<Second>,
}

impl History {
    /// Appends a second, forgetting the oldest once full.
    ///
    /// Seconds that know nothing are skipped until one knows something: a
    /// call connecting has no encoder yet, and an empty history is what says
    /// "nothing to show" without every reader having to look inside it.
    pub fn push(&mut self, second: Second) {
        if self.seconds.is_empty() && second.is_empty() {
            return;
        }
        if self.seconds.len() == KEPT {
            self.seconds.pop_front();
        }
        self.seconds.push_back(second);
    }

    pub fn clear(&mut self) {
        self.seconds.clear();
    }

    /// A copy, oldest first.
    pub fn seconds(&self) -> Vec<Second> {
        self.seconds.iter().copied().collect()
    }
}

/// One second as one chart draws it, in kbps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    /// What was sent, the filled line.
    pub sent: Option<u32>,
    /// The hard line, and part of the scale.
    pub cap: Option<u32>,
    pub target: Option<u32>,
    /// Drawn only where it fits; never part of the scale, or a generous link
    /// would flatten everything else.
    pub estimate: Option<u32>,
}

impl Point {
    pub fn camera(second: &Second) -> Self {
        Self {
            sent: second.camera_sent,
            cap: second.camera_cap,
            target: second.camera_target,
            estimate: second.estimate,
        }
    }

    pub fn share(second: &Second) -> Self {
        Self {
            sent: second.share_sent,
            cap: second.share_cap,
            target: None,
            estimate: None,
        }
    }

    /// Everything sent against the link. The cap is the caps added up, which
    /// sets the scale; the chart does not draw it as a line.
    pub fn total(second: &Second) -> Self {
        Self {
            sent: second.total_sent(),
            cap: second.total_cap(),
            target: None,
            estimate: second.estimate,
        }
    }
}

/// `points` laid into [`SECONDS`] slots, newest at the right, empty slots on
/// the left until the history is full. Keeps the time axis fixed, so a bar's
/// position always means the same age.
pub fn slots(points: &[Point]) -> Vec<Option<Point>> {
    let points = &points[points.len().saturating_sub(SECONDS)..];
    let mut slots = vec![None; SECONDS - points.len()];
    slots.extend(points.iter().copied().map(Some));
    slots
}

/// The value at the top of a chart, in kbps: the larger of the highest cap
/// and the highest rate sent or targeted, with a tenth of headroom, rounded up
/// to a figure that reads as a scale.
pub fn top(points: &[Point]) -> u32 {
    let highest = points
        .iter()
        .flat_map(|point| [point.sent, point.cap, point.target])
        .flatten()
        .max()
        .unwrap_or(0);
    let with_headroom = highest.saturating_add(highest / 10);
    let step = match with_headroom {
        0..=1000 => 100,
        1001..=5000 => 250,
        _ => 1000,
    };
    with_headroom.div_ceil(step).max(1).saturating_mul(step)
}

/// How many seconds the drawn "sent" line averages over.
///
/// An encoder meets its bitrate over a few seconds, not in each one: the
/// second holding a keyframe is several times the ones between. Drawn raw,
/// that is a comb that says nothing about the link and crosses the cap, which
/// is itself an average, every other second.
pub const SMOOTH: usize = 5;

/// Each value averaged with up to `window - 1` known values before it. A gap
/// stays a gap, and a value just after one averages only what it has.
pub fn smoothed(values: &[Option<u32>], window: usize) -> Vec<Option<u32>> {
    (0..values.len())
        .map(|index| {
            values[index]?;
            let from = index.saturating_sub(window.saturating_sub(1));
            let known: Vec<u64> = values[from..=index]
                .iter()
                .rev()
                .map_while(|value| value.map(u64::from))
                .collect();
            Some((known.iter().sum::<u64>() / known.len() as u64) as u32)
        })
        .collect()
}

/// A drawing surface `width` by `height`, with `top` kbps at its upper edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Canvas {
    pub width: f32,
    pub height: f32,
    pub top: u32,
}

impl Canvas {
    /// Where slot `index` of `count` sits, left to right.
    fn x(&self, index: usize, count: usize) -> f32 {
        match count {
            0 | 1 => self.width,
            _ => self.width * index as f32 / (count - 1) as f32,
        }
    }

    /// How far down `kbps` sits. Kept a pixel inside both edges, so a line at
    /// zero or at the top is still wholly on the surface.
    fn y(&self, kbps: u32) -> f32 {
        let share = match self.top {
            0 => 0.0,
            top => (kbps as f32 / top as f32).min(1.0),
        };
        (self.height * (1.0 - share)).clamp(1.0, self.height - 1.0)
    }

    /// `values`, one per slot, as runs of neighbouring points: a gap ends a
    /// run, so nothing is drawn across seconds that knew nothing.
    pub fn runs(&self, values: &[Option<u32>]) -> Vec<Vec<(f32, f32)>> {
        let mut runs = Vec::new();
        let mut run = Vec::new();
        for (index, value) in values.iter().enumerate() {
            match value {
                Some(kbps) => run.push((self.x(index, values.len()), self.y(*kbps))),
                None if run.is_empty() => {}
                None => runs.push(std::mem::take(&mut run)),
            }
        }
        if !run.is_empty() {
            runs.push(run);
        }
        runs
    }
}

/// How wide a lone second is drawn, so one sample is a dash and not nothing.
const LONE: f32 = 6.0;

/// One run as path commands, starting with a move. Straight, or rounded
/// through the midpoints: each point is a control point and the curve passes
/// through the middles of the segments, so it never swings past the data.
fn trace(run: &[(f32, f32)], smooth: bool) -> String {
    use std::fmt::Write;
    let mut d = String::new();
    match run {
        [] => {}
        [(x, y)] => {
            let _ = write!(d, "M{:.1},{y:.1} L{x:.1},{y:.1}", (x - LONE).max(0.0));
        }
        [(x, y), rest @ ..] => {
            let _ = write!(d, "M{x:.1},{y:.1}");
            if smooth {
                for pair in rest.windows(2) {
                    let ((cx, cy), (nx, ny)) = (pair[0], pair[1]);
                    let _ = write!(d, " Q{cx:.1},{cy:.1} {:.1},{:.1}", (cx + nx) / 2.0, (cy + ny) / 2.0);
                }
            } else {
                for (x, y) in &rest[..rest.len() - 1] {
                    let _ = write!(d, " L{x:.1},{y:.1}");
                }
            }
            let (x, y) = rest[rest.len() - 1];
            let _ = write!(d, " L{x:.1},{y:.1}");
        }
    }
    d
}

/// The runs as one path's `d`: a line, rounded or straight.
pub fn line_d(runs: &[Vec<(f32, f32)>], smooth: bool) -> String {
    runs.iter().map(|run| trace(run, smooth)).collect::<Vec<_>>().join(" ")
}

/// The runs as a filled shape's `d`: each rounded line closed down to the
/// bottom edge at `height`.
pub fn area_d(runs: &[Vec<(f32, f32)>], height: f32) -> String {
    runs.iter()
        .filter_map(|run| {
            let (first, last) = (run.first()?, run.last()?);
            let left = if run.len() == 1 { (first.0 - LONE).max(0.0) } else { first.0 };
            Some(format!("{} L{:.1},{height:.1} L{left:.1},{height:.1} Z", trace(run, true), last.0))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(kbps: u32) -> Second {
        Second {
            camera_sent: Some(kbps),
            camera_cap: Some(2500),
            ..Second::default()
        }
    }

    #[test]
    fn the_history_keeps_the_minute_and_its_run_up_oldest_first() {
        let mut history = History::default();
        for kbps in 0..(KEPT as u32 + 5) {
            history.push(sent(kbps));
        }
        let seconds = history.seconds();
        assert_eq!(seconds.len(), KEPT);
        assert_eq!(seconds[0].camera_sent, Some(5));
        assert_eq!(seconds[KEPT - 1].camera_sent, Some(KEPT as u32 + 4));
        history.clear();
        assert!(history.seconds().is_empty());
    }

    #[test]
    fn seconds_that_know_nothing_wait_for_one_that_does() {
        let mut history = History::default();
        history.push(Second::default());
        history.push(Second::default());
        assert!(history.seconds().is_empty(), "a connecting call is nothing to show");
        history.push(sent(100));
        // Once there is something, an empty second is a gap in it.
        history.push(Second::default());
        assert_eq!(history.seconds().len(), 2);
    }

    #[test]
    fn totals_add_what_is_known() {
        let mut second = Second::default();
        assert_eq!((second.total_sent(), second.total_cap()), (None, None));
        second.camera_sent = Some(1800);
        second.camera_cap = Some(2500);
        assert_eq!((second.total_sent(), second.total_cap()), (Some(1800), Some(2500)));
        second.share_sent = Some(0);
        second.share_cap = Some(6000);
        assert_eq!((second.total_sent(), second.total_cap()), (Some(1800), Some(8500)));
    }

    #[test]
    fn slots_put_the_newest_at_the_right() {
        let point = |kbps| Point {
            sent: Some(kbps),
            ..Point::default()
        };
        let slots = slots(&[point(1), point(2)]);
        assert_eq!(slots.len(), SECONDS);
        assert!(slots[..SECONDS - 2].iter().all(Option::is_none));
        assert_eq!(slots[SECONDS - 1], Some(point(2)));

        let many: Vec<Point> = (0..SECONDS as u32 + 3).map(point).collect();
        let full = super::slots(&many);
        assert_eq!(full.len(), SECONDS);
        assert_eq!(full[0], Some(point(3)));
    }

    #[test]
    fn the_top_is_the_cap_or_the_highest_sent_with_headroom() {
        let at = |sent, cap| Point {
            sent: Some(sent),
            cap: Some(cap),
            ..Point::default()
        };
        // Under the cap: the cap sets it, plus a tenth.
        assert_eq!(top(&[at(1800, 2500)]), 2750);
        // Over it: the overshoot does. 3200 + 320 rounds to 3750.
        assert_eq!(top(&[at(1800, 2500), at(3200, 2500)]), 3750);
        // Small numbers get a finer step. 400 + 40 rounds to 500.
        assert_eq!(top(&[at(0, 400)]), 500);
        // Large ones a coarser. 6000 + 600 rounds to 7000.
        assert_eq!(top(&[at(100, 6000)]), 7000);
    }

    #[test]
    fn the_estimate_never_sets_the_top() {
        let point = Point {
            sent: Some(900),
            cap: Some(1000),
            estimate: Some(50_000),
            ..Point::default()
        };
        assert_eq!(top(&[point]), 1250);
    }

    #[test]
    fn nothing_known_still_has_a_top() {
        assert_eq!(top(&[]), 100);
        assert_eq!(top(&[Point::default()]), 100);
    }

    #[test]
    fn smoothing_averages_what_came_before_and_respects_gaps() {
        let values = [Some(100), Some(300), None, Some(600), Some(0), Some(300)];
        assert_eq!(
            smoothed(&values, 3),
            vec![Some(100), Some(200), None, Some(600), Some(300), Some(300)]
        );
        assert_eq!(smoothed(&[], 5), Vec::<Option<u32>>::new());
        assert_eq!(smoothed(&[Some(7)], 0), vec![Some(7)], "a window of nothing is the value itself");
    }

    #[test]
    fn the_oldest_second_shown_is_a_full_average() {
        // A comb: 500, 100, 100, 100, 100, repeating. Its five-second average
        // is 180 everywhere once there are five seconds behind a point.
        let comb: Vec<Option<u32>> = (0..KEPT).map(|second| Some(if second % 5 == 0 { 500 } else { 100 })).collect();
        let smooth = smoothed(&comb, SMOOTH);
        let shown = &smooth[smooth.len() - SECONDS..];
        assert!(shown.iter().all(|value| *value == Some(180)), "{shown:?}");
    }

    const CANVAS: Canvas = Canvas {
        width: 600.0,
        height: 120.0,
        top: 3000,
    };

    #[test]
    fn points_scale_and_stay_inside_the_surface() {
        let runs = CANVAS.runs(&[Some(1500), Some(0), Some(3000), Some(50_000)]);
        assert_eq!(runs, vec![vec![(0.0, 60.0), (200.0, 119.0), (400.0, 1.0), (600.0, 1.0)]]);
        let none = Canvas { top: 0, ..CANVAS };
        assert_eq!(none.runs(&[Some(100)]), vec![vec![(600.0, 119.0)]], "no scale is the floor, not a division by zero");
    }

    #[test]
    fn a_gap_splits_the_line() {
        let runs = CANVAS.runs(&[None, Some(1500), Some(1500), None, None, Some(1500)]);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].len(), 2);
        assert_eq!(runs[1], vec![(600.0, 60.0)]);
    }

    #[test]
    fn paths_are_straight_or_rounded_and_a_lone_second_is_a_dash() {
        let run = vec![(0.0, 60.0), (300.0, 20.0), (600.0, 60.0)];
        assert_eq!(line_d(std::slice::from_ref(&run), false), "M0.0,60.0 L300.0,20.0 L600.0,60.0");
        assert_eq!(line_d(std::slice::from_ref(&run), true), "M0.0,60.0 Q300.0,20.0 450.0,40.0 L600.0,60.0");
        assert_eq!(line_d(&[vec![(600.0, 60.0)]], true), "M594.0,60.0 L600.0,60.0");
        assert_eq!(line_d(&[], true), "");
        assert_eq!(
            area_d(&[run], 120.0),
            "M0.0,60.0 Q300.0,20.0 450.0,40.0 L600.0,60.0 L600.0,120.0 L0.0,120.0 Z"
        );
    }
}
