//! The test pattern in place of a camera (`--test-pattern`).

use std::cell::RefCell;
use std::rc::Rc;

use dioxus_native::prelude::*;

use super::{StreamStatus, UseCameraStream};
use crate::camera::pattern::{self, FPS, HEIGHT, LABEL, WIDTH};
use crate::camera::{FrameSlot, FrameTap};

/// Runs the test pattern into `slot` and `tap` while `enabled`, as
/// [`use_camera_stream`](super::use_camera_stream) runs a camera: the same
/// slot, the same tap, the same status, so nothing downstream can tell.
/// Turning it off clears the slot, as releasing a camera does.
pub fn use_pattern_stream(slot: FrameSlot, enabled: Signal<bool>, tap: Option<FrameTap>) -> UseCameraStream {
    let mut status = use_signal(|| StreamStatus::Idle);
    // Never leaves the interface thread; dropping it stops the pattern.
    let running = use_hook(|| Rc::new(RefCell::new(None::<pattern::Running>)));

    {
        let running = Rc::clone(&running);
        let slot = slot.clone();
        use_effect(move || {
            let on = enabled();
            if on == running.borrow().is_some() {
                return;
            }
            if !on {
                running.borrow_mut().take();
                slot.clear();
                status.set(StreamStatus::Idle);
                return;
            }
            match pattern::spawn(slot.clone(), tap.clone()) {
                Ok(started) => {
                    tracing::info!("camera: {LABEL}, {WIDTH}x{HEIGHT} at {FPS} fps, in place of a camera");
                    *running.borrow_mut() = Some(started);
                    status.set(StreamStatus::Streaming {
                        label: LABEL.to_string(),
                    });
                }
                Err(error) => {
                    tracing::warn!("could not start the test pattern: {error}");
                    status.set(StreamStatus::Unavailable {
                        reason: error.to_string(),
                    });
                }
            }
        });
    }

    use_drop(move || {
        running.borrow_mut().take();
        slot.clear();
    });

    UseCameraStream { status }
}
