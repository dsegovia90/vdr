//! Bridges captured frames onto the GPU for dioxus-native to composite.

mod active;
mod draw;
mod fit;
mod rate;
mod upload;

use dioxus::native::{CustomPaintCtx, CustomPaintSource, DeviceHandle, TextureHandle};

use active::Active;
use rate::FrameRate;

pub use fit::Mirror;

use super::FrameSlot;

/// Scales video frames into a canvas-sized texture and hands it to vello.
///
/// Registered through `use_wgpu`, then referenced from RSX as
/// `canvas { "src": paint_source_id }`.
///
/// Knows nothing about cameras: it drains a [`FrameSlot`] and scales whatever
/// turns up. One of these per stream -- a local capture publishing into the
/// slot, or eventually a decoder doing the same for a remote participant.
pub struct VideoPaintSource {
    /// Identifies this stream in logs; several may run at once.
    label: String,
    slot: FrameSlot,
    state: State,
    rate: FrameRate,
    /// Fixed for this stream's lifetime: mirroring is about whose face it is,
    /// which does not change.
    mirror: Mirror,
    /// Last `slot.cleared()` value acted on. When it moves, the camera was
    /// released and the last frame must come off the screen.
    last_cleared: u64,
}

enum State {
    Suspended,
    Active(Box<Active>),
}

impl VideoPaintSource {
    pub fn new(label: impl Into<String>, slot: FrameSlot, mirror: Mirror) -> Self {
        let rate = FrameRate::new(slot.published());
        let last_cleared = slot.cleared();
        Self {
            label: label.into(),
            slot,
            state: State::Suspended,
            rate,
            last_cleared,
            mirror,
        }
    }
}

impl CustomPaintSource for VideoPaintSource {
    fn resume(&mut self, device_handle: &DeviceHandle) {
        self.state = State::Active(Box::new(Active::new(
            device_handle.device.clone(),
            device_handle.queue.clone(),
        )));
        tracing::info!("[{}] paint source resumed", self.label);
    }

    fn suspend(&mut self) {
        self.state = State::Suspended;
        tracing::info!("[{}] paint source suspended", self.label);
    }

    fn render(
        &mut self,
        mut ctx: CustomPaintCtx<'_>,
        width: u32,
        height: u32,
        scale: f64,
    ) -> Option<TextureHandle> {
        let State::Active(active) = &mut self.state else {
            return None;
        };
        if width == 0 || height == 0 {
            return None;
        }

        // A resized canvas means a brand new, blank target, so the last frame
        // has to be drawn into it again even if the camera has not moved on.
        let resized = active.ensure_target(&mut ctx, width, height, scale);

        // Releasing the camera must blank the canvas: a frozen last frame
        // reads as still being on camera.
        let cleared = self.slot.cleared();
        let blanked = cleared != self.last_cleared;
        if blanked {
            self.last_cleared = cleared;
            active.source = None;
        }

        let uploaded = match self.slot.take() {
            Some(frame) => {
                let uploaded = active.upload(&frame);
                if uploaded {
                    self.rate.tick(&self.label, self.slot.published());
                }
                uploaded
            }
            None => false,
        };

        if uploaded || resized || blanked {
            active.draw(width, height, self.mirror);
        }

        active.target.as_ref().map(|t| t.handle.clone())
    }
}
