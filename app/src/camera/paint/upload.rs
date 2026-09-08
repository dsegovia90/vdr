//! First half of a frame's journey: CPU buffer -> source texture.

use super::active::Active;
use super::fit::row_stride;
use crate::camera::{Frame, Pixels};

pub(super) struct Source {
    pub(super) width: u32,
    pub(super) height: u32,
    format: wgpu::TextureFormat,
    /// True when the source is three planes rather than one packed image.
    /// Drives `fit.w` in the shader, and is part of the identity that decides
    /// whether the textures can be reused.
    pub(super) planar: bool,
    texture: wgpu::Texture,
    /// Chroma planes, present only when `planar`.
    chroma: Option<(wgpu::Texture, wgpu::Texture)>,
    pub(super) bind_group: wgpu::BindGroup,
}

/// One plane's worth of upload: the texture to write and the bytes to write.
struct Plane<'a> {
    width: u32,
    height: u32,
    data: &'a [u8],
    bytes_per_row: u32,
}

impl Active {
    /// Returns whether the frame made it onto the GPU.
    pub(super) fn upload(&mut self, frame: &Frame) -> bool {
        let (width, height) = (frame.width, frame.height);
        if width == 0 || height == 0 {
            return false;
        }

        // Packed BGRA uploads with no conversion at all -- wgpu samples
        // Bgra8Unorm straight into RGBA in the shader. I420 goes up as three R8
        // planes with the YUV->RGB maths in the shader, which beats converting
        // on the CPU and uploads 1.5 bytes/px instead of 4.
        let (format, planar) = match &frame.pixels {
            Pixels::Bgra(_) => (wgpu::TextureFormat::Bgra8Unorm, false),
            Pixels::Rgba(_) => (wgpu::TextureFormat::Rgba8Unorm, false),
            Pixels::I420(_) => (wgpu::TextureFormat::R8Unorm, true),
        };

        let (cw, ch) = (width / 2, height / 2);
        let planes: Vec<Plane<'_>> = match &frame.pixels {
            Pixels::Bgra(data) | Pixels::Rgba(data) => {
                let bytes_per_row = row_stride(data.len(), width, height);
                if data.len() < (bytes_per_row as usize) * (height as usize) {
                    tracing::warn!("frame buffer too small for {width}x{height}");
                    return false;
                }
                vec![Plane { width, height, data, bytes_per_row }]
            }
            Pixels::I420(data) => {
                let y_len = (width as usize) * (height as usize);
                let c_len = (cw as usize) * (ch as usize);
                if cw == 0 || ch == 0 || data.len() < y_len + 2 * c_len {
                    tracing::warn!(
                        "I420 buffer is {} bytes, expected {} for {width}x{height}",
                        data.len(),
                        y_len + 2 * c_len
                    );
                    return false;
                }
                vec![
                    Plane { width, height, data: &data[..y_len], bytes_per_row: width },
                    Plane { width: cw, height: ch, data: &data[y_len..y_len + c_len], bytes_per_row: cw },
                    Plane {
                        width: cw,
                        height: ch,
                        data: &data[y_len + c_len..y_len + 2 * c_len],
                        bytes_per_row: cw,
                    },
                ]
            }
        };

        let reusable = matches!(
            &self.source,
            Some(s) if s.width == width && s.height == height && s.format == format && s.planar == planar
        );
        if !reusable {
            let make = |label: &str, w: u32, h: u32| {
                self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            };

            let texture = make("camera-frame", width, height);
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let chroma = planar.then(|| (make("camera-frame-u", cw, ch), make("camera-frame-v", cw, ch)));
            let (u_view, v_view) = match &chroma {
                Some((u, v)) => (
                    u.create_view(&wgpu::TextureViewDescriptor::default()),
                    v.create_view(&wgpu::TextureViewDescriptor::default()),
                ),
                // Packed mode still has to fill bindings 3 and 4; the shader
                // never samples them.
                None => (self.pipeline.blank_plane.clone(), self.pipeline.blank_plane.clone()),
            };

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
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&u_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&v_view),
                    },
                ],
            });
            tracing::debug!("source now {width}x{height} {format:?} planar={planar}");
            self.source = Some(Source {
                width,
                height,
                format,
                planar,
                texture,
                chroma,
                bind_group,
            });
        }

        let source = self.source.as_ref().expect("source just set");
        let textures: Vec<&wgpu::Texture> = match &source.chroma {
            Some((u, v)) => vec![&source.texture, u, v],
            None => vec![&source.texture],
        };

        for (plane, texture) in planes.iter().zip(textures) {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                plane.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(plane.bytes_per_row),
                    rows_per_image: Some(plane.height),
                },
                wgpu::Extent3d {
                    width: plane.width,
                    height: plane.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        true
    }
}
