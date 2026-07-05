//! Build-once compute-pass helpers shared by the shape evaluators. Each pass
//! compiles its pipeline exactly once (via the `GpuContext` shader cache) and
//! reuses it; the uniform buffer is rewritten per run (CLAUDE.md GPU rule).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;

use crate::buffer::{MaskBuffer, MASK_FORMAT};

fn out_storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: MASK_FORMAT,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn loadable_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    // Non-filterable float: sampled via textureLoad (no sampler), matching the
    // vignette-LUT precedent — works for R32Float and Rgba16Float inputs alike.
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn compute_pipeline(
    ctx: &GpuContext,
    bgl: &wgpu::BindGroupLayout,
    wgsl: &'static str,
    label: &str,
) -> wgpu::ComputePipeline {
    let module = ctx.shader_module(label, wgsl);
    let layout = ctx
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[bgl],
            push_constant_ranges: &[],
        });
    ctx.device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            module: &module,
            entry_point: "main",
            compilation_options: Default::default(),
            cache: None,
        })
}

fn write_uniform<U: bytemuck::Pod>(ctx: &GpuContext, label: &str, u: &U) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    ctx.device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::bytes_of(u),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        })
}

fn dispatch(
    ctx: &GpuContext,
    pipeline: &wgpu::ComputePipeline,
    bind: &wgpu::BindGroup,
    w: u32,
    h: u32,
) {
    let mut enc = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("mask-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
    }
    ctx.queue.submit([enc.finish()]);
}

/// Uniform-only shape pass: `uniform -> R32Float mask`.
pub(crate) struct GenPass<U: bytemuck::Pod> {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    label: &'static str,
    _marker: std::marker::PhantomData<U>,
}

impl<U: bytemuck::Pod> GenPass<U> {
    pub(crate) fn new(ctx: Arc<GpuContext>, wgsl: &'static str, label: &'static str) -> Self {
        let bgl = ctx
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[out_storage_entry(0), uniform_entry(1)],
            });
        let pipeline = compute_pipeline(&ctx, &bgl, wgsl, label);
        Self {
            ctx,
            pipeline,
            bgl,
            label,
            _marker: std::marker::PhantomData,
        }
    }

    pub(crate) fn run(&self, uniform: U, width: u32, height: u32) -> MaskBuffer {
        let out = MaskBuffer::alloc(&self.ctx, width, height);
        let out_view = out
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let ubuf = write_uniform(&self.ctx, self.label, &uniform);
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(self.label),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&out_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: ubuf.as_entire_binding(),
                    },
                ],
            });
        dispatch(&self.ctx, &self.pipeline, &bind, out.width, out.height);
        out
    }
}

/// Sampled shape pass: `input color texture + uniform -> R32Float mask`.
/// Used by the luma-range shape evaluator (and future color-range).
pub(crate) struct SampledPass<U: bytemuck::Pod> {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    label: &'static str,
    _marker: std::marker::PhantomData<U>,
}

impl<U: bytemuck::Pod> SampledPass<U> {
    pub(crate) fn new(ctx: Arc<GpuContext>, wgsl: &'static str, label: &'static str) -> Self {
        let bgl = ctx
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[
                    loadable_texture_entry(0),
                    out_storage_entry(1),
                    uniform_entry(2),
                ],
            });
        let pipeline = compute_pipeline(&ctx, &bgl, wgsl, label);
        Self {
            ctx,
            pipeline,
            bgl,
            label,
            _marker: std::marker::PhantomData,
        }
    }

    pub(crate) fn run(
        &self,
        uniform: U,
        input: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> MaskBuffer {
        let out = MaskBuffer::alloc(&self.ctx, width, height);
        let out_view = out
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let ubuf = write_uniform(&self.ctx, self.label, &uniform);
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(self.label),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(input),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&out_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: ubuf.as_entire_binding(),
                    },
                ],
            });
        dispatch(&self.ctx, &self.pipeline, &bind, out.width, out.height);
        out
    }
}
