//! Bridges captured frames onto the GPU for dioxus-native to composite.

use cameras::{to_rgba8, Frame, PixelFormat};
use dioxus::native::{CustomPaintCtx, CustomPaintSource, DeviceHandle, TextureHandle};

use super::FrameSlot;

/// Scales camera frames into a canvas-sized texture and hands it to vello.
///
/// Registered through `use_wgpu`, then referenced from RSX as
/// `canvas { "src": paint_source_id }`.
pub struct CameraPaintSource {
    /// Identifies this stream in logs; several may run at once.
    label: String,
    slot: FrameSlot,
    state: State,
    rate: FrameRate,
    /// Last `slot.cleared()` value acted on. When it moves, the camera was
    /// released and the last frame must come off the screen.
    last_cleared: u64,
}

enum State {
    Suspended,
    Active(Box<Active>),
}

struct Active {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    /// The most recent frame, at capture resolution.
    source: Option<Source>,
    /// What vello reads, always exactly the canvas size.
    target: Option<Target>,
}

struct Source {
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

struct Target {
    width: u32,
    height: u32,
    view: wgpu::TextureView,
    handle: TextureHandle,
}

impl CameraPaintSource {
    pub fn new(label: impl Into<String>, slot: FrameSlot) -> Self {
        let rate = FrameRate::new(slot.published());
        let last_cleared = slot.cleared();
        Self {
            label: label.into(),
            slot,
            state: State::Suspended,
            rate,
            last_cleared,
        }
    }
}

impl CustomPaintSource for CameraPaintSource {
    fn resume(&mut self, device_handle: &DeviceHandle) {
        self.state = State::Active(Box::new(Active::new(
            device_handle.device.clone(),
            device_handle.queue.clone(),
        )));
        tracing::info!("[{}] paint source resumed", self.label);
    }

    fn suspend(&mut self) {
        self.state = State::Suspended;
        tracing::info!("camera paint source suspended");
    }

    fn render(
        &mut self,
        mut ctx: CustomPaintCtx<'_>,
        width: u32,
        height: u32,
        _scale: f64,
    ) -> Option<TextureHandle> {
        let State::Active(active) = &mut self.state else {
            return None;
        };
        if width == 0 || height == 0 {
            return None;
        }

        // A resized canvas means a brand new, blank target, so the last frame
        // has to be drawn into it again even if the camera has not moved on.
        let resized = active.ensure_target(&mut ctx, width, height);

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
            active.draw(width, height);
        }

        active.target.as_ref().map(|t| t.handle.clone())
    }
}

impl Active {
    fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("camera-preview"),
            source: wgpu::ShaderSource::Wgsl(include_str!("preview.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera-preview"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("camera-preview"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("camera-preview"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    // Must match what vello copies out of.
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("camera-preview"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera-preview"),
            size: std::mem::size_of::<[f32; 4]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
            sampler,
            uniforms,
            source: None,
            target: None,
        }
    }

    /// Returns whether the target was (re)created.
    fn ensure_target(&mut self, ctx: &mut CustomPaintCtx<'_>, width: u32, height: u32) -> bool {
        if matches!(&self.target, Some(t) if t.width == width && t.height == height) {
            return false;
        }

        if let Some(old) = self.target.take() {
            ctx.unregister_texture(old.handle);
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
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let handle = ctx.register_texture(texture);
        tracing::debug!("camera target resized to {width}x{height}");

        self.target = Some(Target {
            width,
            height,
            view,
            handle,
        });
        true
    }

    /// Returns whether the frame made it onto the GPU.
    fn upload(&mut self, frame: &Frame) -> bool {
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
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
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

    /// Draws the current frame into the target, or clears the target to black
    /// when there is no frame -- the camera has been released.
    fn draw(&mut self, width: u32, height: u32) {
        let Some(target) = self.target.as_ref() else {
            return;
        };
        let source = self.source.as_ref();

        if let Some(source) = source {
            let fit = contain(source.width, source.height, width, height);
            self.queue.write_buffer(
                &self.uniforms,
                0,
                bytemuck_cast(&[fit[0], fit[1], 1.0, 0.0]),
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
            });
            if let Some(source) = source {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &source.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
    }
}

/// uv scale that fits `src` inside `dst` while preserving aspect ratio.
///
/// One axis comes out at 1.0 and the other above it, which the shader reads as
/// "sample beyond the edge here", producing letterbox bars.
fn contain(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> [f32; 2] {
    let (src_w, src_h) = (src_w as f32, src_h as f32);
    let (dst_w, dst_h) = (dst_w as f32, dst_h as f32);
    if src_w <= 0.0 || src_h <= 0.0 || dst_w <= 0.0 || dst_h <= 0.0 {
        return [1.0, 1.0];
    }
    let scale = (dst_w / src_w).min(dst_h / src_h);
    [dst_w / (src_w * scale), dst_h / (src_h * scale)]
}

/// Bytes per row of a tightly packed RGBA buffer, tolerating the row padding
/// `to_rgba8` leaves in place for frames that were already RGBA.
fn row_stride(len: usize, width: u32, height: u32) -> u32 {
    let packed = width * 4;
    if height == 0 {
        return packed;
    }
    let stride = len / height as usize;
    if len.is_multiple_of(height as usize) && stride >= packed as usize {
        stride as u32
    } else {
        packed
    }
}

fn bytemuck_cast(values: &[f32; 4]) -> &[u8] {
    // SAFETY: `f32` has no padding or invalid bit patterns, so any `[f32; 4]`
    // is valid to read as the same number of bytes.
    unsafe {
        std::slice::from_raw_parts(
            values.as_ptr().cast::<u8>(),
            std::mem::size_of::<[f32; 4]>(),
        )
    }
}

/// Compares frames delivered by the camera against frames that actually reach
/// the GPU.
///
/// The difference is the interesting number: the renderer can only upload
/// inside `render()`, so if vello paints less often than the camera captures,
/// frames are dropped in `FrameSlot` and the two rates diverge.
struct FrameRate {
    frames: u32,
    since: std::time::Instant,
    published_at_start: u64,
}

impl FrameRate {
    const REPORT_EVERY: u32 = 120;

    fn new(published: u64) -> Self {
        Self {
            frames: 0,
            since: std::time::Instant::now(),
            published_at_start: published,
        }
    }

    fn tick(&mut self, label: &str, published: u64) {
        self.frames += 1;
        if self.frames < Self::REPORT_EVERY {
            return;
        }
        let elapsed = self.since.elapsed().as_secs_f32();
        if elapsed > 0.0 {
            let captured = published.saturating_sub(self.published_at_start);
            tracing::debug!(
                "[{label}] camera {:.1} fps -> preview {:.1} fps ({} dropped)",
                captured as f32 / elapsed,
                self.frames as f32 / elapsed,
                captured.saturating_sub(self.frames as u64),
            );
        }
        *self = Self::new(published);
    }
}
