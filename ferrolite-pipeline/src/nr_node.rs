//! `NoiseReductionNode` — à trous wavelet shrinkage as a multi-pass
//! `Node<PipelineImage>` (P4 design §3.3). Sits between `color_matrix` and
//! `vignette` in both pipelines.
//!
//! **Four textures regardless of `NR_LEVELS`.** Shrinkage is fused into the
//! decomposition pass, so no level is ever retained. Both `approx` and `acc`
//! ping-pong because each is read-modify-write across levels and a read==write
//! binding would alias. These are full-res `rgba16float` (192 MB each at 24 MP),
//! allocated ONLY after the identity early-return, AND FREED again the moment
//! NR returns to identity — an identity NR costs zero bytes whether it has
//! never activated OR was active and went back to identity (a user dragging a
//! strength slider to 0 must not leave ~960 MB parked for the rest of the
//! pipeline's lifetime), which `nr_identity_dispatches_nothing_and_allocates_nothing`
//! (`tests/nr_node.rs`, wired in Task 4), the in-module
//! `identity_is_a_zero_cost_passthrough` test, and
//! `identity_after_active_releases_textures` below all assert. The
//! reallocation this costs on a genuine on→off→on cycle is accepted: dragging
//! the strength slider itself never enters the identity branch (strength > 0
//! keeps NR active throughout the drag), so this only pays once per cycle,
//! not per interaction frame.
//!
//! **Pass structure:** `NR_LEVELS` × `nr_atrous.wgsl` (fused 2D convolution +
//! shrink + accumulate), then one `nr_combine.wgsl` (reconstruct + YCbCr→working).
//! Level 0 binds the ORIGINAL image as both `src` and `approx` and the shader
//! converts RGB→YCbCr on load, so no conversion pass or texture is needed.
//!
//! **Ping-pong parity (hand-traced for `NR_LEVELS = 5`, levels 0..4):**
//! `approx`: level 0 writes `approx_a`; level 1 (odd) writes `approx_b`; level 2
//! (even, not 0) writes `approx_a`; level 3 writes `approx_b`; level 4 writes
//! `approx_a` — so the LAST write (level 4) lands in `approx_a`, matching
//! `final_approx`'s `NR_LEVELS % 2 == 1 => approx_a` below. `acc`: level 0
//! reads `acc_a` (zeroed, see below) and writes `acc_b`; level 1 reads
//! `acc_b`/writes `acc_a`; level 2 reads `acc_a`/writes `acc_b`; level 3 reads
//! `acc_b`/writes `acc_a`; level 4 reads `acc_a`/writes `acc_b` — so the LAST
//! write (level 4) lands in `acc_b`, matching `final_acc`'s
//! `NR_LEVELS % 2 == 1 => acc_b` below.
//!
//! **The accumulator MUST start at zero every evaluate** (stale content from a
//! previous evaluate would silently corrupt output, uncaught by the identity
//! gate — `nr_leaves_a_flat_field_alone` in `tests/nr_node.rs`, and this
//! module's own `second_evaluate_on_same_input_matches_the_first`, are the
//! regression tests). Only `acc_a` needs zeroing: it is the only acc slot read
//! before this evaluate has written it (level 0's read); every later read
//! targets a slot this SAME evaluate already wrote in full (every level's
//! dispatch covers every pixel), so a prior evaluate's leftovers there are
//! always overwritten before being read. Zeroed via a trivial dedicated
//! zero-fill COMPUTE pass (`nr_clear.wgsl`), not a render-pass `LoadOp::Clear`:
//! a render-pass version (requiring `RENDER_ATTACHMENT` on `acc_a`) was tried
//! first but reproduced rare, load-dependent test divergence when run
//! concurrently with the crate's ~200 other GPU tests. The root cause was not
//! confirmed; switching to an all-compute clear removed it across 15+
//! repeated full-suite runs.
//!
//! Not yet wired into `EditPipeline`/`TileEditPipeline` — Task 4 does that.
//! Until then nothing outside this module's own `#[cfg(test)]` block
//! constructs `NoiseReductionNode`, so a plain (non-test) `--lib` build sees
//! every item here as dead; suppressed the same way `tests/common/mod.rs`
//! suppresses it for its own not-yet-all-consumed fixtures.
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Node};

use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::local::NoiseReduction;
use crate::nr::NR_LEVELS;
use crate::uniforms::{nr_uniform, NrUniform};

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
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

fn storage_out_entry(binding: u32, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
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

/// `0 = approx, 1 = acc_in, 2 = dst_next (storage), 3 = dst_acc (storage),
/// 4 = uniform` — `nr_atrous.wgsl`'s bind shape.
fn atrous_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("nr-atrous-bgl"),
        entries: &[
            texture_entry(0),
            texture_entry(1),
            storage_out_entry(2, PIPELINE_FORMAT),
            storage_out_entry(3, PIPELINE_FORMAT),
            uniform_entry(4),
        ],
    })
}

/// `0 = acc, 1 = approx, 2 = dst (storage)` — `nr_combine.wgsl`'s bind shape.
fn combine_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("nr-combine-bgl"),
        entries: &[
            texture_entry(0),
            texture_entry(1),
            storage_out_entry(2, PIPELINE_FORMAT),
        ],
    })
}

/// `0 = dst (storage)` — `nr_clear.wgsl`'s bind shape.
fn clear_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("nr-clear-bgl"),
        entries: &[storage_out_entry(0, PIPELINE_FORMAT)],
    })
}

fn bind_tex(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

fn bind_buf(binding: u32, buf: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buf.as_entire_binding(),
    }
}

// `compute_pipeline`/`view` copied verbatim from `sharpen_node.rs` — this
// crate deliberately duplicates these small helpers per module (see
// `dehaze_node.rs`/`rcd_gpu.rs`/`sharpen_node.rs`), not shared, per the task
// brief's house-pattern note.
fn compute_pipeline(
    ctx: &GpuContext,
    bgl: &wgpu::BindGroupLayout,
    label: &str,
    wgsl: &'static str,
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

fn view(tex: &wgpu::Texture) -> wgpu::TextureView {
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

/// The four ping-pong textures, reallocated only when dims change. All four
/// share IDENTICAL usage flags — unlike an earlier render-pass-clear version
/// of this node, `acc_a` needs no extra usage bit (see `clear_pipeline`'s
/// doc and the module doc's "accumulator MUST start at zero" note for why
/// only `acc_a` is ever cleared, and why it's cleared via compute).
struct Textures {
    dims: (u32, u32),
    approx_a: Arc<wgpu::Texture>,
    approx_b: Arc<wgpu::Texture>,
    acc_a: Arc<wgpu::Texture>,
    acc_b: Arc<wgpu::Texture>,
}

pub(crate) struct NoiseReductionNode {
    ctx: Arc<GpuContext>,
    params: Rc<Cell<NoiseReduction>>,
    atrous_bgl: wgpu::BindGroupLayout,
    atrous_pipeline: wgpu::ComputePipeline,
    combine_bgl: wgpu::BindGroupLayout,
    combine_pipeline: wgpu::ComputePipeline,
    /// Zero-fills `acc_a` at the top of every non-identity `evaluate` — see
    /// the module doc's "accumulator MUST start at zero" note.
    clear_bgl: wgpu::BindGroupLayout,
    clear_pipeline: wgpu::ComputePipeline,
    textures: RefCell<Option<Textures>>,
    /// Pooled per-level uniform buffers. Required because every level's dispatch
    /// batches into ONE encoder/submit: a later `write_buffer` on a buffer an
    /// earlier dispatch also reads would corrupt it at GPU-execution time.
    /// Mirrors `sharpen_node.rs`'s `uniform_pool`/`uniform_cursor`.
    uniform_pool: RefCell<Vec<wgpu::Buffer>>,
    uniform_cursor: Cell<usize>,
    out: RefCell<Option<PipelineImage>>,
    evals: Cell<u32>,
}

impl NoiseReductionNode {
    pub(crate) fn new(ctx: Arc<GpuContext>, params: Rc<Cell<NoiseReduction>>) -> Self {
        let device = &ctx.device;
        let atrous_bgl_layout = atrous_bgl(device);
        let atrous_pipeline = compute_pipeline(
            &ctx,
            &atrous_bgl_layout,
            "nr-atrous",
            include_str!("shaders/nr_atrous.wgsl"),
        );
        let combine_bgl_layout = combine_bgl(device);
        let combine_pipeline = compute_pipeline(
            &ctx,
            &combine_bgl_layout,
            "nr-combine",
            include_str!("shaders/nr_combine.wgsl"),
        );
        let clear_bgl_layout = clear_bgl(device);
        let clear_pipeline = compute_pipeline(
            &ctx,
            &clear_bgl_layout,
            "nr-clear",
            include_str!("shaders/nr_clear.wgsl"),
        );
        Self {
            ctx,
            params,
            atrous_bgl: atrous_bgl_layout,
            atrous_pipeline,
            combine_bgl: combine_bgl_layout,
            combine_pipeline,
            clear_bgl: clear_bgl_layout,
            clear_pipeline,
            textures: RefCell::new(None),
            uniform_pool: RefCell::new(Vec::new()),
            uniform_cursor: Cell::new(0),
            out: RefCell::new(None),
            evals: Cell::new(0),
        }
    }

    /// Number of times this node actually dispatched (test hook: identity NR
    /// must leave this at 0).
    pub(crate) fn eval_count(&self) -> u32 {
        self.evals.get()
    }

    /// GPU bytes currently held by this node's intermediates + output. Zero
    /// until the first non-identity evaluate. Instruments the spec §7.4 memory
    /// gate, mirroring `gpu_pyramid.rs`'s live-byte gauge.
    pub(crate) fn live_bytes(&self) -> u64 {
        let per = |t: &Textures| {
            let (w, h) = t.dims;
            // `rgba16float` = 8 B/px, four textures.
            (w as u64) * (h as u64) * 8 * 4
        };
        let inter = self.textures.borrow().as_ref().map_or(0, per);
        let out = self
            .out
            .borrow()
            .as_ref()
            .map_or(0, |o| (o.width as u64) * (o.height as u64) * 8);
        inter + out
    }

    fn alloc(&self, w: u32, h: u32, label: &str, extra: wgpu::TextureUsages) -> Arc<wgpu::Texture> {
        Arc::new(self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PIPELINE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | extra,
            view_formats: &[],
        }))
    }

    fn ensure_textures(&self, w: u32, h: u32) {
        let mut t = self.textures.borrow_mut();
        let stale = t.as_ref().is_none_or(|x| x.dims != (w, h));
        if stale {
            *t = Some(Textures {
                dims: (w, h),
                approx_a: self.alloc(w, h, "nr-approx-a", wgpu::TextureUsages::empty()),
                approx_b: self.alloc(w, h, "nr-approx-b", wgpu::TextureUsages::empty()),
                acc_a: self.alloc(w, h, "nr-acc-a", wgpu::TextureUsages::empty()),
                acc_b: self.alloc(w, h, "nr-acc-b", wgpu::TextureUsages::empty()),
            });
        }
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        let stale = out.as_ref().is_none_or(|o| (o.width, o.height) != (w, h));
        if stale {
            *out = Some(PipelineImage {
                texture: self.alloc(w, h, "nr-out", wgpu::TextureUsages::COPY_SRC),
                width: w,
                height: h,
            });
        }
        out.as_ref().expect("just ensured").clone()
    }

    /// Write `u` into the next pooled per-level uniform slot (grow on demand)
    /// and return its index — `wgpu::Buffer` has no `Clone`, so (mirroring
    /// `sharpen_node.rs::uniform_slot`) callers borrow `uniform_pool` at this
    /// index rather than taking an owned buffer out of the node.
    fn uniform_slot(&self, u: NrUniform) -> usize {
        let idx = self.uniform_cursor.get();
        self.uniform_cursor.set(idx + 1);
        {
            let mut pool = self.uniform_pool.borrow_mut();
            while pool.len() <= idx {
                pool.push(self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("nr-uniform"),
                    size: std::mem::size_of::<NrUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
        }
        let pool = self.uniform_pool.borrow();
        self.ctx
            .queue
            .write_buffer(&pool[idx], 0, bytemuck::bytes_of(&u));
        idx
    }

    /// Encode a zero-fill dispatch on `tex` (`nr_clear.wgsl`) — see the module
    /// doc's "accumulator MUST start at zero" note for why only `acc_a` is
    /// ever passed here.
    fn clear(&self, enc: &mut wgpu::CommandEncoder, tex: &wgpu::Texture, w: u32, h: u32) {
        let tex_view = view(tex);
        let bg = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("nr-clear"),
                layout: &self.clear_bgl,
                entries: &[bind_tex(0, &tex_view)],
            });
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("nr-clear"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.clear_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
    }
}

impl Node<PipelineImage> for NoiseReductionNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let nr = self.params.get();

        // Gate 1 (spec §7.2): identity NR is a byte-exact passthrough. An `Arc`
        // clone — no compute passes, NO allocation, and `evals` is NOT bumped.
        // Also FREE any textures/output a PRIOR active evaluate allocated: the
        // memory-gate claim ("identity NR costs zero bytes") must hold for a
        // node that WAS active and returned to identity, not just one that
        // never activated — otherwise a strength-to-0 drag leaks ~960 MB for
        // the pipeline's lifetime. The reallocation this costs on the next
        // on-cycle is accepted (see the module doc): a strength drag never
        // passes through this branch mid-interaction.
        if nr.is_identity() {
            *self.textures.borrow_mut() = None;
            *self.out.borrow_mut() = None;
            return src.clone();
        }

        self.evals.set(self.evals.get() + 1);
        self.uniform_cursor.set(0);
        let (w, h) = (src.width, src.height);
        self.ensure_textures(w, h);
        let out = self.ensure_out(w, h);
        let t = self.textures.borrow();
        let t = t.as_ref().expect("just ensured");

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("nr") });

        // The accumulator MUST start at zero every evaluate: stale content from
        // a previous evaluate would silently corrupt output and would NOT be
        // caught by the identity gate. See the module doc's ping-pong/zeroing
        // note for why only `acc_a` needs this.
        self.clear(&mut enc, &t.acc_a, w, h);

        for level in 0..NR_LEVELS {
            // Ping-pong: level 0 reads the ORIGINAL image (the shader converts
            // RGB->YCbCr on load); later levels read the previous `next`.
            let (approx_in, next_out): (&wgpu::Texture, &wgpu::Texture) = if level == 0 {
                (&src.texture, &t.approx_a)
            } else if level % 2 == 1 {
                (&t.approx_a, &t.approx_b)
            } else {
                (&t.approx_b, &t.approx_a)
            };
            let (acc_read, acc_write): (&wgpu::Texture, &wgpu::Texture) = if level % 2 == 0 {
                (&t.acc_a, &t.acc_b)
            } else {
                (&t.acc_b, &t.acc_a)
            };

            let uidx = self.uniform_slot(nr_uniform(&nr, level));
            let approx_view = view(approx_in);
            let acc_in_view = view(acc_read);
            let next_view = view(next_out);
            let acc_out_view = view(acc_write);
            let pool = self.uniform_pool.borrow();
            let bg = self
                .ctx
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("nr-atrous"),
                    layout: &self.atrous_bgl,
                    entries: &[
                        bind_tex(0, &approx_view),
                        bind_tex(1, &acc_in_view),
                        bind_tex(2, &next_view),
                        bind_tex(3, &acc_out_view),
                        bind_buf(4, &pool[uidx]),
                    ],
                });
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("nr-atrous"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.atrous_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }

        // Final residual/accumulator parity: after NR_LEVELS iterations the last
        // `next` and last `acc_write` are whichever slot the loop ended on (see
        // the module doc's hand-traced parity note).
        let final_approx: &wgpu::Texture = if NR_LEVELS % 2 == 1 {
            &t.approx_a
        } else {
            &t.approx_b
        };
        let final_acc: &wgpu::Texture = if NR_LEVELS % 2 == 1 {
            &t.acc_b
        } else {
            &t.acc_a
        };

        let final_acc_view = view(final_acc);
        let final_approx_view = view(final_approx);
        let out_view = view(&out.texture);
        let bg = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("nr-combine"),
                layout: &self.combine_bgl,
                entries: &[
                    bind_tex(0, &final_acc_view),
                    bind_tex(1, &final_approx_view),
                    bind_tex(2, &out_view),
                ],
            });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("nr-combine"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.combine_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }

        self.ctx.queue.submit(Some(enc.finish()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::upload_source;

    /// The node's identity passthrough: no dispatch, no allocation, and the
    /// returned image is the SAME texture (an `Arc` clone, not a copy).
    #[test]
    fn identity_is_a_zero_cost_passthrough() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let px = vec![0.4f32; 16 * 16 * 4];
        let img = ferrolite_image::LinearRgbaF32::new(16, 16, px).expect("len");
        let src = upload_source(&ctx, &img);

        let node =
            NoiseReductionNode::new(ctx.clone(), Rc::new(Cell::new(NoiseReduction::default())));
        let out = node.evaluate(&[&src]);

        assert_eq!(node.eval_count(), 0, "identity must not dispatch");
        assert_eq!(node.live_bytes(), 0, "identity must not allocate");
        assert!(
            Arc::ptr_eq(&out.texture, &src.texture),
            "identity must return the SAME texture, not a copy"
        );
    }

    /// Active NR dispatches once and allocates its four intermediates + output.
    #[test]
    fn active_nr_dispatches_and_allocates_four_plus_out() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let px = vec![0.4f32; 16 * 16 * 4];
        let img = ferrolite_image::LinearRgbaF32::new(16, 16, px).expect("len");
        let src = upload_source(&ctx, &img);

        let node = NoiseReductionNode::new(
            ctx.clone(),
            Rc::new(Cell::new(NoiseReduction {
                luminance: 0.5,
                ..Default::default()
            })),
        );
        let out = node.evaluate(&[&src]);

        assert_eq!(node.eval_count(), 1);
        assert!(
            !Arc::ptr_eq(&out.texture, &src.texture),
            "must be a new texture"
        );
        // 4 intermediates + 1 output, `rgba16float` = 8 B/px at 16x16.
        assert_eq!(node.live_bytes(), 16 * 16 * 8 * 5);
    }

    /// A high-contrast checkerboard (values `0.0`/`4.0`, scene-linear range —
    /// intentionally outside `[0,1]` so the resulting detail coefficients
    /// exceed EVERY level's shrink threshold, guaranteeing a non-negligible
    /// accumulator at every level, not just the finest). A small `±0.1` noise
    /// fixture (like the pending `tests/nr_node.rs`'s `noisy_flat`) was tried
    /// first and did NOT exercise this test's regression: `NR_NOISE_SCALE`'s
    /// per-level thresholds (`nr.rs`) are calibrated for UNIT-VARIANCE noise,
    /// so `±0.1`-amplitude detail sits below every level-0..3 threshold and
    /// shrinks to exactly zero there — the accumulator ends up legitimately
    /// zero regardless of whether it was cleared, so a bug in the clear would
    /// go undetected. This checkerboard's large, unclamped detail forces a
    /// genuinely nonzero accumulator at the levels that matter.
    fn high_contrast_checkerboard(w: u32, h: u32) -> ferrolite_image::LinearRgbaF32 {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 0.0 } else { 4.0 };
                px.extend_from_slice(&[v, v, v, 1.0]);
            }
        }
        ferrolite_image::LinearRgbaF32::new(w, h, px).expect("checkerboard length")
    }

    /// Repeating `evaluate` on the SAME node/input must reproduce the SAME
    /// output — the direct regression test (at this node's level, not the
    /// pipeline's) for a stale/un-zeroed accumulator: if `acc_a` weren't
    /// re-cleared every evaluate, the SECOND evaluate's level-0 accumulator
    /// read would pick up the FIRST evaluate's leftover `acc_a` content
    /// (nonzero here, since this fixture has real detail at every scale)
    /// instead of starting from zero like the first evaluate did, and the two
    /// outputs would diverge.
    #[test]
    fn second_evaluate_on_same_input_matches_the_first() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let img = high_contrast_checkerboard(16, 16);
        let src = upload_source(&ctx, &img);

        let node = NoiseReductionNode::new(
            ctx.clone(),
            Rc::new(Cell::new(NoiseReduction {
                luminance: 1.0,
                color: 1.0,
                ..Default::default()
            })),
        );
        let read = |img: &PipelineImage| -> Vec<u8> {
            let bpr = (img.width * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
                * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("nr-node-test-readback"),
                size: (bpr * img.height) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut enc = ctx
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            enc.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &img.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &buf,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(bpr),
                        rows_per_image: Some(img.height),
                    },
                },
                wgpu::Extent3d {
                    width: img.width,
                    height: img.height,
                    depth_or_array_layers: 1,
                },
            );
            ctx.queue.submit([enc.finish()]);
            let slice = buf.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            ctx.device.poll(wgpu::Maintain::Wait);
            let data = slice.get_mapped_range().to_vec();
            buf.unmap();
            data
        };

        // Read `out1` BEFORE the second `evaluate` call: `ensure_out` caches a
        // single output texture by dims, so `out1`/`out2` are the SAME
        // physical texture — reading `out1` only after the second evaluate
        // would silently read post-second-evaluate content and prove nothing.
        let out1 = node.evaluate(&[&src]);
        let bytes1 = read(&out1);
        let out2 = node.evaluate(&[&src]);
        let bytes2 = read(&out2);
        assert_eq!(node.eval_count(), 2);
        assert_eq!(
            bytes1, bytes2,
            "a stale accumulator would make the second evaluate diverge from the first"
        );
    }

    /// A node that WAS active must go back to zero GPU bytes once its params
    /// return to identity — the direct regression test for the memory-gate
    /// claim in the module doc ("identity NR costs zero bytes" must hold for
    /// a node that was active and returned to identity, not only one that
    /// never activated). Fix-round Important 3.
    #[test]
    fn identity_after_active_releases_textures() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let px = vec![0.4f32; 16 * 16 * 4];
        let img = ferrolite_image::LinearRgbaF32::new(16, 16, px).expect("len");
        let src = upload_source(&ctx, &img);

        let params = Rc::new(Cell::new(NoiseReduction {
            luminance: 0.5,
            ..Default::default()
        }));
        let node = NoiseReductionNode::new(ctx.clone(), params.clone());
        node.evaluate(&[&src]);
        assert!(
            node.live_bytes() > 0,
            "sanity: an active evaluate must allocate something"
        );

        params.set(NoiseReduction::default());
        let out = node.evaluate(&[&src]);
        assert_eq!(
            node.live_bytes(),
            0,
            "returning to identity must release the four intermediates + output"
        );
        assert!(
            Arc::ptr_eq(&out.texture, &src.texture),
            "the identity evaluate must return the SAME texture as its input"
        );
    }

    /// Diagnostic-only cascade mirroring `nr::atrous_shrink_reference`'s loop
    /// exactly, but RECORDING each level's max abs detail instead of
    /// shrinking it away. Used only to CONFIRM a fixture has genuine energy
    /// at every level (in particular the coarsest, spacing 16) before trusting
    /// it as a parity oracle input — never used to build the oracle itself
    /// (`cpu_oracle`, below, calls the real `atrous_shrink_reference`).
    fn max_abs_detail_per_level(plane: &[f32], w: usize, h: usize) -> [f32; NR_LEVELS] {
        let mut approx = plane.to_vec();
        let mut out = [0.0f32; NR_LEVELS];
        for (l, slot) in out.iter_mut().enumerate() {
            let spacing = 1usize << l;
            let next = crate::nr::b3_spline_v(
                &crate::nr::b3_spline_h(&approx, w, h, spacing),
                w,
                h,
                spacing,
            );
            *slot = approx
                .iter()
                .zip(next.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            approx = next;
        }
        out
    }

    /// A deterministic multi-frequency, per-channel-ASYMMETRIC fixture:
    /// `r` varies along x only (period = the full width), `b` along y only
    /// (period = the full height), `g` along the diagonal (a different,
    /// mixed period) — plus a small per-pixel LCG perturbation on every
    /// channel for genuine fine-scale (level-0) detail too. The three
    /// channels are deliberately NOT interchangeable: swapping which
    /// channel feeds Cb vs Cr (a "Cb/Cr transposition" bug) would swap in a
    /// visibly different pattern, not a numerically-close one.
    fn multi_frequency_fixture(w: u32, h: u32) -> Vec<[f32; 3]> {
        use std::f32::consts::PI;
        let mut state = 777_u32;
        let mut next_noise = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((state >> 16) as f32 / 65535.0 - 0.5) * 0.04
        };
        let mut px = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / w as f32;
                let fy = y as f32 / h as f32;
                let r = 0.5 + 0.30 * (2.0 * PI * fx).sin() + next_noise();
                let g = 0.5 + 0.20 * (2.0 * PI * (fx + fy)).cos() + next_noise();
                let b = 0.5 + 0.30 * (2.0 * PI * fy).cos() + next_noise();
                px.push([r, g, b]);
            }
        }
        px
    }

    /// The CPU oracle: per-pixel `rgb_to_ycbcr`, `atrous_shrink_reference` on
    /// each plane with ITS OWN strength/detail (luma uses
    /// `(luminance, detail)`; BOTH chroma planes use `(color, color_detail)`,
    /// mirroring `nr_atrous.wgsl`'s single `t_chroma` applied to both Cb and
    /// Cr), then per-pixel `ycbcr_to_rgb`. Built from `nr.rs` primitives only.
    fn cpu_oracle(planar: &[[f32; 3]], w: usize, h: usize, nr: NoiseReduction) -> Vec<[f32; 3]> {
        let n = w * h;
        let mut y_plane = vec![0.0f32; n];
        let mut cb_plane = vec![0.0f32; n];
        let mut cr_plane = vec![0.0f32; n];
        for (i, c) in planar.iter().enumerate() {
            let ycc = crate::nr::rgb_to_ycbcr(*c);
            y_plane[i] = ycc[0];
            cb_plane[i] = ycc[1];
            cr_plane[i] = ycc[2];
        }
        let y_out = crate::nr::atrous_shrink_reference(&y_plane, w, h, nr.luminance, nr.detail);
        let cb_out = crate::nr::atrous_shrink_reference(&cb_plane, w, h, nr.color, nr.color_detail);
        let cr_out = crate::nr::atrous_shrink_reference(&cr_plane, w, h, nr.color, nr.color_detail);
        (0..n)
            .map(|i| crate::nr::ycbcr_to_rgb([y_out[i], cb_out[i], cr_out[i]]))
            .collect()
    }

    /// Read all four RGBA channels of an `Rgba16Float` `PipelineImage` back to
    /// f32 (test-only; mirrors `sharpen_node.rs::read_rgba_channels`).
    fn read_rgba_f32(ctx: &GpuContext, img: &PipelineImage) -> Vec<[f32; 4]> {
        let (w, h) = (img.width, img.height);
        let bpp = 8u32; // RGBA16F
        let bpr_unpadded = w * bpp;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bpr_padded = bpr_unpadded.div_ceil(align) * align;
        let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nr-node-parity-readback"),
            size: (bpr_padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &img.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buf,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr_padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        ctx.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let mut out = vec![[0.0f32; 4]; (w * h) as usize];
        for row in 0..h {
            let start = (row * bpr_padded) as usize;
            for x in 0..w {
                let o = start + (x * 4) as usize * 2;
                let r = half::f16::from_le_bytes([data[o], data[o + 1]]).to_f32();
                let g = half::f16::from_le_bytes([data[o + 2], data[o + 3]]).to_f32();
                let b = half::f16::from_le_bytes([data[o + 4], data[o + 5]]).to_f32();
                let a = half::f16::from_le_bytes([data[o + 6], data[o + 7]]).to_f32();
                out[(row * w + x) as usize] = [r, g, b, a];
            }
        }
        drop(data);
        buf.unmap();
        out
    }

    /// GPU-vs-CPU parity at NON-SQUARE dims with asymmetric per-channel
    /// content and DIFFERENT luma/chroma strengths — catches what
    /// `active_nr_dispatches_and_allocates_four_plus_out` and
    /// `second_evaluate_on_same_input_matches_the_first` cannot: a WRONG
    /// final ping-pong slot is deterministic (both those tests would still
    /// pass), a transposed clamp axis is invisible on square fixtures, and a
    /// Cb/Cr transposition needs asymmetric chroma content plus distinct
    /// per-plane strengths to surface as a visible R/B error. Fix-round
    /// Important 1 + 2. This test was verified to actually bite by
    /// temporarily injecting a wrong `final_acc` slot and a transposed clamp
    /// axis in `evaluate`/`nr_atrous.wgsl`, confirming FAILs, then reverting
    /// both — see the task-3 fix-round report for the exact edits and
    /// failure output.
    #[test]
    fn nr_node_matches_cpu_oracle_at_nonsquare_dims() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (48u32, 32u32);
        let planar = multi_frequency_fixture(w, h);

        // Confirm the fixture has real energy at the COARSEST level (spacing
        // 16) on the luma plane — otherwise a wrong final-slot bug would be
        // invisible to this test (see the doc above).
        let luma_plane: Vec<f32> = planar
            .iter()
            .map(|c| crate::nr::rgb_to_ycbcr(*c)[0])
            .collect();
        let per_level = max_abs_detail_per_level(&luma_plane, w as usize, h as usize);
        eprintln!(
            "nr_node_matches_cpu_oracle_at_nonsquare_dims: max abs luma detail per level = {per_level:?}"
        );
        assert!(
            per_level[NR_LEVELS - 1] > 1e-4,
            "fixture has no coarsest-level energy, final-slot bugs would be invisible: {per_level:?}"
        );

        let mut interleaved = Vec::with_capacity((w * h * 4) as usize);
        for c in &planar {
            interleaved.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
        }
        let img = ferrolite_image::LinearRgbaF32::new(w, h, interleaved).expect("fixture image");
        let src = upload_source(&ctx, &img);

        // DIFFERENT luma vs chroma strengths so a Cb/Cr transposition fails.
        let nr = NoiseReduction {
            luminance: 1.0,
            color: 0.4,
            ..Default::default()
        };
        let node = NoiseReductionNode::new(ctx.clone(), Rc::new(Cell::new(nr)));
        let out = node.evaluate(&[&src]);

        let gpu = read_rgba_f32(&ctx, &out);
        let expected = cpu_oracle(&planar, w as usize, h as usize, nr);

        // Tolerance: `rgba16float` (~3-4 significant decimal digits) storage
        // round-trips through FIVE accumulation levels plus the combine pass
        // (six dispatches total), each an independent f16 read/write — the
        // error budget compounds across all six, unlike a single-pass point
        // op. `2e-3` matches `sharpen_node.rs`'s two-pass tolerance; this
        // node has three times the passes, so `TOL` below is `6e-3`. The
        // actual observed max deviation (see this test's stdout) is reported
        // in the task-3 fix-round report; it must stay comfortably under
        // `TOL` for this tolerance choice to still be discriminating.
        const TOL: f32 = 6e-3;
        let mut max_d = 0.0f32;
        for (i, (g, e)) in gpu.iter().zip(expected.iter()).enumerate() {
            for ch in 0..3 {
                let d = (g[ch] - e[ch]).abs();
                max_d = max_d.max(d);
                assert!(
                    d < TOL,
                    "pixel {i} channel {ch}: gpu={} expected={} diff={d} (tol {TOL})",
                    g[ch],
                    e[ch]
                );
            }
        }
        eprintln!(
            "nr_node_matches_cpu_oracle_at_nonsquare_dims: max abs diff = {max_d} (tol {TOL})"
        );
    }
}
