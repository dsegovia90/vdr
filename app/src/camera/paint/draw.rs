//! Second half of a frame's journey: source texture -> target texture.

use anyrender::{RenderContext, ResourceId};

use super::active::Active;
use super::fit::{uniform_bytes, Card, Mirror};

pub(super) struct Target {
    width: u32,
    height: u32,
    view: wgpu::TextureView,
    pub(super) handle: ResourceId,
}

impl Active {
    /// Returns whether the target was (re)created.
    pub(super) fn ensure_target(
        &mut self,
        ctx: &mut dyn RenderContext,
        width: u32,
        height: u32,
        scale: f64,
    ) -> bool {
        if matches!(&self.target, Some(t) if t.width == width && t.height == height) {
            return false;
        }

        if let Some(old) = self.target.take() {
            ctx.unregister_resource(old.handle);
        }

        // Vello copies out of this into its Rgba8Unorm atlas, so it needs
        // COPY_SRC; RENDER_ATTACHMENT is for our own scaling pass.
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("camera-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let Ok(handle) = ctx.try_register_custom_resource(Box::new(texture)) else {
            return false;
        };
        tracing::debug!("camera target resized to {width}x{height} at scale {scale}");

        self.target = Some(Target {
            width,
            height,
            view,
            handle,
        });
        true
    }

    /// Draws the current frame into the card-sized target, or clears it to
    /// black when there is no frame -- the camera has been released.
    ///
    /// The target is the card, not the canvas, so there are no bars to draw:
    /// the only fitting left is the side crop, which the shader reads as an x
    /// uv scale below 1.
    pub(super) fn draw(&mut self, card: Card, mirror: Mirror) {
        let Some(target) = self.target.as_ref() else {
            return;
        };
        let source = self.source.as_ref();

        if let Some(source) = source {
            self.queue.write_buffer(
                &self.uniforms,
                0,
                // fit.w selects the shader branch: 1.0 = I420, 0.0 = packed.
                &uniform_bytes([
                    card.crop_x,
                    1.0,
                    mirror.as_uniform(),
                    if source.planar { 1.0 } else { 0.0 },
                ]),
            );
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("camera-preview"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("camera-preview"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(source) = source {
                pass.set_pipeline(&self.pipeline.pipeline);
                pass.set_bind_group(0, &source.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
    }
}
