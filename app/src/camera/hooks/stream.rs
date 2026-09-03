//! Opening a camera and keeping it pumping into a slot.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

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
        let slot = slot.clone();
        use_effect(move || {
            let requested = source.read().clone();
            let attempt = generation.fetch_add(1, Ordering::Relaxed) + 1;

            release(&pump_cell, &slot);

            let Some(requested) = requested else {
                status.set(StreamStatus::Idle);
                return;
            };

            let label = source_label(&requested);
            tracing::info!("opening camera: {label}");
            status.set(StreamStatus::Connecting {
                label: label.clone(),
            });

            let tx = tx.clone();
            let slot = slot.clone();
            let spawned = std::thread::Builder::new()
                .name("camera-connect".into())
                .spawn(move || {
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
            if let Err(error) = spawned {
                tracing::warn!("could not spawn connect thread: {error}");
            }
        });
    }

    {
        let pump_cell = Rc::clone(&pump_cell);
        use_drop(move || release(&pump_cell, &slot));
    }

    UseCameraStream { status }
}

/// Stops the running camera, if any, and blanks what it left behind.
///
/// Silence first, then drop: dropping alone only signals the worker, which
/// still has one frame in flight. Clearing the slot afterwards is what makes
/// the canvas go black and stay black.
fn release(pump_cell: &Rc<RefCell<Option<Running>>>, slot: &FrameSlot) {
    let previous = pump_cell.borrow_mut().take();
    if let Some(previous) = previous {
        previous.silence();
    }
    slot.clear();
}

/// Result of one connect attempt, tagged so stale ones can be dropped.
struct Connect {
    generation: u64,
    result: Result<(Running, String), cameras::Error>,
}

/// A running pump, plus the switch that stops its frames reaching the slot.
///
/// Dropping a `Pump` only *signals* its worker; a capture already in flight
/// still delivers one more frame, which repaints the canvas after the camera
/// has supposedly been released. `pump::stop_and_join` would close that window
/// too, but it blocks until the worker exits and so cannot run on the UI
/// thread (see task 7). This switch is synchronous and costs one atomic.
struct Running {
    /// Held only for its `Drop`, which signals the capture worker to stop.
    /// Task 7 replaces that with `pump::stop_and_join` on a worker thread.
    #[allow(dead_code)]
    pump: Pump,
    live: Arc<AtomicBool>,
}

impl Running {
    /// Stop frames reaching the slot, effective before this returns.
    fn silence(&self) {
        self.live.store(false, Ordering::Relaxed);
    }
}
