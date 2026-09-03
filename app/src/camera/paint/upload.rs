//! First half of a frame's journey: CPU buffer -> source texture.

use cameras::{to_rgba8, Frame, PixelFormat};

use super::active::Active;
use super::fit::row_stride;

pub(super) struct Source {
    pub(super) width: u32,
    pub(super) height: u32,
    format: wgpu::TextureFormat,
    texture: wgpu::Texture,
    pub(super) bind_group: wgpu::BindGroup,
}

impl Active {
    /// Returns whether the frame made it onto the GPU.
    pub(super) fn upload(&mut self, frame: &Frame) -> bool {
        let (width, height) = (frame.width, frame.height);
        if width == 0 || height == 0 {
            return false;
        }

        // BGRA is what we ask the camera for, and wgpu samples it straight into
        // RGBA in the shader, so it needs no CPU conversion at all. Anything
        // else (NV12, YUYV, MJPEG) goes through the CPU decoder first.
        let packed;
        let (format, data, bytes_per_row) = match frame.pixel_format {
            PixelFormat::Bgra8 => (
                wgpu::TextureFormat::Bgra8Unorm,
                &frame.plane_primary[..],
                frame.stride.max(width * 4),
            ),
            _ => {
                packed = match to_rgba8(frame) {
                    Ok(rgba) => rgba,
                    Err(error) => {
                        tracing::warn!("frame conversion failed: {error}");
                        return false;
                    }
                };
                let stride = row_stride(packed.len(), width, height);
                (wgpu::TextureFormat::Rgba8Unorm, &packed[..], stride)
            }
        };

        if data.len() < (bytes_per_row as usize) * (height as usize) {
            tracing::warn!("frame buffer too small for {width}x{height}");
            return false;
        }

        if !matches!(&self.source, Some(s) if s.width == width && s.height == height && s.format == format)
        {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("camera-frame"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("camera-frame"),
                layout: &self.pipeline.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.pipeline.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.uniforms.as_entire_binding(),
                    },
                ],
            });
            tracing::debug!("source now {width}x{height} {format:?}");
            self.source = Some(Source {
                width,
                height,
                format,
                texture,
                bind_group,
            });
        }

        let source = self.source.as_ref().expect("source just set");
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        true
    }
}
