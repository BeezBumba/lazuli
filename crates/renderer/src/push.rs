//! Push-constant replacement using uniform buffers for WebGPU targets.
//!
//! WebGPU does not support push constants (`var<push_constant>` in WGSL is a
//! Vulkan extension unavailable in browsers). When the `webgpu` crate feature
//! is enabled every pipeline that would normally use push constants uses a
//! small `UNIFORM | COPY_DST` buffer with a dedicated bind group instead.

/// A small per-pipeline uniform buffer that replaces push constants.
///
/// Call [`PushUniform::new`] once during pipeline construction to allocate the
/// buffer and bind-group. Before each draw call `update` to upload the data,
/// then bind the group with `set_bind_group`.
pub struct PushUniform {
    layout: wgpu::BindGroupLayout,
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl PushUniform {
    /// Create a uniform buffer of `size` bytes and a bind group for it.
    ///
    /// `size` is rounded up to a multiple of 16 as required by the WebGPU
    /// spec for uniform buffers.
    pub fn new(device: &wgpu::Device, size: u64, label: &str, visibility: wgpu::ShaderStages) -> Self {
        let aligned = (size.max(16) + 15) & !15;

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: aligned,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        Self { layout, buffer, bind_group }
    }

    /// Create a uniform buffer using a pre-existing `BindGroupLayout`.
    ///
    /// Use this when the layout is already owned by a pipeline to avoid
    /// creating a redundant layout object.
    pub fn with_layout(
        device: &wgpu::Device,
        layout: wgpu::BindGroupLayout,
        size: u64,
        label: &str,
    ) -> Self {
        let aligned = (size.max(16) + 15) & !15;

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: aligned,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        Self { layout, buffer, bind_group }
    }

    /// Write `data` into the uniform buffer via the queue.
    pub fn update(&self, queue: &wgpu::Queue, data: &[u8]) {
        queue.write_buffer(&self.buffer, 0, data);
    }

    /// Return the bind group layout for use in pipeline layout construction.
    pub fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// Bind this uniform buffer at the given group index.
    pub fn set_bind_group(&self, pass: &mut wgpu::RenderPass<'_>, index: u32) {
        pass.set_bind_group(index, &self.bind_group, &[]);
    }

    /// Bind this uniform buffer at the given group index for a compute pass.
    pub fn set_bind_group_compute(&self, pass: &mut wgpu::ComputePass<'_>, index: u32) {
        pass.set_bind_group(index, &self.bind_group, &[]);
    }
}
