//! Mask compositing: fold `(MaskBuffer, CompositeMode)` entries into one mask,
//! then optionally invert. The operators mirror `composite_scalar` exactly. The
//! compositor is also surfaced as a generic `Node<MaskBuffer>` so it drops into
//! the unmodified `Graph<MaskBuffer>` executor (contract 4).

use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Node};

use crate::buffer::{MaskBuffer, MASK_FORMAT};
use crate::model::CompositeMode;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FoldUniform {
    mode: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

fn loadable(binding: u32) -> wgpu::BindGroupLayoutEntry {
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

fn storage_out(binding: u32) -> wgpu::BindGroupLayoutEntry {
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

fn uniform(binding: u32) -> wgpu::BindGroupLayoutEntry {
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

fn build_pipeline(
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

/// Build-once fold + invert pipelines. `composite` orchestrates the fold chain.
pub struct CompositePass {
    ctx: Arc<GpuContext>,
    fold_bgl: wgpu::BindGroupLayout,
    fold_pipeline: wgpu::ComputePipeline,
    invert_bgl: wgpu::BindGroupLayout,
    invert_pipeline: wgpu::ComputePipeline,
}

impl CompositePass {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        let fold_bgl = ctx
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mask-fold"),
                entries: &[loadable(0), loadable(1), storage_out(2), uniform(3)],
            });
        let fold_pipeline = build_pipeline(
            &ctx,
            &fold_bgl,
            include_str!("shaders/mask_fold.wgsl"),
            "mask-fold",
        );
        let invert_bgl = ctx
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mask-invert"),
                entries: &[loadable(0), storage_out(1)],
            });
        let invert_pipeline = build_pipeline(
            &ctx,
            &invert_bgl,
            include_str!("shaders/mask_invert.wgsl"),
            "mask-invert",
        );
        Self {
            ctx,
            fold_bgl,
            fold_pipeline,
            invert_bgl,
            invert_pipeline,
        }
    }

    fn fold_into(&self, acc: &MaskBuffer, b: &MaskBuffer, mode: CompositeMode) -> MaskBuffer {
        use wgpu::util::DeviceExt;
        let out = MaskBuffer::alloc(&self.ctx, acc.width, acc.height);
        let acc_view = acc
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let b_view = b
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let out_view = out
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mode_val = match mode {
            CompositeMode::Add => 0u32,
            CompositeMode::Subtract => 1u32,
            CompositeMode::Intersect => 2u32,
        };
        let ubuf = self
            .ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mask-fold-uniform"),
                contents: bytemuck::bytes_of(&FoldUniform {
                    mode: mode_val,
                    pad0: 0,
                    pad1: 0,
                    pad2: 0,
                }),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mask-fold"),
                layout: &self.fold_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&acc_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&b_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&out_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: ubuf.as_entire_binding(),
                    },
                ],
            });
        self.dispatch(&self.fold_pipeline, &bind, out.width, out.height);
        out
    }

    fn invert(&self, src: &MaskBuffer) -> MaskBuffer {
        let out = MaskBuffer::alloc(&self.ctx, src.width, src.height);
        let src_view = src
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let out_view = out
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mask-invert"),
                layout: &self.invert_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&out_view),
                    },
                ],
            });
        self.dispatch(&self.invert_pipeline, &bind, out.width, out.height);
        out
    }

    fn dispatch(&self, pipeline: &wgpu::ComputePipeline, bind: &wgpu::BindGroup, w: u32, h: u32) {
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mask-composite-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }
        self.ctx.queue.submit([enc.finish()]);
    }

    /// Fold `inputs` left-to-right (first seeds the accumulator), then invert if
    /// requested. Panics if `inputs` is empty (the zero-component case is a
    /// caller concern — see `composite_scalar`). All inputs must share dims.
    pub fn composite(&self, inputs: &[(MaskBuffer, CompositeMode)], invert: bool) -> MaskBuffer {
        assert!(!inputs.is_empty(), "composite requires >= 1 input buffer");
        let mut acc = inputs[0].0.clone();
        for (buf, mode) in &inputs[1..] {
            acc = self.fold_into(&acc, buf, *mode);
        }
        if invert {
            acc = self.invert(&acc);
        }
        acc
    }
}

/// A `Node<MaskBuffer>` that folds its graph-provided input buffers by `modes`
/// (modes[0] is the seed's ignored slot) + `invert`. Proves mask compositing
/// integrates into the unmodified `Graph<MaskBuffer>` executor (contract 4).
pub struct CompositeNode {
    pub pass: Rc<CompositePass>,
    pub modes: Vec<CompositeMode>,
    pub invert: bool,
}

impl Node<MaskBuffer> for CompositeNode {
    fn evaluate(&self, inputs: &[&MaskBuffer]) -> MaskBuffer {
        let pairs: Vec<(MaskBuffer, CompositeMode)> = inputs
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let mode = self.modes.get(i).copied().unwrap_or(CompositeMode::Add);
                ((*b).clone(), mode)
            })
            .collect();
        self.pass.composite(&pairs, self.invert)
    }
}
