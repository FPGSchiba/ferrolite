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

    /// Record (but do not submit) one fold step: `out = fold(acc, b, mode)`.
    /// `acc`, `b`, and `out` must never alias the same texture (ping-pong
    /// scratch buffers guarantee this) — `acc`/`b` are read-only inputs here,
    /// including cached component coverage buffers, which this must never
    /// mutate.
    fn record_fold(
        &self,
        enc: &mut wgpu::CommandEncoder,
        acc: &wgpu::Texture,
        b: &wgpu::Texture,
        out: &MaskBuffer,
        mode: CompositeMode,
    ) {
        use wgpu::util::DeviceExt;
        let acc_view = acc.create_view(&wgpu::TextureViewDescriptor::default());
        let b_view = b.create_view(&wgpu::TextureViewDescriptor::default());
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
        Self::record_pass(enc, &self.fold_pipeline, &bind, out.width, out.height);
    }

    /// Record (but do not submit) an invert step: `out = 1 - src`. `src` and
    /// `out` must never alias the same texture.
    fn record_invert(&self, enc: &mut wgpu::CommandEncoder, src: &wgpu::Texture, out: &MaskBuffer) {
        let src_view = src.create_view(&wgpu::TextureViewDescriptor::default());
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
        Self::record_pass(enc, &self.invert_pipeline, &bind, out.width, out.height);
    }

    /// Record a single compute pass (bind pipeline + bind group, dispatch)
    /// into `enc`. Does not submit — wgpu inserts automatic memory barriers
    /// between separate compute passes within one encoder, so each ping-pong
    /// step correctly observes the previous step's writes.
    fn record_pass(
        enc: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        bind: &wgpu::BindGroup,
        w: u32,
        h: u32,
    ) {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("mask-composite-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
    }

    /// Fold `inputs` left-to-right (first seeds the accumulator), then invert if
    /// requested. Panics if `inputs` is empty (the zero-component case is a
    /// caller concern — see `composite_scalar`). All inputs must share dims.
    ///
    /// Batches the whole fold chain (+ optional invert) into ONE command
    /// encoder and ONE `queue.submit`, ping-ponging between two scratch
    /// buffers. The input buffers (including `inputs[0]`, the seed, which may
    /// be a cached component coverage buffer) are only ever read — every
    /// write targets one of the two freshly allocated scratch buffers.
    pub fn composite(&self, inputs: &[(MaskBuffer, CompositeMode)], invert: bool) -> MaskBuffer {
        assert!(!inputs.is_empty(), "composite requires >= 1 input buffer");
        let (w, h) = (inputs[0].0.width, inputs[0].0.height);

        // Single input, no invert: nothing to compute — hand back the seed.
        if inputs.len() == 1 && !invert {
            return inputs[0].0.clone();
        }

        // Two scratch buffers; ping-pong so read-tex != write-tex each step (and
        // the cached input buffers are never written). Only 2 allocs regardless
        // of N.
        let scratch = [
            MaskBuffer::alloc(&self.ctx, w, h),
            MaskBuffer::alloc(&self.ctx, w, h),
        ];
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mask-composite"),
            });

        // Fold: step k reads the previous accumulator, writes the other scratch.
        // acc for step 1 is the seed (inputs[0]); afterwards it alternates
        // scratch.
        let mut acc_tex = &inputs[0].0.texture;
        let mut cur = 0usize;
        for (buf, mode) in &inputs[1..] {
            self.record_fold(&mut enc, acc_tex, &buf.texture, &scratch[cur], *mode);
            acc_tex = &scratch[cur].texture;
            cur ^= 1;
        }
        // `cur` now points at the free scratch; the last write went to
        // scratch[cur ^ 1].
        let mut result_idx = cur ^ 1;

        if invert {
            // Read the last accumulator, write the free scratch.
            let src_tex = if inputs.len() == 1 {
                &inputs[0].0.texture
            } else {
                &scratch[result_idx].texture
            };
            self.record_invert(&mut enc, src_tex, &scratch[cur]);
            result_idx = cur;
        }

        self.ctx.queue.submit([enc.finish()]);
        scratch[result_idx].clone()
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

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_gpu::GpuContext;

    fn const_buf(ctx: &Arc<GpuContext>, w: u32, h: u32, v: f32) -> MaskBuffer {
        let buf = MaskBuffer::alloc(ctx, w, h);
        let data = vec![v; (w * h) as usize];
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &buf.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&data),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        buf
    }

    #[test]
    fn batched_composite_matches_scalar_reference_three_inputs() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let pass = CompositePass::new(ctx.clone());
        // 2x2 constant buffers: 0.8 (seed, Add), 0.5 (Subtract), 0.3 (Intersect)
        let a = const_buf(&ctx, 2, 2, 0.8);
        let b = const_buf(&ctx, 2, 2, 0.5);
        let c = const_buf(&ctx, 2, 2, 0.3);
        let out = pass.composite(
            &[
                (a, crate::model::CompositeMode::Add),
                (b, crate::model::CompositeMode::Subtract),
                (c, crate::model::CompositeMode::Intersect),
            ],
            false,
        );
        // scalar reference: intersect(subtract(0.8, 0.5), 0.3)
        let want = crate::model::composite_scalar(
            &[
                (0.8, crate::model::CompositeMode::Add),
                (0.5, crate::model::CompositeMode::Subtract),
                (0.3, crate::model::CompositeMode::Intersect),
            ],
            false,
        );
        let got = crate::compositor::read_mask_r32f(&ctx, &out);
        assert!(
            got.iter().all(|&v| (v - want).abs() < 1e-4),
            "got {:?} want {want}",
            &got[..1]
        );
    }
}
