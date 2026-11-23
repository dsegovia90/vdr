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

/// How many seconds the history keeps.
pub const SECONDS: usize = 60;

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

/// The last [`SECONDS`] seconds, oldest first.
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
        if self.seconds.len() == SECONDS {
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
    /// The bar.
    pub sent: Option<u32>,
    /// The hard line, and part of the scale.
    pub cap: Option<u32>,
    pub target: Option<u32>,
    /// Drawn where it fits, clipped to the top where it does not; never part
    /// of the scale, or a generous link would flatten every bar.
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

/// How tall a bar of `kbps` is in a chart `px` high topped at `top`. Never
/// taller than the chart.
pub fn bar_px(kbps: u32, top: u32, px: u32) -> u32 {
    if top == 0 {
        return 0;
    }
    (kbps as u64 * px as u64 / top as u64).min(px as u64) as u32
}

/// Where a line `thickness` px thick at `kbps` sits, as its distance from the
/// bottom. A value above the top is clipped to it, line and all inside the
/// chart.
pub fn line_px(kbps: u32, top: u32, px: u32, thickness: u32) -> u32 {
    bar_px(kbps, top, px).min(px.saturating_sub(thickness))
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
    fn the_history_keeps_the_last_minute_oldest_first() {
        let mut history = History::default();
        for kbps in 0..(SECONDS as u32 + 5) {
            history.push(sent(kbps));
        }
        let seconds = history.seconds();
        assert_eq!(seconds.len(), SECONDS);
        assert_eq!(seconds[0].camera_sent, Some(5));
        assert_eq!(seconds[SECONDS - 1].camera_sent, Some(SECONDS as u32 + 4));
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
    fn heights_scale_and_stop_at_the_top() {
        assert_eq!(bar_px(1500, 3000, 120), 60);
        assert_eq!(bar_px(0, 3000, 120), 0);
        assert_eq!(bar_px(3000, 3000, 120), 120);
        assert_eq!(bar_px(9000, 3000, 120), 120);
        assert_eq!(bar_px(100, 0, 120), 0, "no scale is no height, not a division by zero");
    }

    #[test]
    fn a_line_above_the_top_stays_inside_the_chart() {
        assert_eq!(line_px(1500, 3000, 120, 2), 60);
        assert_eq!(line_px(3000, 3000, 120, 2), 118);
        assert_eq!(line_px(50_000, 3000, 120, 2), 118);
        assert_eq!(line_px(0, 3000, 120, 2), 0);
    }
}
