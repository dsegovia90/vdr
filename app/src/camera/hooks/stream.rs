//! Opening a camera and keeping it pumping into a slot.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use cameras::pump::{self, Pump};
use cameras::{open_source, source_label, CameraSource, StreamConfig};
use dioxus::prelude::*;

use super::channel::use_channel;
use super::StreamStatus;
use crate::camera::format::negotiate;
use crate::camera::FrameSlot;

/// Handle returned by [`use_camera_stream`].
#[derive(Copy, Clone, PartialEq)]
pub struct UseCameraStream {
    /// Where the stream is in its lifecycle. Purely descriptive of the
    /// hardware, never of what the user asked for.
    pub status: Signal<StreamStatus>,
}

/// Opens `source` and pumps its frames into `slot`.
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
        use_effect(move || {
            let requested = source.read().clone();
            let attempt = generation.get() + 1;
            generation.set(attempt);

            let previous = take(&pump_cell, &slot);

            let Some(requested) = requested else {
                status.set(StreamStatus::Idle);
                queue(&ops, "camera-release", move || join(previous));
                return;
            };

            let label = source_label(&requested);
            tracing::info!("opening camera: {label}");
            status.set(StreamStatus::Connecting {
                label: label.clone(),
            });

            let tx = tx.clone();
            let slot = slot.clone();
            queue(&ops, "camera-connect", move || {
                // The device is not free until the old worker has exited.
                join(previous);
                let config = negotiate(&requested, config);
                let result = open_source(requested, config).map(|camera| {
                    let live = Arc::new(AtomicBool::new(true));
                    let pump = pump::spawn(camera, {
                        let live = Arc::clone(&live);
                        move |frame| {
                            if live.load(Ordering::Relaxed) {
                                slot.publish(frame);
                            }
                        }
                    });
                    (Running { pump, live }, label)
                });
                let _ = tx.unbounded_send(Connect {
                    generation: attempt,
                    result,
                });
            });
        });
    }

    {
        let pump_cell = Rc::clone(&pump_cell);
        let ops = Rc::clone(&ops);
        use_drop(move || {
            let previous = take(&pump_cell, &slot);
            queue(&ops, "camera-release", move || join(previous));
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
) {
    let previous = ops.borrow_mut().take();
    let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
        if let Some(previous) = previous {
            let _ = previous.join();
        }
        op();
    });
    match spawned {
        Ok(handle) => *ops.borrow_mut() = Some(handle),
        // Ordering is lost until the next successful spawn, but nothing
        // hangs. See task 8 for the status this strands.
        Err(error) => tracing::warn!("could not spawn {name} thread: {error}"),
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
