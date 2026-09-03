//! Opening a camera and keeping it pumping into a slot.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use cameras::pump::{self, Pump};
use cameras::{open_source, source_label, CameraSource, StreamConfig};
use dioxus::prelude::*;

use super::channel::use_channel;
use super::StreamStatus;
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
    // attempt lets us discard the stale results when they land.
    let generation = use_hook(|| Arc::new(AtomicU64::new(0)));

    let ops = use_hook(|| Rc::new(RefCell::new(None::<JoinHandle<()>>)));

    let tx = use_channel({
        let pump_cell = Rc::clone(&pump_cell);
        let generation = Arc::clone(&generation);
        move |connect: Connect| {
            if connect.generation != generation.load(Ordering::Relaxed) {
                // Superseded by a later attempt. Silence it before it drops,
                // or a camera nobody asked for keeps painting.
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
        let generation = Arc::clone(&generation);
        let ops = Rc::clone(&ops);
        let slot = slot.clone();
        use_effect(move || {
            let requested = source.read().clone();
            let attempt = generation.fetch_add(1, Ordering::Relaxed) + 1;

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
                // The device is not free until the old worker has actually
                // exited, so this join has to happen before `open_source`.
                join(previous);
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
/// has one frame in flight. Clearing the slot afterwards is what makes the
/// canvas go black and stay black.
///
/// Returns the camera rather than dropping it, because the caller still has to
/// [`join`] it and that must not happen here -- this runs on the UI thread.
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
/// Teardown and open must not overlap. `stop_and_join` waits for the capture
/// thread to notice the shutdown flag, which it only checks between frames --
/// up to `DEFAULT_FRAME_TIMEOUT`, 500ms in `cameras`. Calling `open_source`
/// inside that window fails with `DeviceInUse`, and turning the camera off and
/// straight back on is a normal thing for a user to do.
///
/// A mutex would not be enough: it hands out access in arbitrary order, and
/// what this needs is *sequence*. Each operation joining its predecessor gives
/// that by construction.
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
        // The chain is broken but nothing is stuck: any thread already running
        // is simply no longer waited on. See task 8 for the status it strands.
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
/// The two halves stop the camera on different timescales. `silence` is
/// immediate and safe on the UI thread, so the canvas can blank at once.
/// `stop_and_join` is what actually frees the device, but it blocks for as
/// long as the worker takes to notice.
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
