//! Bridges captured frames onto the GPU for dioxus-native to composite.

mod active;
mod draw;
mod fit;
mod pipeline;
mod rate;
mod upload;

use anyrender::{PaintRef, PaintScene, RenderContext, ResourceId, Scene};
use blitz_dom::node::ComputedStyles;
use dioxus_native::{DeviceHandle, Widget};
use peniko::kurbo::{Affine, RoundedRect};
use peniko::{Fill, ImageBrush, ImageSampler};
use dioxus_native::prelude::*;

use active::Active;
use rate::FrameRate;

pub use fit::Mirror;
use fit::Card;

/// How round a video card's corners are, in CSS pixels; scaled by the window's
/// pixel density when painted, so it looks the same on a Retina screen.
const CORNER_RADIUS: f64 = 12.0;
pub use pipeline::PipelineCache;

/// The pipeline cache every stream in this app shares.
///
/// Self-provisioning: the first caller installs it at the root, so there is no
/// provider to wire up and none to forget. A forgotten provider here would not
/// fail loudly -- every stream would just quietly build its own pipeline, which
/// is exactly the state this replaces.
pub fn use_pipeline_cache() -> PipelineCache {
    use_hook(|| {
        // `provide_root_context` is not in dioxus's prelude, only dioxus_core's.
        // `provide_context` would be wrong here: it installs at the *calling*
        // scope, so every stream would get its own cache.
        try_consume_context::<PipelineCache>()
            .unwrap_or_else(|| dioxus_core::provide_root_context(PipelineCache::default()))
    })
}

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
    /// Shared with every other stream, so they do not each recompile the shader.
    cache: PipelineCache,
    /// Last `slot.cleared()` value acted on. When it moves, the camera was
    /// released and the last frame must come off the screen.
    last_cleared: u64,
}

enum State {
    Suspended,
    Active(Box<Active>),
}

impl VideoPaintSource {
    pub fn new(
        label: impl Into<String>,
        slot: FrameSlot,
        mirror: Mirror,
        cache: PipelineCache,
    ) -> Self {
        let rate = FrameRate::new(slot.published());
        let last_cleared = slot.cleared();
        Self {
            label: label.into(),
            slot,
            state: State::Suspended,
            rate,
            last_cleared,
            mirror,
            cache,
        }
    }
}

impl Widget for VideoPaintSource {
    fn can_create_surfaces(&mut self, render_ctx: &mut dyn RenderContext) {
        let Some(any) = render_ctx.renderer_specific_context() else {
            return;
        };
        let Ok(device_handle) = any.downcast::<DeviceHandle>() else {
            return;
        };
        let pipeline = self.cache.get_or_build(&device_handle.device);
        self.state = State::Active(Box::new(Active::new(
            device_handle.device.clone(),
            device_handle.queue.clone(),
            pipeline,
        )));
        tracing::info!("[{}] paint source resumed", self.label);
    }

    fn destroy_surfaces(&mut self) {
        self.state = State::Suspended;
        tracing::info!("[{}] paint source suspended", self.label);
    }

    /// Always `true`: video needs a repaint every vsync, and nothing else would
    /// ask for one. Blitz polls this only after a frame is drawn, so returning
    /// `false` while idle would stop the loop with no way for a frame arriving
    /// on the camera or decoder thread to restart it. The old `canvas` path
    /// behaved the same way (a document with a canvas counted as animating).
    fn requires_redraw(&self) -> bool {
        true
    }

    fn paint(
        &mut self,
        render_ctx: &mut dyn RenderContext,
        _styles: &ComputedStyles,
        width: u32,
        height: u32,
        scale: f64,
    ) -> Scene {
        let mut scene = Scene::new();
        if let Some((id, card)) = self.render(render_ctx, width, height, scale) {
            // **The renderer draws the card's shape, not us.** The texture is
            // exactly the card, so filling a rounded rectangle with it gives
            // anti-aliased corners without the shader knowing about them, and
            // everything outside the card is simply never painted -- no alpha
            // for the renderer to interpret.
            let (x, y) = (card.x as f64, card.y as f64);
            let shape = RoundedRect::new(
                x,
                y,
                x + card.width as f64,
                y + card.height as f64,
                CORNER_RADIUS * scale,
            );
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                PaintRef::Resource(ImageBrush {
                    image: id,
                    sampler: ImageSampler::default(),
                }),
                // The image is placed at the brush's origin, so move it to the
                // card's corner.
                Some(Affine::translate((x, y))),
                &shape,
            );
        }
        scene
    }
}

impl VideoPaintSource {
    fn render(
        &mut self,
        ctx: &mut dyn RenderContext,
        width: u32,
        height: u32,
        scale: f64,
    ) -> Option<(ResourceId, Card)> {
        let State::Active(active) = &mut self.state else {
            return None;
        };
        if width == 0 || height == 0 {
            return None;
        }

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

        // Laid out after the upload, because the card's shape is the video's.
        // With no video yet -- or a camera that was turned off -- a 16:9 card
        // stands in, so the tile keeps its place rather than vanishing.
        let (video_w, video_h) = active
            .source
            .as_ref()
            .map_or((16, 9), |source| (source.width, source.height));
        let card = fit::card(video_w, video_h, width, height);
        let resized = active.ensure_target(ctx, card.width, card.height, scale);

        if uploaded || resized || blanked {
            active.draw(card, self.mirror);
        }

        active.target.as_ref().map(|t| (t.handle, card))
    }
}
