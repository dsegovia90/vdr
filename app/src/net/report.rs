//! Our connection report: what goes in the `link` and `seen` fields of our
//! presence, gathered from [`Metrics`] and each peer's [`PeerStats`].
//!
//! **Ticked once a second by the attendance loop**, which already runs for
//! the length of a call and holds everyone's entry. The quality states are
//! judged every tick, so their hysteresis has a fine clock; the report itself
//! is stored on our own entry every [`REPORT_EVERY`], the rate the presence
//! document is re-said for its clock anyway, so it rides on a patch that goes
//! out regardless. Each peer's history takes a point at the same moments.
//!
//! **Gathered whether or not the switch is on.** The switch decides what is
//! said and shown (`Presence::here`, the tiles, the Network tab), so turning
//! it on mid-call publishes a report at once rather than after a wait.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use super::Metrics;
use super::metrics::{Path, PeerStats};
use super::quality::{self, Inputs, Judge, Link, LossWindow, REPORT_EVERY, Route, SEEN_MAX, Seen};

/// The state between ticks.
#[derive(Default)]
pub(super) struct Reporter {
    loss: LossWindow,
    link: Judge,
    /// One per peer we receive, keyed by endpoint id.
    cells: HashMap<String, Judge>,
    ticks: u64,
    /// What each stream sent at each tick since the last report, averaged
    /// into it: one second's figure is mostly where the keyframe fell.
    camera_sent: Vec<u32>,
    share_sent: Vec<u32>,
}

/// Ticks between reports: [`REPORT_EVERY`] in seconds.
const TICKS_PER_REPORT: u64 = REPORT_EVERY.as_secs();

impl Reporter {
    /// One second's tick. `present` is who has a live control broadcast;
    /// `people` everyone's entry by id.
    pub(super) fn tick(
        &mut self,
        now: Instant,
        metrics: &Metrics,
        local: &PeerStats,
        people: &HashMap<String, Arc<PeerStats>>,
        present: &HashSet<String>,
        me: &str,
    ) {
        for stats in people.values() {
            stats.roll();
        }
        self.loss.push(metrics.packets());
        let camera_on = local.camera_on();
        let camera_cap = metrics.camera_cap_kbps().filter(|_| camera_on);
        let share_cap = metrics.share_cap_kbps();
        if let Some(sent) = metrics.camera_sent_kbps().filter(|_| camera_on) {
            self.camera_sent.push(sent);
        }
        if let Some(sent) = metrics.share_sent_kbps() {
            self.share_sent.push(sent);
        }
        let loss = self.loss.recent();
        let raw = quality::classify(Inputs {
            rtt: metrics.rtt(),
            loss,
            estimate: metrics.estimate_kbps(),
            ask: camera_cap.unwrap_or(0).saturating_add(share_cap.unwrap_or(0)),
        });
        let q = self.link.observe(raw, now);

        // Who we report on: present, well-formed, not us, capped.
        let mut seen = BTreeMap::new();
        let mut ids: Vec<&String> = people
            .keys()
            .filter(|id| present.contains(*id) && quality::valid_id(id) && id.as_str() != me)
            .collect();
        ids.sort();
        ids.truncate(SEEN_MAX);
        for id in &ids {
            let stats = &people[*id];
            let late = stats.late_median();
            let raw = quality::cell(
                stats.video_fps(),
                stats.link().and_then(|link| link.fps),
                late,
                stats.camera_on(),
            );
            let q = self.cells.entry((*id).clone()).or_default().observe(raw, now);
            let fps = stats.video_fps().filter(|_| stats.camera_on());
            seen.insert(
                (*id).clone(),
                Seen {
                    q,
                    fps: fps.map(|fps| fps.round() as u32),
                    ms: late.filter(|_| stats.camera_on()),
                    delay: stats.delay_percentiles().0.filter(|_| stats.camera_on()),
                },
            );
        }
        self.cells.retain(|id, _| ids.contains(&id));

        self.ticks += 1;
        // The first tick reports, so a joiner is heard from at once.
        if !(self.ticks - 1).is_multiple_of(TICKS_PER_REPORT) {
            return;
        }
        let average = |sent: &mut Vec<u32>| {
            let taken = std::mem::take(sent);
            (!taken.is_empty()).then(|| (taken.iter().map(|v| u64::from(*v)).sum::<u64>() / taken.len() as u64) as u32)
        };
        let link = Link {
            q,
            est: metrics.estimate_kbps(),
            rtt: metrics.rtt(),
            loss,
            cam: metrics.camera_state().filter(|_| camera_on),
            share: metrics.share_state(),
            fps: metrics.camera_fps().filter(|_| camera_on),
            path: match metrics.path() {
                Path::Direct => Some(Route::Direct),
                Path::Relayed => Some(Route::Relayed),
                Path::Mixed => Some(Route::Mixed),
                Path::Unknown => None,
            },
            cam_sent: average(&mut self.camera_sent),
            cam_cap: camera_cap,
            share_sent: average(&mut self.share_sent),
            share_cap,
        }
        .clean();
        local.set_link(Some(link));
        local.set_seen(quality::clean_seen(seen, me));
        for stats in people.values() {
            stats.push_report();
        }
    }
}
