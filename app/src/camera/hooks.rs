//! Camera hooks that work under any renderer.
//!
//! These mirror `dioxus_cameras::use_devices` / `use_camera_stream`, including
//! their reconnect handling, but publish frames into a [`FrameSlot`] we can
//! actually read instead of into a registry whose only reader is an HTTP server.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cameras::pump::{self, Pump};
use cameras::{open_source, source_label, CameraSource, Device, StreamConfig};
use dioxus::prelude::*;
use futures_channel::mpsc::unbounded;
use futures_util::StreamExt;

use super::FrameSlot;

/// Handle returned by [`use_devices`].
#[derive(Copy, Clone, PartialEq)]
pub struct UseDevices {
    /// Cameras currently reported by the OS.
    pub devices: Signal<Vec<Device>>,
    /// Flips to `true` after the first scan, so the UI can tell "not scanned
    /// yet" apart from "scanned, found nothing".
    pub ready: Signal<bool>,
    /// Rescan on demand.
    pub refresh: Callback<()>,
}

/// Keeps a signal populated with the connected cameras, rescanning on demand.
pub fn use_devices() -> UseDevices {
    let mut devices = use_signal(Vec::<Device>::new);
    let mut ready = use_signal(|| false);

    // Enumeration hits platform APIs and can block for hundreds of
    // milliseconds, so it runs on a worker thread and reports back.
    let tx = use_hook(|| {
        let (tx, mut rx) = unbounded::<Vec<Device>>();
        spawn(async move {
            while let Some(list) = rx.next().await {
                devices.set(list);
                ready.set(true);
            }
        });
        tx
    });

    let refresh = use_callback(move |()| {
        let tx = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("camera-enumerate".into())
            .spawn(move || match cameras::devices() {
                Ok(list) => {
                    let _ = tx.unbounded_send(list);
                }
                Err(error) => tracing::warn!("device enumeration failed: {error}"),
            });
        if let Err(error) = spawned {
            tracing::warn!("could not spawn enumeration thread: {error}");
        }
    });

    use_effect(move || refresh.call(()));

    UseDevices {
        devices,
        ready,
        refresh,
    }
}

/// Lifecycle of the stream driven by [`use_camera_stream`].
#[derive(Clone, Debug)]
pub enum StreamStatus {
    /// No source selected.
    Idle,
    /// Opening the camera on a worker thread.
    Connecting {
        /// Human-readable name of the source being opened.
        label: String,
    },
    /// Camera is open and delivering frames.
    Streaming {
        /// Human-readable name of the active source.
        label: String,
    },
    /// The last open attempt failed. Match on `error` to branch on the kind --
    /// `PermissionDenied` in particular deserves its own message.
    Failed {
        /// Why the camera could not be opened.
        error: cameras::Error,
    },
}

impl PartialEq for StreamStatus {
    fn eq(&self, other: &Self) -> bool {
        // `cameras::Error` is not PartialEq, so compare its rendered form.
        match (self, other) {
            (Self::Idle, Self::Idle) => true,
            (Self::Connecting { label: a }, Self::Connecting { label: b }) => a == b,
            (Self::Streaming { label: a }, Self::Streaming { label: b }) => a == b,
            (Self::Failed { error: a }, Self::Failed { error: b }) => {
                a.to_string() == b.to_string()
            }
            _ => false,
        }
    }
}

impl fmt::Display for StreamStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => f.write_str("No camera selected"),
            Self::Connecting { label } => write!(f, "Connecting to {label}..."),
            Self::Streaming { label } => write!(f, "Streaming: {label}"),
            Self::Failed { error } => write!(f, "Could not open camera: {error}"),
        }
    }
}

/// Handle returned by [`use_camera_stream`].
#[derive(Copy, Clone, PartialEq)]
pub struct UseCameraStream {
    /// Where the stream is in its lifecycle.
    pub status: Signal<StreamStatus>,
    /// Whether the pump is delivering frames. Setting this to `false` parks the
    /// pump but keeps the camera open, so resuming is instant.
    pub active: Signal<bool>,
}

/// Opens `source` and pumps its frames into `slot`.
///
/// Reopens whenever `source` changes and tears the camera down on unmount.
pub fn use_camera_stream(
    slot: FrameSlot,
    source: Signal<Option<CameraSource>>,
    config: StreamConfig,
) -> UseCameraStream {
    let mut status = use_signal(|| StreamStatus::Idle);
    let active = use_signal(|| true);

    // Non-reactive state: nothing should re-render when the pump changes. `Rc`
    // rather than `Arc` because this never leaves the UI thread -- the pump is
    // built on the connect thread and handed over, but only ever stored here.
    let pump_cell = use_hook(|| Rc::new(RefCell::new(None::<Pump>)));

    // Bumped on every source change. `open_source` cannot be cancelled, so a
    // rapid A -> B -> A switch leaves connect threads running; tagging each
    // attempt lets us discard the stale results when they land.
    let generation = use_hook(|| Arc::new(AtomicU64::new(0)));

    let tx = use_hook({
        let pump_cell = Rc::clone(&pump_cell);
        let generation = Arc::clone(&generation);
        move || {
            let (tx, mut rx) = unbounded::<Connect>();
            spawn(async move {
                while let Some(connect) = rx.next().await {
                    if connect.generation != generation.load(Ordering::Relaxed) {
                        continue;
                    }
                    match connect.result {
                        Ok((pump, label)) => {
                            pump::set_active(&pump, *active.peek());
                            *pump_cell.borrow_mut() = Some(pump);
                            status.set(StreamStatus::Streaming { label });
                        }
                        Err(error) => status.set(StreamStatus::Failed { error }),
                    }
                }
            });
            tx
        }
    });

    // Pause and resume without closing the camera.
    {
        let pump_cell = Rc::clone(&pump_cell);
        use_effect(move || {
            let is_active = *active.read();
            if let Some(pump) = pump_cell.borrow().as_ref() {
                pump::set_active(pump, is_active);
            }
        });
    }

    {
        let pump_cell = Rc::clone(&pump_cell);
        let generation = Arc::clone(&generation);
        let slot = slot.clone();
        use_effect(move || {
            let requested = source.read().clone();
            let attempt = generation.fetch_add(1, Ordering::Relaxed) + 1;

            // Dropping the pump signals its worker to stop and frees the
            // camera before we try to open the next one.
            *pump_cell.borrow_mut() = None;
            slot.clear();

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
                        let pump = pump::spawn(camera, move |frame| slot.publish(frame));
                        (pump, label)
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
        use_drop(move || {
            *pump_cell.borrow_mut() = None;
            slot.clear();
        });
    }

    UseCameraStream { status, active }
}

/// Result of one connect attempt, tagged so stale ones can be dropped.
struct Connect {
    generation: u64,
    result: Result<(Pump, String), cameras::Error>,
}
