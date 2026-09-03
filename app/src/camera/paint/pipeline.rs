//! The GPU objects every stream can share.

use std::sync::{Arc, Mutex, Weak};

/// Shader, pipeline, layout and sampler.
///
/// Byte-identical for every stream, so they are built once and shared. The
/// alternative is one shader compilation and one pipeline per participant,
/// felt as a hitch each time somebody joins.
pub(super) struct Pipeline {
    pub(super) pipeline: wgpu::RenderPipeline,
    pub(super) bind_group_layout: wgpu::BindGroupLayout,
    pub(super) sampler: wgpu::Sampler,
}

/// Shared handle to [`Pipeline`], built on first use.
///
/// Lazy because the device only arrives at `resume`, and `Weak` so that the
/// pipeline dies once every stream has suspended -- which is what makes it
/// rebuild correctly after a lost device rather than handing out objects
/// belonging to a device that is gone.
#[derive(Clone, Default)]
pub struct PipelineCache(Arc<Mutex<Weak<Pipeline>>>);

impl PipelineCache {
    pub(super) fn get_or_build(&self, device: &wgpu::Device) -> Arc<Pipeline> {
        // A poisoned lock only means a paint source panicked mid-build; the
        // `Weak` it holds is still structurally fine.
        let mut cached = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = cached.upgrade() {
            return existing;
        }
        let built = Arc::new(Pipeline::build(device));
        *cached = Arc::downgrade(&built);
        tracing::debug!("built shared preview pipeline");
        built
    }
}

impl Pipeline {
    fn build(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("camera-preview"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../preview.wgsl").into()),
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

        Self {
            pipeline,
            bind_group_layout,
            sampler,
        }
    }
}
