//! Opening a camera and keeping it pumping into a slot.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cameras::pump::{self, Pump};
use cameras::{open_source, source_label, CameraSource, StreamConfig};
use dioxus_native::prelude::*;

use super::StreamStatus;
use crate::camera::format::negotiate;
use crate::camera::{from_capture, FrameSlot, FrameTap};
use crate::channel::use_channel;

/// How often the watchdog samples the frame counter.
const WATCHDOG_TICK: Duration = Duration::from_secs(1);

/// Silence longer than this while `Streaming` means the capture thread is gone.
///
/// The pump tolerates 500ms gaps itself (`DEFAULT_FRAME_TIMEOUT`) and the
/// slowest mode any test camera offers is 5fps, so this is ~15 missed frames --
/// far outside anything a working camera produces.
const LOST_AFTER: Duration = Duration::from_secs(3);

/// Handle returned by [`use_camera_stream`].
#[derive(Copy, Clone, PartialEq)]
pub struct UseCameraStream {
    /// Where the stream is in its lifecycle. Purely descriptive of the
    /// hardware, never of what the user asked for.
    pub status: Signal<StreamStatus>,
}

/// Opens `source` and pumps its frames into `slot`, and into `tap` if one is
/// given.
///
/// The tap is the network path's branch of the fan-out; see [`FrameTap`] for
/// why it is not another reader of the slot.
///
/// Reopens whenever `source` changes and tears the camera down on unmount.
/// Setting `source` to `None` **releases the device**, that is the only way
/// to guarantee the OS camera indicator goes out, so it is how turning video
/// off is expressed. The cost is that turning it back on pays a full
/// `open_source()` reconnect, which `StreamStatus::Connecting` covers in the UI.
pub fn use_camera_stream(
    slot: FrameSlot,
    source: Signal<Option<CameraSource>>,
    config: StreamConfig,
    tap: Option<FrameTap>,
) -> UseCameraStream {
    let mut status = use_signal(|| StreamStatus::Idle);

    // Non-reactive state: nothing should re-render when the pump changes. `Rc`
    // rather than `Arc` because this never leaves the UI thread -- the pump is
    // built on the connect thread and handed over, but only ever stored here.
    let pump_cell = use_hook(|| Rc::new(RefCell::new(None::<Running>)));

    // Bumped on every source change. `open_source` cannot be cancelled, so a
    // rapid A -> B -> A switch leaves connect threads running; tagging each
    // attempt lets us discard the stale results when they land. Only the copy
    // in `Connect` crosses to a thread, so this stays `Rc<Cell<_>>`.
    let generation = use_hook(|| Rc::new(Cell::new(0u64)));

    let ops = use_hook(|| Rc::new(RefCell::new(None::<JoinHandle<()>>)));

    let tx = use_channel({
        let pump_cell = Rc::clone(&pump_cell);
        let generation = Rc::clone(&generation);
        move |connect: Connect| {
            if connect.generation != generation.get() {
                // Silence before dropping, or a camera nobody asked for
                // keeps painting.
                if let Ok((running, _)) = &connect.result {
                    running.silence();
                }
                return;
            }
            match connect.result {
                Ok((running, label)) => {
                    *pump_cell.borrow_mut() = Some(running);
                    status.set(StreamStatus::Streaming { label });
                }
                Err(error) => status.set(StreamStatus::Failed { error }),
            }
        }
    });

    {
        let pump_cell = Rc::clone(&pump_cell);
        let generation = Rc::clone(&generation);
        let ops = Rc::clone(&ops);
        let slot = slot.clone();
        let tap = tap.clone();
        use_effect(move || {
            let requested = source.read().clone();
            let attempt = generation.get() + 1;
            generation.set(attempt);

            let previous = take(&pump_cell, &slot);

            let Some(requested) = requested else {
                status.set(StreamStatus::Idle);
                if let Err(error) = queue(&ops, "camera-release", move || join(previous)) {
                    tracing::warn!("could not spawn release thread: {error}");
                }
                return;
            };

            let label = source_label(&requested);
            tracing::info!("opening camera: {label}");
            status.set(StreamStatus::Connecting {
                label: label.clone(),
            });

            let tx = tx.clone();
            let slot = slot.clone();
            let tap = tap.clone();
            let spawned = queue(&ops, "camera-connect", move || {
                // The device is not free until the old worker has exited.
                join(previous);
                let config = negotiate(&requested, config);
                let result = open_source(requested, config).map(|camera| {
                    let live = Arc::new(AtomicBool::new(true));
                    let pump = pump::spawn(camera, {
                        let live = Arc::clone(&live);
                        move |frame| {
                            if !live.load(Ordering::Relaxed) {
                                return;
                            }
                            // The one place every captured frame passes
                            // through, which is why the fan-out is here
                            // (task 2). Convert once and share: the clone is a
                            // refcount bump, a second `from_capture` would be
                            // a second conversion.
                            let frame = from_capture(&frame);
                            if let Some(tap) = &tap {
                                tap.publish(frame.clone());
                            }
                            slot.publish(frame);
                        }
                    });
                    (Running { pump, live }, label)
                });
                let _ = tx.unbounded_send(Connect {
                    generation: attempt,
                    result,
                });
            });
            // Nothing will ever send a `Connect`, so without this the status
            // sits on `Connecting` forever with no way out -- changing camera
            // just queues another spawn that may fail the same way.
            if let Err(error) = spawned {
                tracing::warn!("could not spawn connect thread: {error}");
                status.set(StreamStatus::Unavailable {
                    reason: error.to_string(),
                });
            }
        });
    }

    {
        let pump_cell = Rc::clone(&pump_cell);
        let ops = Rc::clone(&ops);
        let slot = slot.clone();
        use_drop(move || {
            let previous = take(&pump_cell, &slot);
            let _ = queue(&ops, "camera-release", move || join(previous));
        });
    }

    // A dead capture thread is silent, so it can only be inferred from frames
    // stopping. `published` is monotonic across camera switches, which is what
    // makes sampling it enough -- no extra bookkeeping in the hot path.
    {
        let slot = slot.clone();
        let mut watchdog = Watchdog::new(slot.published(), Instant::now());
        let tick = use_channel(move |()| {
            if !watchdog.observe(slot.published(), Instant::now()) {
                return;
            }
            // Only `Streaming` can go quiet unexpectedly. `Connecting` has not
            // started yet and `Idle` is the user's own doing.
            let label = match &*status.peek() {
                StreamStatus::Streaming { label } => label.clone(),
                _ => return,
            };
            tracing::warn!("no frames from {label} for {LOST_AFTER:?}; treating as lost");
            status.set(StreamStatus::Lost { label });
        });

        use_hook(move || {
            // Exits on its own once the component unmounts and the receiver
            // goes with it.
            let spawned = std::thread::Builder::new()
                .name("camera-watchdog".into())
                .spawn(move || {
                    while tick.unbounded_send(()).is_ok() {
                        std::thread::sleep(WATCHDOG_TICK);
                    }
                });
            if let Err(error) = spawned {
                tracing::warn!("could not spawn watchdog thread: {error}");
            }
        });
    }

    UseCameraStream { status }
}

/// Takes the running camera out of `pump_cell` and blanks what it left behind.
///
/// Silence first: dropping a pump alone only signals its worker, which still
/// has one frame in flight. Returns the camera rather than dropping it, since
/// [`join`] must not run here on the UI thread.
#[must_use]
fn take(pump_cell: &Rc<RefCell<Option<Running>>>, slot: &FrameSlot) -> Option<Running> {
    let previous = pump_cell.borrow_mut().take();
    if let Some(previous) = &previous {
        previous.silence();
    }
    slot.clear();
    previous
}

/// Waits for a stopped camera's worker to exit. Blocks; never call it on the
/// UI thread.
fn join(previous: Option<Running>) {
    if let Some(previous) = previous {
        previous.stop_and_join();
    }
}

/// Runs `op` on a worker thread, after every camera operation queued before it.
///
/// Teardown and open must not overlap: a worker can hold the device for up to
/// one frame timeout (500ms) after being told to stop, and `open_source` inside
/// that window fails with `DeviceInUse`. A mutex would not do -- it grants
/// access in arbitrary order, and what this needs is sequence.
fn queue(
    ops: &Rc<RefCell<Option<JoinHandle<()>>>>,
    name: &'static str,
    op: impl FnOnce() + Send + 'static,
) -> std::io::Result<()> {
    let previous = ops.borrow_mut().take();
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            if let Some(previous) = previous {
                let _ = previous.join();
            }
            op();
        });
    match spawned {
        Ok(handle) => {
            *ops.borrow_mut() = Some(handle);
            Ok(())
        }
        // Ordering is lost until the next successful spawn, but nothing hangs:
        // a thread already running is simply no longer waited on.
        Err(error) => Err(error),
    }
}

/// Watches the frame counter for a capture thread that has stopped delivering.
///
/// Split out from the tick handler so the timing can be tested without a
/// camera: silence is the only evidence available, and getting the reset wrong
/// would either cry wolf on every reconnect or never fire at all.
struct Watchdog {
    seen: u64,
    since: Instant,
}

impl Watchdog {
    /// Takes `now` rather than reading the clock, so the timing is testable:
    /// an internal `Instant::now()` here lands microseconds after the caller's
    /// and silently shortens the first interval.
    fn new(published: u64, now: Instant) -> Self {
        Self {
            seen: published,
            since: now,
        }
    }

    /// Returns whether the stream has now been silent long enough to be dead.
    fn observe(&mut self, published: u64, now: Instant) -> bool {
        if published != self.seen {
            self.seen = published;
            self.since = now;
            return false;
        }
        now.duration_since(self.since) >= LOST_AFTER
    }
}

/// Result of one connect attempt, tagged so stale ones can be dropped.
struct Connect {
    generation: u64,
    result: Result<(Running, String), cameras::Error>,
}

/// A running pump, plus the switch that stops its frames reaching the slot.
///
/// Stopping happens on two timescales: `silence` is immediate and UI-thread
/// safe, `stop_and_join` blocks but is what actually frees the device.
struct Running {
    pump: Pump,
    live: Arc<AtomicBool>,
}

impl Running {
    /// Stop frames reaching the slot, effective before this returns.
    fn silence(&self) {
        self.live.store(false, Ordering::Relaxed);
    }

    /// Block until the capture worker exits, which is when the OS considers
    /// the device free and the indicator light goes out.
    fn stop_and_join(self) {
        pump::stop_and_join(self.pump);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delivering_camera_is_never_lost() {
        let start = Instant::now();
        let mut watchdog = Watchdog::new(0, start);
        for frame in 1..100 {
            let now = start + LOST_AFTER * frame;
            assert!(!watchdog.observe(frame as u64, now), "frame {frame}");
        }
    }

    #[test]
    fn silence_past_the_threshold_is_lost() {
        let start = Instant::now();
        let mut watchdog = Watchdog::new(7, start);
        assert!(!watchdog.observe(7, start));
        assert!(watchdog.observe(7, start + LOST_AFTER));
    }

    #[test]
    fn silence_short_of_the_threshold_is_not() {
        let start = Instant::now();
        let mut watchdog = Watchdog::new(7, start);
        assert!(!watchdog.observe(7, start + LOST_AFTER - Duration::from_millis(1)));
    }

    #[test]
    fn a_resumed_counter_restarts_the_clock() {
        // The reconnect case. Frames stop while the old camera is torn down,
        // then the new one starts counting from where the old left off -- the
        // clock has to reset, or the next tick reports a camera that is fine.
        let start = Instant::now();
        let mut watchdog = Watchdog::new(7, start);
        assert!(!watchdog.observe(7, start + LOST_AFTER - Duration::from_millis(1)));
        assert!(!watchdog.observe(8, start + LOST_AFTER));
        assert!(!watchdog.observe(8, start + LOST_AFTER + Duration::from_millis(1)));
    }
}
