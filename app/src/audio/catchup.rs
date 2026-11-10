//! Catch-up: plays a peer's voice slightly fast while their playout buffer runs
//! long, so their lag behind live stays bounded.
//!
//! **Why this exists.** The playback sink resamples at a fixed ratio and never
//! corrects drift. Whatever lands in it stays and plays late: a microphone whose
//! clock runs fast against our speaker adds a few ms a minute, and a network
//! stall followed by a burst adds the whole burst at once. Nothing takes either
//! back out until the sink's own overflow at about 2.3 s, so a voice drifts
//! further and further behind a face that never waits.
//!
//! **What it does**, the way WebRTC's NetEQ "accelerate" does it: when the
//! buffer stays long, a voiced block loses one pitch period, cross-faded so the
//! waveform stays continuous and the pitch unchanged, and a silent block is
//! dropped whole. Each decoded block goes in and a block of the same length or
//! shorter comes out -- nothing is held back, so it adds no latency, which is
//! the very thing it is removing. At 1.0x it does nothing at all, and the block
//! passes through bit for bit.
//!
//! **The rate cap holds between any two cuts, not just on average.** A cut is
//! paid for in advance: time owed accrues at the playback rate, a period is cut
//! only once a whole period is owed, the cut settles the whole debt, and no
//! more than one longest period can be banked. So however often the pitch search fails, the audio between two
//! cuts never plays faster than 1.10x, and a voice that comes back after a
//! stretch of failed searches is not cut several blocks running. Silence is
//! the exception, and meant to be: a pause dropped whole is not heard as speed.
//!
//! **Steered by the floor**, the lowest buffer depth of the last second, read
//! just before each write. The floor is the slack the worst-timed packet of the
//! last second still had, so steering by it keeps the jitter cushion: on a
//! jittery link the mean rises above the floor, and catch-up never eats that.
//!
//! **Time is counted in decoded audio**, never by the clock on the wall, so a
//! test can run twenty simulated minutes in a moment and get the same answer
//! every time.
//!
//! Pure: no I/O, no clock, no allocation per block once its buffers are warm.

use std::f32::consts::PI;
use std::time::Duration;

/// The floor above which catch-up engages: about 70 ms more than the sink needs,
/// which is over three Opus frames of lasting excess.
pub(crate) const START_MS: f32 = 120.0;
/// The floor below which it lets go. The band between this and [`START_MS`] is
/// hysteresis, so slow drift triggers a short catch-up every few minutes rather
/// than a flutter.
pub(crate) const STOP_MS: f32 = 70.0;
/// A floor this far behind is not worth catching up on audibly: jump to live.
pub(crate) const SKIP_MS: f32 = 1000.0;
/// At most one jump to live per this much audio, so a backlog still arriving is
/// drained rather than skipped twice.
const SKIP_EVERY_MS: f64 = 5000.0;

/// Playback rate just after engaging.
const RATE_MIN: f32 = 1.02;
/// The cap. Voiced cuts are whole pitch periods, so locally the speed-up comes
/// in bursts above the average; 1.10 keeps those bursts unremarkable.
const RATE_MAX: f32 = 1.10;
/// Excess over [`STOP_MS`] at which the rate starts rising from [`RATE_MIN`],
/// and the span over which it reaches [`RATE_MAX`].
const RAMP_FROM_MS: f32 = 50.0;
const RAMP_SPAN_MS: f32 = 450.0;

/// The most time that may be owed: one longest period. Any more, and a
/// stretch where the pitch search keeps failing (common in real speech) would
/// bank a run of cuts, taken block after block once it succeeds -- a 100 Hz
/// voice losing half of each of four blocks, played locally at 2x.
const OWED_MAX_MS: f32 = MAX_PERIOD_MS;

/// A block quieter than this is silence, and can be dropped whole: about
/// -46 dBFS, a quarter of the level at which a tile shows someone talking.
pub(crate) const SILENT: f32 = 0.005;

/// How long a pitch period may be. 100-400 Hz covers nearly every voice; the
/// upper bound is what lets a cut (two periods long) fit in one 20 ms block.
const MIN_PERIOD_MS: f32 = 2.5;
const MAX_PERIOD_MS: f32 = 10.0;
/// The coarse pitch search runs at a quarter of the rate (12 kHz from 48).
const DECIMATE: usize = 4;
/// How alike two periods must be before one is cut: NetEQ's threshold. Below
/// it the block is not periodic enough for a cut to go unheard.
const VOICED: f32 = 0.9;
/// How far below the best coarse score an earlier peak may be and still win.
/// Every multiple of the period scores alike; the shortest is the one wanted.
const PEAK_SLACK: f32 = 0.05;

/// The floor is the minimum over two buckets of this much audio each.
const FLOOR_BUCKET_MS: f32 = 500.0;
/// Cross-fade over which a dropped block's start runs into the next block.
const SPLICE_MS: f32 = 2.5;

/// What to do with one block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Write it unchanged.
    Pass,
    /// Cut this many frames (one pitch period) out of it.
    Cut(usize),
    /// Write nothing: it is silence, and the buffer can spare it.
    Drop,
    /// The buffer is so far behind that the caller should start a fresh one.
    SkipToLive,
}

/// What [`Catchup::process`] did to a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Write the block. `changed` is false when it is exactly what came in, so
    /// the caller can write the bytes it already has.
    Play { changed: bool },
    /// Write nothing.
    Drop,
    /// Start a fresh sink, write the block as it came in, and call
    /// [`Catchup::skipped`].
    SkipToLive,
}

/// How much catch-up has taken out, over the whole call.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Removed {
    /// By cutting pitch periods out of speech.
    pub voiced_ms: f64,
    /// By dropping silent blocks.
    pub silent_ms: f64,
}

/// Root mean square of interleaved samples, the same measure as the talking
/// meter's `loudness` but on samples rather than bytes.
pub(crate) fn level(block: &[f32]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    let sum: f64 = block.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / block.len() as f64).sqrt() as f32
}

/// Whether a block counts as silence. A sender running discontinuous
/// transmission marks its silence as such; ours does not, so the level decides.
pub(crate) fn is_silent(rms: f32, activity: moq_audio::Activity) -> bool {
    activity.is_dtx() || rms < SILENT
}

fn ms_to_frames(ms: f32, rate: u32) -> usize {
    (ms * rate as f32 / 1000.0).round() as usize
}

fn frames_to_ms(frames: usize, rate: u32) -> f32 {
    frames as f32 * 1000.0 / rate as f32
}

/// A raised-cosine weight, 0 at `i = 0` rising towards 1 at `i = n`.
fn ramp(i: usize, n: usize) -> f32 {
    0.5 - 0.5 * (PI * i as f32 / n as f32).cos()
}

/// Normalized correlation of `x[0..lag]` against `x[lag..2 lag]`.
fn similarity(x: &[f32], lag: usize) -> f32 {
    let (a, b) = (&x[..lag], &x[lag..2 * lag]);
    let (mut ab, mut aa, mut bb) = (0.0f32, 0.0f32, 0.0f32);
    for (&a, &b) in a.iter().zip(b) {
        ab += a * b;
        aa += a * a;
        bb += b * b;
    }
    let norm = (aa * bb).sqrt();
    if norm <= 1e-12 { 0.0 } else { ab / norm }
}

/// The pitch search, with buffers kept between calls so a search allocates
/// nothing once warm.
#[derive(Default)]
pub(crate) struct Pitch {
    mono: Vec<f32>,
    coarse: Vec<f32>,
    scores: Vec<f32>,
}

impl Pitch {
    /// The block's pitch period in frames, or `None` when it is not voiced
    /// clearly enough to cut.
    ///
    /// Runs on the channel average, coarse at a quarter rate over 2.5-10 ms,
    /// then refined at the full rate around the winner. Of the coarse peaks,
    /// the shortest within [`PEAK_SLACK`] of the best is taken: every multiple
    /// of a period matches about as well as the period itself, and a shorter
    /// cut is the gentler one. A tone above 400 Hz therefore comes back as a
    /// multiple of its period, which cuts just as cleanly.
    pub(crate) fn period(&mut self, block: &[f32], channels: usize, rate: u32) -> Option<usize> {
        let channels = channels.max(1);
        let frames = block.len() / channels;
        let shortest = ms_to_frames(MIN_PERIOD_MS, rate);
        let longest = ms_to_frames(MAX_PERIOD_MS, rate);
        if frames < 2 * longest || level(block) < SILENT {
            return None;
        }
        self.mono.clear();
        self.mono.extend(
            block.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        self.coarse.clear();
        self.coarse.extend(
            self.mono.as_chunks::<DECIMATE>().0.iter().map(|run| run.iter().sum::<f32>() / DECIMATE as f32),
        );

        let low = (shortest / DECIMATE).max(1);
        let high = (longest / DECIMATE).min(self.coarse.len() / 2);
        if high <= low {
            return None;
        }
        self.scores.clear();
        self.scores.extend((low..=high).map(|lag| similarity(&self.coarse, lag)));
        let best = self.scores.iter().copied().fold(f32::MIN, f32::max);
        if best < VOICED - PEAK_SLACK {
            return None;
        }
        let last = self.scores.len() - 1;
        let peak = (0..=last).find(|&i| {
            let score = self.scores[i];
            score >= best - PEAK_SLACK
                && (i == 0 || score >= self.scores[i - 1])
                && (i == last || score >= self.scores[i + 1])
        })?;

        let centre = (low + peak) * DECIMATE;
        let from = centre.saturating_sub(DECIMATE).max(1);
        let from = from.max(shortest);
        let to = (centre + DECIMATE).min(frames / 2).min(longest);
        let (lag, score) = (from..=to)
            .map(|lag| (lag, similarity(&self.mono, lag)))
            .fold((0, f32::MIN), |best, next| if next.1 > best.1 { next } else { best });
        (score >= VOICED).then_some(lag)
    }
}

/// Cuts one pitch period out of a block: `x[0..2p]` becomes a `p`-long
/// raised-cosine cross-fade from `x[0..p]` into `x[p..2p]`.
///
/// The first sample is still `x[0]`, so the block meets the previous one where
/// it always did, and the last of the fade runs into `x[2p]`. That is why no
/// look-back or look-ahead is needed. Every channel is cut at the same frame,
/// so they stay aligned. A block too short for the cut is left alone.
pub(crate) fn cut_period(block: &mut Vec<f32>, channels: usize, p: usize) {
    let channels = channels.max(1);
    if p == 0 || 2 * p > block.len() / channels {
        return;
    }
    for i in 0..p {
        let w = ramp(i, p);
        for c in 0..channels {
            let a = block[i * channels + c];
            let b = block[(i + p) * channels + c];
            block[i * channels + c] = a + w * (b - a);
        }
    }
    block.drain(p * channels..2 * p * channels);
}

/// The seam a dropped silent block leaves behind.
///
/// The start of the first dropped block is kept, and the next block written is
/// cross-faded from it into its own start. The fade's first sample is the
/// dropped block's first, which follows on from what was played, and its end is
/// the next block's own, so both edges of the seam are continuous. A run of
/// drops keeps the first one's head: that is where the played audio left off.
#[derive(Default)]
pub(crate) struct Splice {
    head: Vec<f32>,
    pending: bool,
}

impl Splice {
    /// Notes a dropped block.
    pub(crate) fn keep(&mut self, dropped: &[f32], channels: usize, rate: u32) {
        if self.pending {
            return;
        }
        let channels = channels.max(1);
        let frames = ms_to_frames(SPLICE_MS, rate).min(dropped.len() / channels);
        self.head.clear();
        self.head.extend_from_slice(&dropped[..frames * channels]);
        self.pending = true;
    }

    /// Fades the kept head into the start of `block`. Returns whether there was
    /// one; it stays kept until [`Self::clear`], in case `block` is dropped too.
    pub(crate) fn blend(&self, block: &mut [f32], channels: usize) -> bool {
        if !self.pending {
            return false;
        }
        let channels = channels.max(1);
        let frames = self.head.len().min(block.len()) / channels;
        for i in 0..frames {
            let w = ramp(i, frames);
            for c in 0..channels {
                let at = i * channels + c;
                block[at] = self.head[at] + w * (block[at] - self.head[at]);
            }
        }
        true
    }

    pub(crate) fn clear(&mut self) {
        self.pending = false;
    }
}

/// The lowest depth over the last 0.5-1 s of audio: two buckets of
/// [`FLOOR_BUCKET_MS`]. It follows the depth down at once and up only once the
/// low reading has aged out.
#[derive(Default)]
struct Floor {
    current: Option<f32>,
    previous: Option<f32>,
    filled_ms: f32,
}

impl Floor {
    fn push(&mut self, depth_ms: f32, block_ms: f32) {
        if self.filled_ms >= FLOOR_BUCKET_MS {
            self.previous = self.current.take();
            self.filled_ms = 0.0;
        }
        self.current = Some(self.current.map_or(depth_ms, |low| low.min(depth_ms)));
        self.filled_ms += block_ms;
    }

    fn get(&self) -> Option<f32> {
        match (self.current, self.previous) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// The decision half: when to engage, how fast, what to take.
struct Controller {
    enabled: bool,
    rate: u32,
    engaged: bool,
    floor: Floor,
    owed_ms: f32,
    /// Whether the last block was silent, for the drop's hangover.
    was_silent: bool,
    removed: Removed,
    skips: u32,
    /// Audio decided on so far, in ms: the controller's only clock.
    clock_ms: f64,
    last_skip_ms: Option<f64>,
}

impl Controller {
    fn playback_rate(&self) -> f32 {
        if !self.engaged {
            return 1.0;
        }
        let excess = self.floor.get().unwrap_or(0.0) - STOP_MS;
        let ramp = ((excess - RAMP_FROM_MS) / RAMP_SPAN_MS).clamp(0.0, 1.0);
        (RATE_MIN + (RATE_MAX - RATE_MIN) * ramp).min(RATE_MAX)
    }

    fn decide(
        &mut self,
        depth: Duration,
        block_ms: f32,
        silent: bool,
        period: impl FnOnce() -> Option<usize>,
    ) -> Action {
        self.floor.push(depth.as_secs_f32() * 1000.0, block_ms);
        self.clock_ms += block_ms as f64;
        // One quiet block alone is often a soft consonant's tail or a breath,
        // not a pause: only the second in a row is dropped.
        let after_silence = std::mem::replace(&mut self.was_silent, silent);
        if !self.enabled {
            return Action::Pass;
        }
        let floor = self.floor.get().unwrap_or(0.0);

        let rested = self.last_skip_ms.is_none_or(|at| self.clock_ms - at >= SKIP_EVERY_MS);
        if floor >= SKIP_MS && rested {
            return Action::SkipToLive;
        }

        if self.engaged && floor < STOP_MS {
            self.engaged = false;
            self.owed_ms = 0.0;
        } else if !self.engaged && floor > START_MS {
            self.engaged = true;
        }
        if !self.engaged {
            return Action::Pass;
        }

        if silent {
            // A drop takes the whole block at once, so it is held back when it
            // would leave less than the target in hand.
            if after_silence && floor - block_ms >= STOP_MS {
                self.removed.silent_ms += block_ms as f64;
                return Action::Drop;
            }
            return Action::Pass;
        }

        let rate = self.playback_rate();
        self.owed_ms = (self.owed_ms + block_ms * (rate - 1.0) / rate).min(OWED_MAX_MS);
        // Not worth a pitch search until even the shortest period is owed.
        if self.owed_ms < MIN_PERIOD_MS {
            return Action::Pass;
        }
        match period() {
            Some(p) => {
                let p_ms = frames_to_ms(p, self.rate);
                if self.owed_ms >= p_ms {
                    // Any remainder is forgiven rather than carried: carried,
                    // it would pay for the next cut a block later, and that
                    // span would play faster than the cap.
                    self.owed_ms = 0.0;
                    self.removed.voiced_ms += p_ms as f64;
                    Action::Cut(p)
                } else {
                    Action::Pass
                }
            }
            None => Action::Pass,
        }
    }

    fn skipped(&mut self) {
        self.floor = Floor::default();
        self.engaged = false;
        self.owed_ms = 0.0;
        self.skips += 1;
        self.last_skip_ms = Some(self.clock_ms);
    }

    fn skip_failed(&mut self) {
        self.last_skip_ms = Some(self.clock_ms);
    }
}

/// Catch-up for one peer's stream.
pub(crate) struct Catchup {
    controller: Controller,
    pitch: Pitch,
    splice: Splice,
    channels: usize,
}

impl Catchup {
    /// For a stream at `rate` Hz with `channels` interleaved. Disabled, it
    /// still measures the floor but never touches the audio.
    pub(crate) fn new(rate: u32, channels: usize, enabled: bool) -> Self {
        Self {
            controller: Controller {
                enabled,
                rate: rate.max(1),
                engaged: false,
                floor: Floor::default(),
                owed_ms: 0.0,
                was_silent: false,
                removed: Removed::default(),
                skips: 0,
                clock_ms: 0.0,
                last_skip_ms: None,
            },
            pitch: Pitch::default(),
            splice: Splice::default(),
            channels: channels.max(1),
        }
    }

    /// Decides what to do with the next block. `depth` is the sink's buffered
    /// audio just before the write; `period` is only called when a cut is due.
    /// The call goes through [`Self::process`]; this is for driving the
    /// controller alone.
    #[cfg(test)]
    pub(crate) fn decide(
        &mut self,
        depth: Duration,
        block_ms: f32,
        silent: bool,
        period: impl FnOnce() -> Option<usize>,
    ) -> Action {
        self.controller.decide(depth, block_ms, silent, period)
    }

    /// Runs one decoded block through: decides, then cuts, splices or drops it
    /// in place. The path the call takes and the tests take alike.
    pub(crate) fn process(&mut self, block: &mut Vec<f32>, depth: Duration, silent: bool) -> Outcome {
        let channels = self.channels;
        let rate = self.controller.rate;
        let block_ms = frames_to_ms(block.len() / channels, rate);
        // The seam goes in first, so the pitch search sees what will be played.
        let spliced = self.splice.blend(block, channels);
        let pitch = &mut self.pitch;
        let action = self.controller.decide(depth, block_ms, silent, || {
            pitch.period(block, channels, rate)
        });
        match action {
            Action::Pass => {
                self.splice.clear();
                Outcome::Play { changed: spliced }
            }
            Action::Cut(p) => {
                self.splice.clear();
                cut_period(block, channels, p);
                Outcome::Play { changed: true }
            }
            Action::Drop => {
                // Untouched unless a splice was already pending, and then the
                // earlier head is the one kept.
                self.splice.keep(block, channels, rate);
                Outcome::Drop
            }
            Action::SkipToLive => {
                self.splice.clear();
                Outcome::SkipToLive
            }
        }
    }

    /// The caller started a fresh sink: forget the old one's depth.
    pub(crate) fn skipped(&mut self) {
        self.controller.skipped();
    }

    /// The caller could not open a fresh sink: carry on with the old one, and
    /// try again no sooner than another skip would be allowed.
    pub(crate) fn skip_failed(&mut self) {
        self.controller.skip_failed();
    }

    /// The playback rate being aimed for: 1.0 unless engaged.
    pub(crate) fn rate(&self) -> f32 {
        self.controller.playback_rate()
    }

    /// The lowest depth over the last second, as of the last block.
    pub(crate) fn floor(&self) -> Duration {
        Duration::from_secs_f32(self.controller.floor.get().unwrap_or(0.0).max(0.0) / 1000.0)
    }

    pub(crate) fn removed(&self) -> Removed {
        self.controller.removed
    }

    pub(crate) fn skips(&self) -> u32 {
        self.controller.skips
    }

    #[cfg(test)]
    pub(crate) fn engaged(&self) -> bool {
        self.controller.engaged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moq_audio::Activity;

    const RATE: u32 = 48_000;
    const BLOCK: usize = 960;

    /// xorshift64: deterministic noise with no dependency.
    struct Rng(u64);

    impl Rng {
        /// Uniform in [-1, 1).
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        }
    }

    /// A two-pole resonator: one formant.
    struct Resonator {
        a1: f32,
        a2: f32,
        y1: f32,
        y2: f32,
    }

    impl Resonator {
        fn new(freq: f32, bandwidth: f32) -> Self {
            let r = (-PI * bandwidth / RATE as f32).exp();
            let theta = 2.0 * PI * freq / RATE as f32;
            Self { a1: 2.0 * r * theta.cos(), a2: -r * r, y1: 0.0, y2: 0.0 }
        }

        fn run(&mut self, x: f32) -> f32 {
            let y = x + self.a1 * self.y1 + self.a2 * self.y2;
            self.y2 = self.y1;
            self.y1 = y;
            y
        }
    }

    /// Something speech-shaped: a glottal pulse train gliding 110-220 Hz
    /// through two formants, in 4 Hz syllables, with a 150 ms pause of quiet
    /// room noise every 1.2 s.
    struct Speech {
        n: u64,
        phase: f64,
        f1: Resonator,
        f2: Resonator,
        rng: Rng,
    }

    impl Speech {
        fn new() -> Self {
            Self {
                n: 0,
                phase: 0.0,
                f1: Resonator::new(700.0, 150.0),
                f2: Resonator::new(1200.0, 150.0),
                rng: Rng(0x9e37_79b9_7f4a_7c15),
            }
        }

        fn next(&mut self) -> f32 {
            let t = self.n as f64 / RATE as f64;
            self.n += 1;
            let cycle = (t / 2.0) % 2.0;
            let f0 = if cycle < 1.0 { 110.0 + 110.0 * cycle } else { 220.0 - 110.0 * (cycle - 1.0) };
            self.phase += f0 / RATE as f64;
            let pulse = if self.phase >= 1.0 {
                self.phase -= 1.0;
                1.0
            } else {
                0.0
            };
            let voiced = 0.03 * self.f1.run(pulse) + 0.015 * self.f2.run(pulse);
            if t % 1.2 >= 1.05 {
                // -60 dBFS RMS: a noise-suppressed room.
                return 0.001 * 1.732 * self.rng.next();
            }
            let syllable = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * 4.0 * t).cos();
            voiced * (0.15 + 0.85 * syllable) as f32
        }
    }

    fn tone(freq: f64, amplitude: f64) -> impl FnMut() -> f32 {
        let mut n = 0u64;
        move || {
            let t = n as f64 / RATE as f64;
            n += 1;
            (amplitude * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32
        }
    }

    fn speech() -> impl FnMut() -> f32 {
        let mut speech = Speech::new();
        move || speech.next()
    }

    /// Fills a block of `frames`; stereo carries `-L/2` on the right. Not
    /// `-L`: the analysis runs on the channel average, which that would cancel.
    fn block(source: &mut impl FnMut() -> f32, frames: usize, channels: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * channels);
        for _ in 0..frames {
            let s = source();
            out.push(s);
            if channels == 2 {
                out.push(-0.5 * s);
            }
        }
        out
    }

    fn max_step(samples: &[f32], channels: usize) -> f32 {
        samples
            .chunks_exact(channels)
            .zip(samples.chunks_exact(channels).skip(1))
            .map(|(a, b)| (b[0] - a[0]).abs())
            .fold(0.0, f32::max)
    }

    fn depth(ms: f32) -> Duration {
        Duration::from_secs_f32(ms / 1000.0)
    }

    /// A model of the sink as a sample counter. The device plays continuously
    /// on its own clock; the sender delivers 20 ms blocks at `1 + skew` of
    /// that rate, held back during a stall and then delivered at once. A push
    /// into a nearly empty buffer is padded to 50 ms, as the real sink does.
    struct Sim<S: FnMut() -> f32> {
        catchup: Catchup,
        source: S,
        channels: usize,
        skew: f64,
        stall: Option<(f64, f64)>,
        /// Frames buffered after the last write, and the device time of it.
        level: f64,
        t: f64,
        k: u64,
        keep: bool,
        out: Vec<f32>,
        last_in: Option<f32>,
        last_out: Option<f32>,
        max_step_in: f32,
        max_step_out: f32,
        skips: u32,
        floors: Vec<(f64, f32)>,
        engaged_ms: f64,
        rate_ms: f64,
        played_in: usize,
        played_out: usize,
        engaged_count: u32,
        released_count: u32,
        /// Frames played since the last cut, and the fastest any span
        /// between two cuts played.
        since_cut: usize,
        fastest_span: f64,
    }

    impl<S: FnMut() -> f32> Sim<S> {
        fn new(source: S, channels: usize, skew: f64) -> Self {
            Self {
                catchup: Catchup::new(RATE, channels, true),
                source,
                channels,
                skew,
                stall: None,
                level: 0.0,
                t: 0.0,
                k: 0,
                keep: false,
                out: Vec::new(),
                last_in: None,
                last_out: None,
                max_step_in: 0.0,
                max_step_out: 0.0,
                skips: 0,
                floors: Vec::new(),
                engaged_ms: 0.0,
                rate_ms: 0.0,
                played_in: 0,
                played_out: 0,
                engaged_count: 0,
                released_count: 0,
                since_cut: 0,
                fastest_span: 1.0,
            }
        }

        fn stall(mut self, at_s: f64, for_ms: f64) -> Self {
            self.stall = Some((at_s * RATE as f64, for_ms * RATE as f64 / 1000.0));
            self
        }

        fn seconds(&self) -> f64 {
            self.t / RATE as f64
        }

        fn write(&mut self, samples: &[f32]) {
            if self.level < 0.0125 * RATE as f64 {
                self.level = 0.05 * RATE as f64;
            }
            self.level += (samples.len() / self.channels) as f64;
            for frame in samples.chunks_exact(self.channels) {
                if let Some(last) = self.last_out {
                    self.max_step_out = self.max_step_out.max((frame[0] - last).abs());
                }
                self.last_out = Some(frame[0]);
            }
            if self.keep {
                self.out.extend_from_slice(samples);
            }
        }

        fn step(&mut self) {
            let nominal = self.k as f64 * BLOCK as f64 / (1.0 + self.skew);
            let arrival = match self.stall {
                Some((from, length)) if nominal >= from && nominal < from + length => from + length,
                _ => nominal,
            };
            self.level = (self.level - (arrival - self.t)).max(0.0);
            self.t = arrival;
            self.k += 1;

            let mut samples = block(&mut self.source, BLOCK, self.channels);
            for frame in samples.chunks_exact(self.channels) {
                if let Some(last) = self.last_in {
                    self.max_step_in = self.max_step_in.max((frame[0] - last).abs());
                }
                self.last_in = Some(frame[0]);
            }
            let silent = is_silent(level(&samples), Activity::Active);
            let was = self.catchup.engaged();
            let outcome = self.catchup.process(&mut samples, depth_of(self.level), silent);
            match (was, self.catchup.engaged()) {
                (false, true) => self.engaged_count += 1,
                (true, false) => self.released_count += 1,
                _ => {}
            }
            if self.catchup.engaged() {
                self.engaged_ms += 20.0;
                self.rate_ms += 20.0 * self.catchup.rate() as f64;
            }
            match outcome {
                Outcome::Play { .. } => {
                    let played = samples.len() / self.channels;
                    self.since_cut += BLOCK;
                    if played < BLOCK {
                        let span = self.since_cut as f64;
                        self.fastest_span = self.fastest_span.max(span / (span - (BLOCK - played) as f64));
                        self.since_cut = 0;
                    }
                    if self.catchup.engaged() {
                        self.played_in += BLOCK;
                        self.played_out += samples.len() / self.channels;
                    }
                    self.write(&samples);
                }
                Outcome::Drop => {}
                Outcome::SkipToLive => {
                    self.skips += 1;
                    self.catchup.skipped();
                    self.level = 0.0;
                    // A fresh sink: the mixer fades it in, so no step to judge.
                    self.last_out = None;
                    self.write(&samples);
                }
            }
            self.floors.push((self.seconds(), self.catchup.floor().as_secs_f32() * 1000.0));
        }

        fn run_until(&mut self, seconds: f64) {
            while self.seconds() < seconds {
                self.step();
            }
        }

        /// The highest floor seen at or after `from_s`.
        fn max_floor_after(&self, from_s: f64) -> f32 {
            self.floors.iter().filter(|(t, _)| *t >= from_s).map(|(_, f)| *f).fold(0.0, f32::max)
        }

        /// When the floor first fell under `ms` at or after `from_s`.
        fn under_after(&self, from_s: f64, ms: f32) -> Option<f64> {
            self.floors.iter().find(|(t, f)| *t >= from_s && *f < ms).map(|(t, _)| *t)
        }

        fn removed_ms(&self) -> f64 {
            let removed = self.catchup.removed();
            removed.voiced_ms + removed.silent_ms
        }
    }

    fn depth_of(frames: f64) -> Duration {
        Duration::from_secs_f64(frames / RATE as f64)
    }

    #[test]
    fn finds_voice_periods_and_not_noise() {
        let mut pitch = Pitch::default();
        for freq in [100.0, 150.0, 200.0, 400.0] {
            let samples = block(&mut tone(freq, 0.5), BLOCK, 1);
            let expected = RATE as f64 / freq;
            let found = pitch.period(&samples, 1, RATE).expect("a tone is voiced");
            assert!((found as f64 - expected).abs() <= 1.0, "{freq} Hz: {found}, wanted {expected}");
        }
        // 440 Hz is below the search range; a multiple of it cuts as cleanly.
        let samples = block(&mut tone(440.0, 0.5), BLOCK, 1);
        let found = pitch.period(&samples, 1, RATE).expect("a tone is voiced");
        assert!((found as f64 - 2.0 * RATE as f64 / 440.0).abs() <= 1.0, "440 Hz: {found}");

        let mut rng = Rng(7);
        let noise = block(&mut || 0.3 * rng.next(), BLOCK, 1);
        assert_eq!(pitch.period(&noise, 1, RATE), None, "white noise");
        assert_eq!(pitch.period(&vec![0.0; BLOCK], 1, RATE), None, "silence");
    }

    #[test]
    fn a_cut_is_continuous_and_keeps_the_pitch() {
        let mut source = tone(440.0, 0.5);
        let mut pitch = Pitch::default();
        let mut out = Vec::new();
        for _ in 0..100 {
            let mut samples = block(&mut source, BLOCK, 1);
            let p = pitch.period(&samples, 1, RATE).unwrap();
            cut_period(&mut samples, 1, p);
            assert_eq!(samples.len(), BLOCK - p);
            out.extend_from_slice(&samples);
        }
        let natural = 0.5 * 2.0 * PI * 440.0 / RATE as f32;
        let step = max_step(&out, 1);
        assert!(step <= 1.4 * natural, "step {step} against {natural}");

        // Every block lost a fifth of itself; a resample would read 550 Hz.
        let rising = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        let freq = rising as f64 / (out.len() as f64 / RATE as f64);
        assert!((freq - 440.0).abs() <= 4.4, "pitch moved to {freq:.1} Hz");
    }

    #[test]
    fn a_silent_drop_leaves_no_step() {
        let channels = 1;
        // Between two blocks of quiet noise.
        let mut rng = Rng(11);
        let mut noise = || 0.004 * rng.next();
        let blocks: Vec<Vec<f32>> = (0..3).map(|_| block(&mut noise, BLOCK, channels)).collect();
        let natural = max_step(&blocks.concat(), channels);
        let mut splice = Splice::default();
        splice.keep(&blocks[1], channels, RATE);
        let mut next = blocks[2].clone();
        assert!(splice.blend(&mut next, channels));
        let out = [blocks[0].clone(), next].concat();
        assert!(max_step(&out, channels) <= natural, "noise through a drop");

        // A quiet tone, whose phase jumps across the drop.
        let mut source = tone(440.0, 0.004);
        let blocks: Vec<Vec<f32>> = (0..3).map(|_| block(&mut source, BLOCK, channels)).collect();
        let natural = 0.004 * 2.0 * PI * 440.0 / RATE as f32;
        let mut splice = Splice::default();
        splice.keep(&blocks[1], channels, RATE);
        let mut next = blocks[2].clone();
        splice.blend(&mut next, channels);
        let out = [blocks[0].clone(), next].concat();
        let step = max_step(&out, channels);
        assert!(step <= 1.4 * natural, "tone through a drop: {step} against {natural}");
    }

    #[test]
    fn drift_of_200_ppm_stays_bounded_for_20_minutes() {
        let mut sim = Sim::new(speech(), 1, 200e-6);
        sim.run_until(20.0 * 60.0);
        let peak = sim.max_floor_after(0.0);
        assert!(peak <= START_MS + 20.0, "floor reached {peak} ms");
        assert!(
            sim.max_step_out <= 1.5 * sim.max_step_in,
            "step {} against {}",
            sim.max_step_out,
            sim.max_step_in
        );
        assert_eq!(sim.skips, 0);
        // 200 ppm of 20 minutes is 240 ms. What was not removed is still in
        // the buffer, between the target and the threshold.
        let removed = sim.removed_ms();
        assert!(sim.fastest_span <= RATE_MAX as f64 + 1e-3);
        let left = sim.level * 1000.0 / RATE as f64 - 50.0;
        assert!((removed + left - 240.0).abs() <= 24.0, "removed {removed:.0} + left {left:.0} ms");
        assert!((removed - 240.0).abs() <= 0.3 * 240.0, "removed {removed:.0} ms");
    }

    #[test]
    fn drift_of_2_percent_stays_bounded() {
        let mut speech = Sim::new(speech(), 1, 0.02);
        speech.run_until(60.0);
        let peak = speech.max_floor_after(0.0);
        assert!(peak <= START_MS + 60.0, "speech: floor reached {peak} ms");
        assert!(speech.rate_ms / speech.engaged_ms <= 1.10 + 1e-6);

        let mut tone = Sim::new(tone(440.0, 0.5), 1, 0.02);
        tone.run_until(60.0);
        let peak = tone.max_floor_after(0.0);
        assert!(peak <= START_MS + 60.0, "tone: floor reached {peak} ms");
        assert!(tone.rate_ms / tone.engaged_ms <= 1.10 + 1e-6);
        // What actually played, not just what was aimed for.
        let played = tone.played_in as f64 / tone.played_out as f64;
        assert!(played <= 1.11, "tone played at {played:.3}x");
        assert_eq!(speech.skips + tone.skips, 0);
        for sim in [&speech.fastest_span, &tone.fastest_span] {
            assert!(*sim <= RATE_MAX as f64 + 1e-3, "a span between cuts played at {sim:.3}x");
        }
    }

    #[test]
    fn an_800_ms_burst_drains() {
        let mut sim = Sim::new(speech(), 1, 0.0).stall(10.0, 800.0);
        sim.run_until(30.0);
        let peak = sim.max_floor_after(10.0);
        assert!(peak > 700.0, "the burst never landed: {peak} ms");
        let back = sim.under_after(11.5, STOP_MS).expect("never drained");
        assert!(sim.fastest_span <= RATE_MAX as f64 + 1e-3, "speech: a span played at {:.3}x", sim.fastest_span);
        // Silence drops do most of it; a single quiet block is held as a
        // hangover, and cuts are paid in advance, so about 6.5 s.
        assert!(back - 10.8 <= 7.5, "speech drained {:.1} s after the burst", back - 10.8);
        assert_eq!(sim.skips, 0);

        // With no pause to drop, only the rate drains it, and the rate falls
        // with the floor: 3 s at 1.10x down to 570 ms, 9 s along the ramp to
        // 120, and 2.5 s at 1.02x to 70 -- about 15 s at the target rate.
        // Cuts come in whole periods and a cut forgives what is left over,
        // so a 4.5 ms period actually plays at about 1.08x where 1.10 is
        // aimed for: about 17 s.
        let mut sim = Sim::new(tone(440.0, 0.5), 1, 0.0).stall(10.0, 800.0);
        sim.run_until(40.0);
        let back = sim.under_after(11.5, STOP_MS).expect("never drained");
        assert!(sim.fastest_span <= RATE_MAX as f64 + 1e-3, "tone: a span played at {:.3}x", sim.fastest_span);
        assert!(back - 10.8 <= 18.0, "tone drained {:.1} s after the burst", back - 10.8);
        assert_eq!(sim.skips, 0);
    }

    #[test]
    fn a_1500_ms_burst_skips_once() {
        let mut sim = Sim::new(speech(), 1, 0.0).stall(10.0, 1500.0);
        sim.run_until(40.0);
        assert_eq!(sim.skips, 1);
        let late = sim.max_floor_after(20.0);
        assert!(late <= START_MS, "floor {late} ms long after the skip");
    }

    #[test]
    fn hysteresis() {
        let mut catchup = Catchup::new(RATE, 1, true);
        let mut changes = Vec::new();
        let mut feed = |catchup: &mut Catchup, ms: f32, blocks: usize| {
            for _ in 0..blocks {
                let was = catchup.engaged();
                catchup.decide(depth(ms), 20.0, false, || None);
                if catchup.engaged() != was {
                    changes.push(catchup.engaged());
                }
            }
        };
        // Wandering 80-115 ms, never past the threshold.
        for i in 0..2000 {
            let ms = 97.5 + 17.5 * (i as f32 * 0.37).sin();
            feed(&mut catchup, ms, 1);
        }
        assert!(!catchup.engaged());
        feed(&mut catchup, 130.0, 100);
        // Between the two thresholds: stays engaged.
        feed(&mut catchup, 100.0, 100);
        feed(&mut catchup, 60.0, 100);
        feed(&mut catchup, 100.0, 100);
        assert_eq!(changes, vec![true, false]);
    }

    #[test]
    fn idle_is_bit_identical() {
        let mut catchup = Catchup::new(RATE, 1, true);
        let mut source = speech();
        let mut silent_seen = 0;
        for _ in 0..500 {
            let original = block(&mut source, BLOCK, 1);
            let mut samples = original.clone();
            let silent = is_silent(level(&samples), Activity::Active);
            silent_seen += silent as u32;
            assert_eq!(catchup.process(&mut samples, depth(50.0), silent), Outcome::Play { changed: false });
            assert!(samples.iter().zip(&original).all(|(a, b)| a.to_bits() == b.to_bits()));
        }
        assert!(silent_seen > 0, "the signal had no pauses to leave alone");
    }

    #[test]
    fn a_short_block_passes_even_when_a_cut_is_due() {
        let mut catchup = Catchup::new(RATE, 1, true);
        // Engage, and run up the debt with nothing to cut.
        for _ in 0..100 {
            catchup.decide(depth(300.0), 20.0, false, || None);
        }
        assert!(catchup.engaged());
        // The pre-skip-trimmed first block of a decoder epoch: 13.5 ms, which
        // a 150 Hz period would fit twice in.
        let original = block(&mut tone(150.0, 0.5), 648, 1);
        let mut samples = original.clone();
        assert_eq!(catchup.process(&mut samples, depth(300.0), false), Outcome::Play { changed: false });
        assert_eq!(samples, original);
    }

    #[test]
    fn stereo_stays_aligned() {
        let mut sim = Sim::new(speech(), 2, 0.0).stall(2.0, 800.0);
        sim.keep = true;
        sim.run_until(12.0);
        let removed = sim.catchup.removed();
        assert!(removed.voiced_ms > 0.0 && removed.silent_ms > 0.0, "{removed:?}");
        for frame in sim.out.as_chunks::<2>().0 {
            assert_eq!(frame[1].to_bits(), (-0.5 * frame[0]).to_bits());
        }
    }

    #[test]
    fn dtx_counts_as_silent() {
        assert!(is_silent(0.3, Activity::Dtx));
        assert!(!is_silent(0.3, Activity::Active));
        let mut catchup = Catchup::new(RATE, 1, true);
        for _ in 0..100 {
            catchup.decide(depth(300.0), 20.0, false, || None);
        }
        let mut source = tone(200.0, 0.5);
        let silent = is_silent(level(&block(&mut source, BLOCK, 1)), Activity::Dtx);
        // The first quiet block is the hangover, the second goes.
        let mut first = block(&mut source, BLOCK, 1);
        assert_eq!(catchup.process(&mut first, depth(300.0), silent), Outcome::Play { changed: false });
        let mut second = block(&mut source, BLOCK, 1);
        assert_eq!(catchup.process(&mut second, depth(300.0), silent), Outcome::Drop);
    }
}
