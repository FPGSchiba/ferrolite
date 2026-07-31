//! `SharpenNode` — the unsharp-mask sharpen as a separable, multi-pass
//! `Node<PipelineImage>`. Replaces the fused `sharpen.wgsl` `PointOpNode`
//! (O((2r+1)^2) box blur) with an O(2r+1) two-pass box blur (H then V) per
//! DISTINCT radius, plus a cheap apply pass per active dispatch, at the SAME
//! graph position/inputs in both `EditPipeline` and `TileEditPipeline`
//! (`sharpen.wgsl` stays in-tree as reference math — see that file's doc).
//!
//! **Mathematical identity this depends on:** a clamped-edge box mean is
//! separable — computing the horizontal mean (radius `r`, x clamped to
//! `[0, w-1]`) and then the vertical mean of THAT (radius `r`, y clamped to
//! `[0, h-1]`), each normalized by `2r+1`, equals the fused 2D box mean
//! (normalized by `(2r+1)^2`) to float-order precision. This holds because
//! `clamp` DUPLICATES the border texel rather than skipping it — every
//! output pixel's fused-2D window always has exactly `(2r+1)^2` taps (some
//! repeated at the border), and expanding the two 1D sums shows the H-then-V
//! composition visits the same multiset of source texels with the same
//! weights, just summed in a different (but float-associative-safe within
//! 1e-6 for well-conditioned inputs) order. `separable_box_equals_2d_box`
//! (below) proves this on a CPU reference before the GPU passes ever ran.
//!
//! **Phase 4 Task 4 (per-mask sharpen):** after the (optional) GLOBAL apply
//! (`c + a_g·(c − blur_rg)`, byte-identical to Task 1 — same shader, same
//! bind group), every VISIBLE mask layer with its OWN
//! `adjustments.sharpen.amount != 0.0` adds `m_i·a_i·(c_in − blur_ri)` to the
//! running accumulator, where `c_in` is THIS NODE'S INPUT (`src`, the
//! pre-sharpen image — the SAME `src` the global pass reads), never the
//! running accumulator. This additive-unsharp formulation is
//! ORDER-INDEPENDENT across layers (each layer's contribution is a fixed-base
//! delta term; summing fixed-base deltas is commutative) — layers are
//! applied in stack order purely for determinism, not because order matters
//! mathematically. One separable box blur is computed per DISTINCT radius
//! across the whole evaluate (global + every active layer), not per
//! dispatch — see `encode_blur`'s callers in `evaluate`. The masks
//! themselves are NOT recomposited here: they are read from the Color-stage
//! engine's `SharedMasks` handle (`local_node.rs`), shared via the SAME
//! `Rc<RefCell<LocalAdjustments>>` for looking up each layer's own sharpen
//! fields by index — see that type's freshness-invariant doc for why reading
//! it here is always correct.
//!
//! **Pass structure (per DISTINCT radius, in `encode_blur`):**
//!   1. `sharpen_box_h.wgsl` — src (rgba16float) -> `h_blur` (rgba16float),
//!      horizontal box mean, radius `r`.
//!   2. `sharpen_box_v.wgsl` — `h_blur` -> `blur` (rgba16float), vertical box
//!      mean of the H pass's output, same radius.
//!
//! Then, per active dispatch (the global op, then each active layer in stack
//! order):
//!   - Global: `sharpen_apply.wgsl` — `out = src + amount*(src - blur)`,
//!     clamped non-negative, alpha passed through — reads both `src` and
//!     `blur` (unchanged from Task 1).
//!   - Per-layer: `sharpen_apply_masked.wgsl` — `out = accum +
//!     mask*amount*(src - blur)`, clamped non-negative — reads the running
//!     `accum`, the ORIGINAL `src`, this layer's radius's `blur`, and this
//!     layer's own composited `mask`.
//!
//! **Phase 4 Task 5 (Detail + Masking, GLOBAL dispatch only):** when the
//! global op's `detail != 0.0 || masking != 0.0`, the global apply dispatch
//! routes through `sharpen_apply_detail.wgsl` instead of `sharpen_apply.wgsl`
//! — `out = src + amount*edge*mix(src-blur_r, src-blur_fine, detail)` (design
//! §4.3). `blur_fine` is the SAME separable-blur machinery at radius
//! `max(1, r/3)`, requested as just another DISTINCT radius — computed only
//! when `detail != 0.0`; when `masking != 0.0` alone, the MAIN blur is bound
//! to the `blur_fine` slot too (so `mix(..., 0.0)` discards it at zero cost,
//! no extra blur dispatched). When BOTH are `0.0`, the node dispatches the
//! OLD `sharpen_apply.wgsl` with the IDENTICAL bind group as before this
//! task — not merely an equivalent formula through the new shader — so every
//! pre-P4 parity golden stays byte-exact. Per-mask-layer sharpen
//! (`sharpen_apply_masked.wgsl`) is UNCHANGED by this task: a layer's own
//! `detail`/`masking` fields exist on `Sharpen` (so they round-trip through
//! `AdjustmentSet` without extra plumbing) but are not yet consumed by the
//! masked-apply dispatch.
//!
//! **Identity passthrough:** when NEITHER the global op (`amount == 0 ||
//! radius <= 0`) NOR any visible layer has an active sharpen, `evaluate`
//! returns `src.clone()` (a cheap `Arc` clone of `PipelineImage`, mirroring
//! `DehazeTransmissionNode`'s early-return pattern) WITHOUT dispatching
//! anything — byte-identical to the old fused shader's in-shader identity
//! branch for the global-only case, and zero extra cost for identity layers.
//!
//! **Dims-keyed intermediates + pooled uniforms:** `blur_slots` (one
//! `Intermediates` per DISTINCT radius this evaluate needs, reallocated only
//! on a `(w, h)` change, reused by POSITION across evaluates — mirrors
//! `dehaze_node.rs::Intermediates`'s `ensure_*` pattern) and `uniform_pool`
//! (one buffer per dispatch this evaluate, grown on demand — mirrors
//! `local_node.rs`'s `apply_bufs`/`apply_buf_cursor` pool, required because
//! every dispatch this evaluate needs is batched into ONE command
//! encoder/submit, so a later `write_buffer` on a buffer an earlier dispatch
//! also reads would corrupt it at GPU-execution time).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Node};
use ferrolite_mask::MaskBuffer;

use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::local::LocalAdjustments;
use crate::local_node::SharedMasks;
use crate::uniforms::{SharpenUniform, MAX_SHARPEN_RADIUS};

/// Intermediate plane format for the H/V blur passes: the same
/// `PIPELINE_FORMAT` (`rgba16float`) every other node's textures use, NOT
/// full-precision `Rgba32Float`. This was tried (see the Task 1 report) and
/// reverted: full-precision intermediates roughly DOUBLE this node's memory
/// traffic (two full-res `Rgba32Float` planes vs `Rgba16Float`), which measured
/// as a same-session A/B regression from ~56ms to ~92-104ms on case (a) of
/// `engine_bench` — the exact opposite of this task's purpose (cutting sharpen
/// time via O(r) taps). `rgba16float` intermediates DO measurably widen the
/// `full_global` parity golden's drift (~7.9e-3, vs ~4.0e-3 at full precision)
/// — an accepted, documented precision cost of the perf win (see the Task 1
/// report's "Parity" section for the root-cause diagnosis: the drift scales
/// with local pixel variance, proven via synthetic GPU-vs-GPU comparisons, not
/// an edge-handling bug — `separable_box_equals_2d_box` below proves the
/// underlying math is exact to 1e-6 in full CPU f32 precision).
const BLUR_FORMAT: wgpu::TextureFormat = PIPELINE_FORMAT;

/// Intermediate planes (see `BLUR_FORMAT`), keyed on `(w, h)` and reallocated
/// together when the input dims change (mirrors
/// `dehaze_node.rs::Intermediates`). One of these lives per DISTINCT radius
/// this evaluate needs (see `SharpenNode::blur_slots`), not per dispatch.
struct Intermediates {
    dims: (u32, u32),
    /// `sharpen_box_h.wgsl`'s output: horizontal box mean of `src`.
    h_blur: wgpu::Texture,
    /// `sharpen_box_v.wgsl`'s output: vertical box mean of `h_blur` — the
    /// final separable box blur the apply passes read.
    blur: wgpu::Texture,
}

fn alloc_plane(ctx: &GpuContext, w: u32, h: u32, label: &str) -> wgpu::Texture {
    ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: BLUR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    })
}

impl Intermediates {
    fn new(ctx: &GpuContext, w: u32, h: u32) -> Self {
        Self {
            dims: (w, h),
            h_blur: alloc_plane(ctx, w, h, "sharpen-h-blur"),
            blur: alloc_plane(ctx, w, h, "sharpen-blur"),
        }
    }
}

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

/// Shared by both blur passes: `0 = src texture, 1 = storage-write dst, 2 =
/// uniform` — the same shape `sharpen_box_h.wgsl`/`sharpen_box_v.wgsl` bind,
/// so one bind-group layout and one pair of pipelines cover both dispatches
/// (mirrors `dehaze_node.rs::plane_bgl` being reused for min/box passes).
fn blur_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sharpen-blur-bgl"),
        entries: &[
            texture_entry(0),
            storage_out_entry(1, BLUR_FORMAT),
            uniform_entry(2),
        ],
    })
}

/// `0 = src, 1 = blur, 2 = storage-write dst, 3 = uniform` — the GLOBAL apply
/// pass's bind shape (unchanged from Task 1).
fn apply_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sharpen-apply-bgl"),
        entries: &[
            texture_entry(0),
            texture_entry(1),
            storage_out_entry(2, PIPELINE_FORMAT),
            uniform_entry(3),
        ],
    })
}

/// `0 = src, 1 = blur, 2 = blur_fine, 3 = storage-write dst, 4 = uniform` —
/// the Phase 4 Task 5 Detail/Masking-aware GLOBAL apply pass's bind shape
/// (`sharpen_apply_detail.wgsl`). Only ever bound for the GLOBAL dispatch
/// (see the module doc); per-mask-layer sharpen keeps using `masked_apply_bgl`.
fn apply_detail_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sharpen-apply-detail-bgl"),
        entries: &[
            texture_entry(0),
            texture_entry(1),
            texture_entry(2),
            storage_out_entry(3, PIPELINE_FORMAT),
            uniform_entry(4),
        ],
    })
}

/// `0 = accum (running total), 1 = orig_src (this node's ORIGINAL input), 2 =
/// this dispatch's blur, 3 = mask (R32Float, non-filterable — mirrors
/// `local_adjust.wgsl`'s mask binding), 4 = storage-write dst, 5 = uniform` —
/// the Phase 4 Task 4 per-mask-layer masked apply pass's bind shape
/// (`sharpen_apply_masked.wgsl`).
fn masked_apply_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sharpen-apply-masked-bgl"),
        entries: &[
            texture_entry(0),
            texture_entry(1),
            texture_entry(2),
            texture_entry(3),
            storage_out_entry(4, PIPELINE_FORMAT),
            uniform_entry(5),
        ],
    })
}

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

fn dispatch(
    enc: &mut wgpu::CommandEncoder,
    label: &str,
    pipeline: &wgpu::ComputePipeline,
    bind: &wgpu::BindGroup,
    w: u32,
    h: u32,
) {
    let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some(label),
        timestamp_writes: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind, &[]);
    pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
}

/// The separable sharpen: a blur pass pair per distinct radius, an apply pass
/// per active dispatch (global + per active mask layer). Pipelines/layouts
/// built once; intermediates/uniforms/output pooled and reallocated only as
/// this evaluate's dispatch count or the input dims change.
pub(crate) struct SharpenNode {
    ctx: Arc<GpuContext>,
    params: Rc<Cell<SharpenUniform>>,
    // Phase 4 Task 4: shared with the Color-stage engine node's own
    // `local_layers` (the SAME `Rc`, always current) — read for each visible
    // layer's OWN `adjustments.sharpen` (amount/radius), keyed by the raw
    // stack index `shared_masks` carries alongside each composited buffer.
    layers: Rc<RefCell<LocalAdjustments>>,
    // Phase 4 Task 4: the Color-stage engine's composited visible-layer masks
    // (see `SharedMasks`'s freshness invariant doc, `local_node.rs`) — this
    // node only ever READS it, never clears/writes it.
    shared_masks: Rc<RefCell<SharedMasks>>,

    // Pooled per-dispatch uniform buffers: one per (distinct-radius blur pair
    // OR apply dispatch) this evaluate needs, grown on demand, cursor reset at
    // the top of `evaluate`. Required because every dispatch this evaluate
    // needs is batched into ONE command encoder/submit — a later
    // `write_buffer` on a buffer an EARLIER dispatch also reads would
    // corrupt it at GPU-execution time (the GPU only observes a buffer's
    // content as of submit time, not as of each individual `write_buffer`
    // call) — mirrors `local_node.rs`'s `apply_bufs`/`apply_buf_cursor`.
    uniform_pool: RefCell<Vec<wgpu::Buffer>>,
    uniform_cursor: Cell<usize>,

    blur_bgl: wgpu::BindGroupLayout,
    h_pipeline: wgpu::ComputePipeline,
    v_pipeline: wgpu::ComputePipeline,

    apply_bgl: wgpu::BindGroupLayout,
    apply_pipeline: wgpu::ComputePipeline,

    // Phase 4 Task 5: Detail/Masking-aware GLOBAL apply pass, dispatched
    // instead of `apply_pipeline` only when the global op's `detail` and
    // `masking` aren't both zero (see the module doc's gate-2 note).
    apply_detail_bgl: wgpu::BindGroupLayout,
    apply_detail_pipeline: wgpu::ComputePipeline,

    masked_apply_bgl: wgpu::BindGroupLayout,
    masked_apply_pipeline: wgpu::ComputePipeline,

    // One `Intermediates` per DISTINCT radius this evaluate needs, reused by
    // POSITION across evaluates (a later evaluate with fewer distinct radii
    // uses a prefix of the pool; extra trailing slots stay allocated but
    // unused — bounded by the largest distinct-radii count any past evaluate
    // needed, never unbounded since that count is capped by 1 (global) +
    // visible-layer count).
    blur_slots: RefCell<Vec<Intermediates>>,
    // A/B ping-pong output textures (mirrors `local_node.rs`'s `apply_out`):
    // required because within one evaluate the accumulator chains through
    // multiple apply dispatches (`current = apply(&current, ...)`), and a
    // dispatch's read (accum) and write (dst) texture must never be the same
    // resource (wgpu validation panics on that usage conflict in one bind
    // group). `ensure_out` always picks whichever of A/B is NOT the current
    // accumulator (by `Arc::ptr_eq`), so read != write on every dispatch
    // regardless of how many layers are active.
    out: RefCell<Option<[PipelineImage; 2]>>,

    // Test hook: cumulative count of distinct-radius blur pairs computed
    // (proves "one blur per DISTINCT radius", not per dispatch).
    blurs: Cell<u32>,
    // Test hook: cumulative count of `evaluate` calls (mirrors
    // `LocalAdjustmentsNode::evals` — proves the graph's dirty-tracking
    // actually re-runs this node when expected, e.g. a mask-layer
    // sharpen-amount-only change).
    evals: Cell<u32>,
}

impl SharpenNode {
    pub(crate) fn new(
        ctx: Arc<GpuContext>,
        params: Rc<Cell<SharpenUniform>>,
        layers: Rc<RefCell<LocalAdjustments>>,
        shared_masks: Rc<RefCell<SharedMasks>>,
    ) -> Self {
        let device = &ctx.device;

        let blur_bgl_layout = blur_bgl(device);
        let h_pipeline = compute_pipeline(
            &ctx,
            &blur_bgl_layout,
            "sharpen-box-h",
            include_str!("shaders/sharpen_box_h.wgsl"),
        );
        let v_pipeline = compute_pipeline(
            &ctx,
            &blur_bgl_layout,
            "sharpen-box-v",
            include_str!("shaders/sharpen_box_v.wgsl"),
        );

        let apply_bgl_layout = apply_bgl(device);
        let apply_pipeline = compute_pipeline(
            &ctx,
            &apply_bgl_layout,
            "sharpen-apply",
            include_str!("shaders/sharpen_apply.wgsl"),
        );

        let apply_detail_bgl_layout = apply_detail_bgl(device);
        let apply_detail_pipeline = compute_pipeline(
            &ctx,
            &apply_detail_bgl_layout,
            "sharpen-apply-detail",
            include_str!("shaders/sharpen_apply_detail.wgsl"),
        );

        let masked_apply_bgl_layout = masked_apply_bgl(device);
        let masked_apply_pipeline = compute_pipeline(
            &ctx,
            &masked_apply_bgl_layout,
            "sharpen-apply-masked",
            include_str!("shaders/sharpen_apply_masked.wgsl"),
        );

        Self {
            ctx,
            params,
            layers,
            shared_masks,
            uniform_pool: RefCell::new(Vec::new()),
            uniform_cursor: Cell::new(0),
            blur_bgl: blur_bgl_layout,
            h_pipeline,
            v_pipeline,
            apply_bgl: apply_bgl_layout,
            apply_pipeline,
            apply_detail_bgl: apply_detail_bgl_layout,
            apply_detail_pipeline,
            masked_apply_bgl: masked_apply_bgl_layout,
            masked_apply_pipeline,
            blur_slots: RefCell::new(Vec::new()),
            out: RefCell::new(None),
            blurs: Cell::new(0),
            evals: Cell::new(0),
        }
    }

    /// Ensure blur slot `idx` exists and is sized for `(w, h)`. `idx`
    /// addresses a POSITION in this evaluate's distinct-radii list, not a
    /// specific radius value (see the `blur_slots` field doc).
    fn ensure_blur_slot(&self, idx: usize, w: u32, h: u32) {
        let mut slots = self.blur_slots.borrow_mut();
        while slots.len() <= idx {
            slots.push(Intermediates::new(&self.ctx, w, h));
        }
        if slots[idx].dims != (w, h) {
            slots[idx] = Intermediates::new(&self.ctx, w, h);
        }
    }

    fn alloc_out(&self, w: u32, h: u32, label: &str) -> PipelineImage {
        let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
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
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        PipelineImage {
            texture: Arc::new(tex),
            width: w,
            height: h,
        }
    }

    /// Return the A/B slot that is NOT `input` (by texture identity),
    /// allocating/reallocating both slots together if dims changed (see the
    /// `out` field doc).
    fn ensure_out(&self, input: &PipelineImage, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        let needs_alloc = match out.as_ref() {
            Some([a, _]) => (a.width, a.height) != (w, h),
            None => true,
        };
        if needs_alloc {
            *out = Some([
                self.alloc_out(w, h, "sharpen-out-a"),
                self.alloc_out(w, h, "sharpen-out-b"),
            ]);
        }
        let [a, b] = out.as_ref().unwrap();
        if Arc::ptr_eq(&a.texture, &input.texture) {
            b.clone()
        } else {
            a.clone()
        }
    }

    /// Write `val` into the next pooled per-dispatch uniform slot (grow on
    /// demand) and return its index — see the `uniform_pool` field doc for
    /// why each dispatch needs its OWN buffer.
    fn uniform_slot(&self, val: SharpenUniform) -> usize {
        let slot = self.uniform_cursor.get();
        self.uniform_cursor.set(slot + 1);
        {
            let mut bufs = self.uniform_pool.borrow_mut();
            while bufs.len() <= slot {
                bufs.push(self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("sharpen-uniform"),
                    size: std::mem::size_of::<SharpenUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
        }
        let bufs = self.uniform_pool.borrow();
        self.ctx
            .queue
            .write_buffer(&bufs[slot], 0, bytemuck::bytes_of(&val));
        slot
    }

    /// Encode the separable box blur (H then V) for ONE radius: `src_view` ->
    /// `h_view` (horizontal pass) -> `blur_view` (vertical pass, the final
    /// blur). `uniform_buf` carries that radius (its `amount` field is dead —
    /// kept only so the WGSL `struct P` layout matches the apply passes'
    /// byte-for-byte).
    #[allow(clippy::too_many_arguments)]
    fn encode_blur(
        &self,
        enc: &mut wgpu::CommandEncoder,
        uniform_buf: &wgpu::Buffer,
        src_view: &wgpu::TextureView,
        h_view: &wgpu::TextureView,
        blur_view: &wgpu::TextureView,
        w: u32,
        h: u32,
    ) {
        let h_bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sharpen-box-h-bind"),
                layout: &self.blur_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(h_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniform_buf.as_entire_binding(),
                    },
                ],
            });
        let v_bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sharpen-box-v-bind"),
                layout: &self.blur_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(h_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(blur_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniform_buf.as_entire_binding(),
                    },
                ],
            });

        dispatch(enc, "sharpen-box-h", &self.h_pipeline, &h_bind, w, h);
        dispatch(enc, "sharpen-box-v", &self.v_pipeline, &v_bind, w, h);
    }

    /// Encode the GLOBAL (unmasked) apply dispatch — unchanged shape from
    /// Task 1.
    #[allow(clippy::too_many_arguments)]
    fn encode_apply(
        &self,
        enc: &mut wgpu::CommandEncoder,
        src_view: &wgpu::TextureView,
        blur_view: &wgpu::TextureView,
        dst_view: &wgpu::TextureView,
        uniform_buf: &wgpu::Buffer,
        w: u32,
        h: u32,
    ) {
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sharpen-apply-bind"),
                layout: &self.apply_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(blur_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(dst_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: uniform_buf.as_entire_binding(),
                    },
                ],
            });
        dispatch(enc, "sharpen-apply", &self.apply_pipeline, &bind, w, h);
    }

    /// Encode the Detail/Masking-aware GLOBAL apply dispatch (Phase 4 Task
    /// 5) — same as `encode_apply` plus a second blur texture (`blur_fine`,
    /// binding 2). Only ever used for the GLOBAL op; see the module doc.
    #[allow(clippy::too_many_arguments)]
    fn encode_apply_detail(
        &self,
        enc: &mut wgpu::CommandEncoder,
        src_view: &wgpu::TextureView,
        blur_view: &wgpu::TextureView,
        blur_fine_view: &wgpu::TextureView,
        dst_view: &wgpu::TextureView,
        uniform_buf: &wgpu::Buffer,
        w: u32,
        h: u32,
    ) {
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sharpen-apply-detail-bind"),
                layout: &self.apply_detail_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(blur_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(blur_fine_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(dst_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: uniform_buf.as_entire_binding(),
                    },
                ],
            });
        dispatch(
            enc,
            "sharpen-apply-detail",
            &self.apply_detail_pipeline,
            &bind,
            w,
            h,
        );
    }

    /// Encode a per-mask-layer masked apply dispatch (Phase 4 Task 4).
    #[allow(clippy::too_many_arguments)]
    fn encode_masked_apply(
        &self,
        enc: &mut wgpu::CommandEncoder,
        accum_view: &wgpu::TextureView,
        orig_src_view: &wgpu::TextureView,
        blur_view: &wgpu::TextureView,
        mask_view: &wgpu::TextureView,
        dst_view: &wgpu::TextureView,
        uniform_buf: &wgpu::Buffer,
        w: u32,
        h: u32,
    ) {
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sharpen-apply-masked-bind"),
                layout: &self.masked_apply_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(accum_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(orig_src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(blur_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(mask_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(dst_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: uniform_buf.as_entire_binding(),
                    },
                ],
            });
        dispatch(
            enc,
            "sharpen-apply-masked",
            &self.masked_apply_pipeline,
            &bind,
            w,
            h,
        );
    }

    /// Number of distinct-radius blur pairs computed so far (cumulative,
    /// test hook): proves "one blur per DISTINCT radius", not per dispatch.
    #[cfg(test)]
    pub(crate) fn blur_count(&self) -> u32 {
        self.blurs.get()
    }

    /// Number of times this node's `evaluate` has run (cumulative, test
    /// hook): mirrors `LocalAdjustmentsNode::eval_count`.
    #[cfg(test)]
    pub(crate) fn eval_count(&self) -> u32 {
        self.evals.get()
    }
}

impl Node<PipelineImage> for SharpenNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        self.evals.set(self.evals.get() + 1);
        let src = inputs[0];
        self.uniform_cursor.set(0);

        let p = self.params.get();
        let global_active = p.amount != 0.0 && p.radius > 0;
        // Phase 4 Task 5: the global dispatch needs the Detail/Masking apply
        // shader whenever either is active. `blur_fine` (radius
        // `max(1, r/3)`) is only actually COMPUTED (a distinct blur pair
        // requested below) when `detail != 0.0` — when only `masking` is
        // active the main blur is bound to the `blur_fine` slot too, since
        // `mix(delta_r, delta_fine, 0.0)` discards it (see the module doc's
        // efficiency note).
        let global_extras_active = global_active && (p.detail != 0.0 || p.masking != 0.0);
        let global_fine_radius = (global_active && p.detail != 0.0).then(|| (p.radius / 3).max(1));

        // Phase 4 Task 4: gather each VISIBLE layer's own active sharpen
        // (amount != 0, radius > 0 after the same `MAX_SHARPEN_RADIUS` clamp
        // `sharpen_uniform` applies) alongside the Color engine's composited
        // mask for that layer index. A `shared_masks` entry whose index no
        // longer resolves (can't happen per `SharedMasks`'s freshness
        // invariant, but guarded defensively) or whose live amount is now 0
        // contributes nothing.
        let layer_ops: Vec<(i32, f32, MaskBuffer)> = {
            let layers = self.layers.borrow();
            let shared = self.shared_masks.borrow();
            shared
                .buffers
                .iter()
                .filter_map(|(idx, mask)| {
                    let layer = layers.layers.get(*idx)?;
                    let s = layer.adjustments.sharpen;
                    let r = s.radius.min(MAX_SHARPEN_RADIUS) as i32;
                    (s.amount != 0.0 && r > 0).then(|| (r, s.amount, mask.clone()))
                })
                .collect()
        };

        // Identity passthrough: neither the global op nor any visible layer
        // has an active sharpen — return the input unchanged, no GPU work at
        // all (mirrors `DehazeTransmissionNode`'s early-return pattern).
        if !global_active && layer_ops.is_empty() {
            return src.clone();
        }

        let (w, h) = (src.width, src.height);

        // One blur per DISTINCT radius: the global radius (if active) first,
        // then the global op's `blur_fine` radius (Phase 4 Task 5, only when
        // `detail != 0.0` — see above), then each active layer's radius, all
        // deduplicated.
        let mut radii: Vec<i32> = Vec::new();
        if global_active {
            radii.push(p.radius);
        }
        if let Some(fr) = global_fine_radius {
            if !radii.contains(&fr) {
                radii.push(fr);
            }
        }
        for (r, _, _) in &layer_ops {
            if !radii.contains(r) {
                radii.push(*r);
            }
        }

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sharpen-separable"),
            });
        let src_view = view(&src.texture);

        // Compute every distinct radius's blur FIRST, before any apply pass.
        for (i, &r) in radii.iter().enumerate() {
            self.ensure_blur_slot(i, w, h);
            let slot = self.uniform_slot(SharpenUniform {
                amount: 0.0,
                radius: r,
                detail: 0.0,
                masking: 0.0,
            });
            let bufs = self.uniform_pool.borrow();
            let ubuf = &bufs[slot];
            let blur_slots = self.blur_slots.borrow();
            let im = &blur_slots[i];
            let h_view = view(&im.h_blur);
            let blur_view = view(&im.blur);
            self.encode_blur(&mut enc, ubuf, &src_view, &h_view, &blur_view, w, h);
            self.blurs.set(self.blurs.get() + 1);
        }

        let mut current: PipelineImage = src.clone();

        if global_active {
            let idx = radii
                .iter()
                .position(|&x| x == p.radius)
                .expect("global radius pushed above");
            let blur_view = {
                let blur_slots = self.blur_slots.borrow();
                view(&blur_slots[idx].blur)
            };
            let out = self.ensure_out(&current, w, h);
            let slot = self.uniform_slot(SharpenUniform {
                amount: p.amount,
                radius: p.radius,
                detail: p.detail,
                masking: p.masking,
            });
            if global_extras_active {
                // Phase 4 Task 5: either detail or masking is active, so
                // route through the Detail/Masking apply shader. When
                // `detail == 0.0` (masking-only), `global_fine_radius` is
                // `None` and `blur_fine` binds the SAME main-blur texture as
                // `blur` — `mix(delta_r, delta_fine, 0.0)` in the shader
                // discards it, so this stays correct without computing a
                // second blur.
                let fine_idx = match global_fine_radius {
                    Some(fr) => radii
                        .iter()
                        .position(|&x| x == fr)
                        .expect("fine radius pushed above"),
                    None => idx,
                };
                let blur_fine_view = {
                    let blur_slots = self.blur_slots.borrow();
                    view(&blur_slots[fine_idx].blur)
                };
                let bufs = self.uniform_pool.borrow();
                let ubuf = &bufs[slot];
                let cur_view = view(&current.texture);
                let out_view = view(&out.texture);
                self.encode_apply_detail(
                    &mut enc,
                    &cur_view,
                    &blur_view,
                    &blur_fine_view,
                    &out_view,
                    ubuf,
                    w,
                    h,
                );
            } else {
                // Both zero: dispatch the OLD shader with the identical bind
                // group as before this task — gate 2 (design §7.2), so this
                // path stays byte-exact rather than merely equivalent.
                let bufs = self.uniform_pool.borrow();
                let ubuf = &bufs[slot];
                let cur_view = view(&current.texture);
                let out_view = view(&out.texture);
                self.encode_apply(&mut enc, &cur_view, &blur_view, &out_view, ubuf, w, h);
            }
            current = out;
        }

        for (r, amount, mask) in &layer_ops {
            let idx = radii
                .iter()
                .position(|x| x == r)
                .expect("layer radius collected above");
            let blur_view = {
                let blur_slots = self.blur_slots.borrow();
                view(&blur_slots[idx].blur)
            };
            let out = self.ensure_out(&current, w, h);
            let slot = self.uniform_slot(SharpenUniform {
                amount: *amount,
                radius: *r,
                detail: 0.0,
                masking: 0.0,
            });
            {
                let bufs = self.uniform_pool.borrow();
                let ubuf = &bufs[slot];
                let accum_view = view(&current.texture);
                let mask_view = view(&mask.texture);
                let out_view = view(&out.texture);
                self.encode_masked_apply(
                    &mut enc,
                    &accum_view,
                    &src_view,
                    &blur_view,
                    &mask_view,
                    &out_view,
                    ubuf,
                    w,
                    h,
                );
            }
            current = out;
        }

        self.ctx.queue.submit([enc.finish()]);
        current
    }
}

impl Node<PipelineImage> for Rc<SharpenNode> {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        (**self).evaluate(inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::{AdjustmentSet, MaskLayer};
    use crate::nodes::upload_source;
    use crate::op::Sharpen;
    use ferrolite_image::LinearRgbaF32;
    use ferrolite_mask::MaskDefinition;

    /// CPU reference: the OLD fused 2D box mean — mirrors `sharpen.wgsl`'s
    /// nested loop and clamp exactly (clamp the combined `(x+dx, y+dy)` to
    /// `[0, dims-1]` on BOTH axes together, normalize by the actual tap count
    /// `(2r+1)^2`).
    fn box_mean_2d(px: &[[f32; 3]], w: usize, h: usize, r: i32) -> Vec<[f32; 3]> {
        let mut out = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0f32; 3];
                let mut n = 0.0f32;
                for dy in -r..=r {
                    for dx in -r..=r {
                        let qx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                        let qy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                        let p = px[qy * w + qx];
                        sum[0] += p[0];
                        sum[1] += p[1];
                        sum[2] += p[2];
                        n += 1.0;
                    }
                }
                out[y * w + x] = [sum[0] / n, sum[1] / n, sum[2] / n];
            }
        }
        out
    }

    /// CPU reference: the separable H-then-V box mean — mirrors
    /// `sharpen_box_h.wgsl`/`sharpen_box_v.wgsl` exactly (clamp only the
    /// pass's own axis, normalize by `2r+1` each pass).
    fn box_mean_separable(px: &[[f32; 3]], w: usize, h: usize, r: i32) -> Vec<[f32; 3]> {
        let n = (2 * r + 1) as f32;
        let mut h_out = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0f32; 3];
                for dx in -r..=r {
                    let qx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    let p = px[y * w + qx];
                    sum[0] += p[0];
                    sum[1] += p[1];
                    sum[2] += p[2];
                }
                h_out[y * w + x] = [sum[0] / n, sum[1] / n, sum[2] / n];
            }
        }
        let mut v_out = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0f32; 3];
                for dy in -r..=r {
                    let qy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                    let p = h_out[qy * w + x];
                    sum[0] += p[0];
                    sum[1] += p[1];
                    sum[2] += p[2];
                }
                v_out[y * w + x] = [sum[0] / n, sum[1] / n, sum[2] / n];
            }
        }
        v_out
    }

    /// Deterministic gradient + cheap pseudo-noise fixture (no RNG dependency,
    /// mirrors this crate's other test fixtures — e.g. `engine_bench.rs`'s
    /// `bench_source`).
    fn gradient_noise_fixture(w: usize, h: usize) -> Vec<[f32; 3]> {
        let mut px = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let n = ((x * 7 + y * 13) % 11) as f32 / 10.0;
                px[y * w + x] = [
                    x as f32 / w as f32 + n * 0.05,
                    y as f32 / h as f32 + n * 0.03,
                    0.25 + n * 0.02,
                ];
            }
        }
        px
    }

    /// Step 1 (TDD): proves the separable H-then-V box mean equals the fused
    /// 2D box mean, per-pixel, within float noise — BEFORE any GPU shader
    /// exists. This is the mathematical identity `SharpenNode`'s three-pass
    /// split depends on (see the module doc).
    #[test]
    fn separable_box_equals_2d_box() {
        let (w, h, r) = (16usize, 16usize, 3i32);
        let px = gradient_noise_fixture(w, h);
        let two_d = box_mean_2d(&px, w, h, r);
        let sep = box_mean_separable(&px, w, h, r);
        assert_eq!(two_d.len(), sep.len());
        for (i, (a, b)) in two_d.iter().zip(sep.iter()).enumerate() {
            for c in 0..3 {
                let d = (a[c] - b[c]).abs();
                assert!(
                    d < 1e-6,
                    "pixel {i} channel {c}: 2d={} separable={} diff={d}",
                    a[c],
                    b[c]
                );
            }
        }
    }

    /// Read all four RGBA channels of an `Rgba16Float` `PipelineImage` back to
    /// f32 (test-only; mirrors `dehaze_node.rs::read_rgba_channels`).
    fn read_rgba_channels(ctx: &GpuContext, img: &PipelineImage) -> Vec<[f32; 4]> {
        let (w, h) = (img.width, img.height);
        let bpp = 8u32; // RGBA16F
        let bpr_unpadded = w * bpp;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bpr_padded = bpr_unpadded.div_ceil(align) * align;
        let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sharpen-node-test-readback"),
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

    /// Step 1 (TDD) continued: `SharpenNode`'s GPU output must match the OLD
    /// fused 2D formula computed CPU-side, within `2e-3` (absorbs the
    /// rgba16float storage round-trip through two intermediate planes, same
    /// order as `dehaze_node.rs`'s GPU-vs-CPU tolerances).
    #[test]
    fn sharpen_node_matches_old_2d_formula() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (32u32, 24u32);
        let (r, amount) = (3i32, 0.8f32);

        let planar = gradient_noise_fixture(w as usize, h as usize);
        let mut interleaved = Vec::with_capacity((w * h * 4) as usize);
        for p in &planar {
            interleaved.extend_from_slice(&[p[0], p[1], p[2], 1.0]);
        }
        let img = LinearRgbaF32::new(w, h, interleaved).expect("fixture image");
        let src = upload_source(&ctx, &img);

        let params = Rc::new(Cell::new(SharpenUniform {
            amount,
            radius: r,
            detail: 0.0,
            masking: 0.0,
        }));
        let node = SharpenNode::new(ctx.clone(), params, no_layers(), no_shared_masks());
        let out = node.evaluate(&[&src]);
        assert_eq!((out.width, out.height), (w, h));

        let gpu = read_rgba_channels(&ctx, &out);
        let cpu_blur = box_mean_2d(&planar, w as usize, h as usize, r);

        let mut max_d = 0.0f32;
        for (i, (g, blur)) in gpu.iter().zip(cpu_blur.iter()).enumerate() {
            let c = planar[i];
            for ch in 0..3 {
                let expected = (c[ch] + amount * (c[ch] - blur[ch])).max(0.0);
                let d = (g[ch] - expected).abs();
                max_d = max_d.max(d);
                assert!(
                    d < 2e-3,
                    "pixel {i} channel {ch}: gpu={} expected={} diff={d}",
                    g[ch],
                    expected
                );
            }
            assert!((g[3] - 1.0).abs() < 1e-6, "alpha mismatch at pixel {i}");
        }
        eprintln!("sharpen_node_matches_old_2d_formula: max abs diff = {max_d}");
    }

    /// Identity passthrough: `amount == 0` must return the SAME texture (an
    /// `Arc` clone, not a copy) — no GPU work, byte-identical to the input.
    #[test]
    fn sharpen_node_identity_passthrough_same_texture() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (8u32, 8u32);
        let px = vec![0.3f32; (w * h * 4) as usize];
        let img = LinearRgbaF32::new(w, h, px).expect("flat fixture");
        let src = upload_source(&ctx, &img);

        let params = Rc::new(Cell::new(SharpenUniform {
            amount: 0.0,
            radius: 5,
            detail: 0.0,
            masking: 0.0,
        }));
        let node = SharpenNode::new(ctx.clone(), params, no_layers(), no_shared_masks());
        let out = node.evaluate(&[&src]);
        assert!(
            Arc::ptr_eq(&out.texture, &src.texture),
            "amount == 0 must return the input texture unchanged (no dispatch, no copy)"
        );

        // radius <= 0 must also passthrough, independent of amount.
        let params2 = Rc::new(Cell::new(SharpenUniform {
            amount: 0.8,
            radius: 0,
            detail: 0.0,
            masking: 0.0,
        }));
        let node2 = SharpenNode::new(ctx, params2, no_layers(), no_shared_masks());
        let out2 = node2.evaluate(&[&src]);
        assert!(
            Arc::ptr_eq(&out2.texture, &src.texture),
            "radius <= 0 must return the input texture unchanged"
        );
    }

    // ── Phase 4 Task 4: per-mask sharpen ──────────────────────────────────

    fn no_layers() -> Rc<RefCell<LocalAdjustments>> {
        Rc::new(RefCell::new(LocalAdjustments::default()))
    }

    fn no_shared_masks() -> Rc<RefCell<SharedMasks>> {
        Rc::new(RefCell::new(SharedMasks::default()))
    }

    /// A mask layer at `layers.layers[idx]` with the given sharpen
    /// amount/radius (everything else default/identity).
    fn sharpen_layer(amount: f32, radius: u32) -> MaskLayer {
        MaskLayer {
            name: "l".into(),
            visible: true,
            mask: MaskDefinition::default(),
            adjustments: AdjustmentSet {
                sharpen: Sharpen {
                    amount,
                    radius,
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    /// A constant-value `MaskBuffer` (test-only; mirrors
    /// `local_node.rs::alloc_full_coverage_mask`'s write pattern, generalized
    /// to an arbitrary per-pixel value list).
    fn mask_buffer_from(ctx: &GpuContext, w: u32, h: u32, values: &[f32]) -> MaskBuffer {
        assert_eq!(values.len(), (w * h) as usize);
        let buf = MaskBuffer::alloc(ctx, w, h);
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &buf.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(values),
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

    /// A full-coverage (1.0 everywhere) mask layer's sharpen must equal the
    /// GLOBAL sharpen at the same amount/radius (full coverage ⇒ same
    /// result) — the direct GPU proof that the masked-apply formula reduces
    /// to the global formula when `mask == 1.0` and there's nothing else in
    /// the accumulator yet (`accum == src`).
    #[test]
    fn full_coverage_mask_layer_sharpen_equals_global_sharpen() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (32u32, 24u32);
        let (r, amount) = (2i32, 1.0f32);

        let planar = gradient_noise_fixture(w as usize, h as usize);
        let mut interleaved = Vec::with_capacity((w * h * 4) as usize);
        for p in &planar {
            interleaved.extend_from_slice(&[p[0], p[1], p[2], 1.0]);
        }
        let img = LinearRgbaF32::new(w, h, interleaved).expect("fixture image");
        let src = upload_source(&ctx, &img);

        // Node A: global sharpen active, no layers.
        let global_params = Rc::new(Cell::new(SharpenUniform {
            amount,
            radius: r,
            detail: 0.0,
            masking: 0.0,
        }));
        let node_a = SharpenNode::new(ctx.clone(), global_params, no_layers(), no_shared_masks());
        let out_a = node_a.evaluate(&[&src]);

        // Node B: global inactive, ONE full-coverage layer with the SAME
        // amount/radius.
        let inactive_global = Rc::new(Cell::new(SharpenUniform {
            amount: 0.0,
            radius: 0,
            detail: 0.0,
            masking: 0.0,
        }));
        let layers = Rc::new(RefCell::new(LocalAdjustments {
            layers: vec![sharpen_layer(amount, r as u32)],
        }));
        let full_mask = mask_buffer_from(&ctx, w, h, &vec![1.0f32; (w * h) as usize]);
        let shared_masks = Rc::new(RefCell::new(SharedMasks {
            buffers: vec![(0, full_mask)],
        }));
        let node_b = SharpenNode::new(ctx.clone(), inactive_global, layers, shared_masks);
        let out_b = node_b.evaluate(&[&src]);

        let a_px = read_rgba_channels(&ctx, &out_a);
        let b_px = read_rgba_channels(&ctx, &out_b);
        let mut max_d = 0.0f32;
        for (a, b) in a_px.iter().zip(b_px.iter()) {
            for ch in 0..4 {
                max_d = max_d.max((a[ch] - b[ch]).abs());
            }
        }
        assert!(
            max_d < 1e-4,
            "full-coverage mask-layer sharpen must equal global sharpen at the same params: max diff {max_d}"
        );
    }

    /// A half-coverage mask (left half 1.0, right half 0.0) must sharpen ONLY
    /// the masked (left) half; the unmasked (right) half stays bit-identical
    /// to the ORIGINAL input (global inactive, so `accum == src` there and
    /// `mask == 0` adds nothing).
    #[test]
    fn half_coverage_mask_layer_sharpens_only_masked_pixels() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (16u32, 12u32);
        let (r, amount) = (2i32, 0.6f32);

        let planar = gradient_noise_fixture(w as usize, h as usize);
        let mut interleaved = Vec::with_capacity((w * h * 4) as usize);
        for p in &planar {
            interleaved.extend_from_slice(&[p[0], p[1], p[2], 1.0]);
        }
        let img = LinearRgbaF32::new(w, h, interleaved.clone()).expect("fixture image");
        let src = upload_source(&ctx, &img);

        let mut mask_values = vec![0.0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..(w / 2) {
                mask_values[(y * w + x) as usize] = 1.0;
            }
        }
        let mask = mask_buffer_from(&ctx, w, h, &mask_values);

        let inactive_global = Rc::new(Cell::new(SharpenUniform {
            amount: 0.0,
            radius: 0,
            detail: 0.0,
            masking: 0.0,
        }));
        let layers = Rc::new(RefCell::new(LocalAdjustments {
            layers: vec![sharpen_layer(amount, r as u32)],
        }));
        let shared_masks = Rc::new(RefCell::new(SharedMasks {
            buffers: vec![(0, mask)],
        }));
        let node = SharpenNode::new(ctx.clone(), inactive_global, layers, shared_masks);
        let out = node.evaluate(&[&src]);

        let gpu = read_rgba_channels(&ctx, &out);
        let cpu_blur = box_mean_2d(&planar, w as usize, h as usize, r);

        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = y * w as usize + x;
                let c = planar[i];
                if x < (w / 2) as usize {
                    // Masked: full sharpen formula applies.
                    for ch in 0..3 {
                        let expected = (c[ch] + amount * (c[ch] - cpu_blur[i][ch])).max(0.0);
                        let d = (gpu[i][ch] - expected).abs();
                        assert!(
                            d < 2e-3,
                            "masked pixel ({x},{y}) ch{ch}: gpu={} expected={} diff={d}",
                            gpu[i][ch],
                            expected
                        );
                    }
                } else {
                    // Unmasked: equals the original input within the f16
                    // upload/storage round-trip tolerance (the SAME 2e-3
                    // `sharpen_node_matches_old_2d_formula` uses above — `c`
                    // is the pre-upload f32 CPU value, `gpu` came from an f16
                    // texture the whole way through, so an exact bit
                    // comparison would spuriously fail on ordinary
                    // quantization, not a logic bug).
                    for ch in 0..3 {
                        let d = (gpu[i][ch] - c[ch]).abs();
                        assert!(
                            d < 2e-3,
                            "unmasked pixel ({x},{y}) ch{ch}: gpu={} src={} diff={d}",
                            gpu[i][ch],
                            c[ch]
                        );
                    }
                }
            }
        }
    }

    /// Distinct global/layer radii must produce exactly 2 blurs (one per
    /// DISTINCT radius); the SAME radius on both must produce exactly 1.
    #[test]
    fn distinct_radii_produce_one_blur_each_shared_radius_reused() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (16u32, 16u32);
        let img = LinearRgbaF32::new(w, h, vec![0.3f32; (w * h * 4) as usize]).expect("fixture");
        let src = upload_source(&ctx, &img);
        let mask = mask_buffer_from(&ctx, w, h, &vec![1.0f32; (w * h) as usize]);

        // Distinct radii: global r=2, layer r=5.
        let params = Rc::new(Cell::new(SharpenUniform {
            amount: 0.5,
            radius: 2,
            detail: 0.0,
            masking: 0.0,
        }));
        let layers = Rc::new(RefCell::new(LocalAdjustments {
            layers: vec![sharpen_layer(0.4, 5)],
        }));
        let shared_masks = Rc::new(RefCell::new(SharedMasks {
            buffers: vec![(0, mask.clone())],
        }));
        let node = SharpenNode::new(ctx.clone(), params, layers, shared_masks);
        node.evaluate(&[&src]);
        assert_eq!(node.blur_count(), 2, "distinct radii must yield 2 blurs");

        // Shared radius: global r=3, layer r=3 too — one blur, reused.
        let params2 = Rc::new(Cell::new(SharpenUniform {
            amount: 0.5,
            radius: 3,
            detail: 0.0,
            masking: 0.0,
        }));
        let layers2 = Rc::new(RefCell::new(LocalAdjustments {
            layers: vec![sharpen_layer(0.4, 3)],
        }));
        let shared_masks2 = Rc::new(RefCell::new(SharedMasks {
            buffers: vec![(0, mask)],
        }));
        let node2 = SharpenNode::new(ctx, params2, layers2, shared_masks2);
        node2.evaluate(&[&src]);
        assert_eq!(
            node2.blur_count(),
            1,
            "a radius shared by global + layer must yield exactly 1 blur"
        );
    }

    /// A layer whose `shared_masks` entry exists but whose LIVE sharpen
    /// amount is 0 (identity) must add zero dispatches: output equals
    /// whatever the global-only path (or plain passthrough) would produce,
    /// and no blur is computed for its radius unless some OTHER active
    /// dispatch happens to share it.
    #[test]
    fn identity_layer_sharpen_adds_zero_dispatches() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (8u32, 8u32);
        let img = LinearRgbaF32::new(w, h, vec![0.3f32; (w * h * 4) as usize]).expect("fixture");
        let src = upload_source(&ctx, &img);
        let mask = mask_buffer_from(&ctx, w, h, &vec![1.0f32; (w * h) as usize]);

        // Global inactive, one layer present but amount == 0 (its OWN
        // radius, 9, is distinct from anything else) -> pure passthrough,
        // zero blurs.
        let params = Rc::new(Cell::new(SharpenUniform {
            amount: 0.0,
            radius: 0,
            detail: 0.0,
            masking: 0.0,
        }));
        let layers = Rc::new(RefCell::new(LocalAdjustments {
            layers: vec![sharpen_layer(0.0, 9)],
        }));
        let shared_masks = Rc::new(RefCell::new(SharedMasks {
            buffers: vec![(0, mask)],
        }));
        let node = SharpenNode::new(ctx.clone(), params, layers, shared_masks);
        let out = node.evaluate(&[&src]);
        assert!(
            Arc::ptr_eq(&out.texture, &src.texture),
            "identity layer (amount 0) + inactive global must passthrough with zero dispatches"
        );
        assert_eq!(node.blur_count(), 0, "identity layer must compute no blur");
    }
}
