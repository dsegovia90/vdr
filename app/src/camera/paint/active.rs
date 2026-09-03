//! Per-stream GPU state: everything the shared pipeline does not cover.

use std::sync::Arc;

use super::draw::Target;
use super::pipeline::Pipeline;
use super::upload::Source;

pub(super) struct Active {
    pub(super) device: wgpu::Device,
    pub(super) queue: wgpu::Queue,
    /// Shared with every other stream on this device.
    pub(super) pipeline: Arc<Pipeline>,
    /// Per-stream: holds this stream's fit scale and mirror flag.
    pub(super) uniforms: wgpu::Buffer,
    /// The most recent frame, at capture resolution.
    pub(super) source: Option<Source>,
    /// What vello reads, always exactly the canvas size.
    pub(super) target: Option<Target>,
}

impl Active {
    pub(super) fn new(device: wgpu::Device, queue: wgpu::Queue, pipeline: Arc<Pipeline>) -> Self {
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
            uniforms,
            source: None,
            target: None,
        }
    }
}
