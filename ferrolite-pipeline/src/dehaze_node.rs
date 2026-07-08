//! `DehazeTransmissionNode` — the refined dehaze transmission map as a
//! multi-pass `Node<PipelineImage>`. Mirrors `crate::dehaze::transmission_map`
//! (separable dark-channel block-min + guided-filter refinement) exactly, on
//! the GPU. Independent of `amount`: the `Graph`'s dirty-caching means an
//! amount-only drag (handled by the future `DehazeRecoveryNode`, QS-Task 3)
//! never re-triggers this node's (relatively) expensive multi-pass evaluate.
//!
//! Pass structure (mirrors `transmission_map` step-by-step; see that fn's doc
//! for the reference math):
//!   1. `dehaze_dark_channel` — src rgba16float -> `dc0` (normalized dark
//!      channel) + `guide` (luma), both R32Float.
//!   2. `dehaze_min_h` then `dehaze_min_v` — separable block-min of `dc0` over
//!      `radius`; `min_v` folds in the `praw = clamp(1-omega*dc,0,1)` transform.
//!   3. `dehaze_products` — `gg = guide*guide`, `gp = guide*praw`.
//!   4. `dehaze_box_h`/`dehaze_box_v` (radius = guided radius `gr`) applied to
//!      `guide, praw, gg, gp` -> `mean_g, mean_p, corr_g, corr_gp`. The SAME two
//!      pipelines are reused (different bind groups) for all four planes, and
//!      again in step 6 for `a, b`.
//!   5. `dehaze_guided_ab` — the guided-filter linear coefficients `a`, `b`.
//!   6. box filter (reusing step 4's pipelines) `a, b` -> `mean_a, mean_b`.
//!   7. `dehaze_guided_q` — `q = clamp(mean_a*guide + mean_b, 0, 1)`, written
//!      into all four channels of the rgba16float output.
//!
//! All intermediate planes are single-channel `R32Float` storage textures,
//! cached in `Intermediates` and reallocated only when `(w, h)` changes
//! (mirrors `local_node.rs`'s `alloc_out`/`ensure_out` pattern). Every compute
//! pipeline is built ONCE in `new` (CLAUDE.md GPU rule) and all eight passes of
//! one `evaluate` are encoded into a single command buffer / single submit;
//! wgpu's automatic resource tracking inserts the necessary barriers between
//! passes that read a plane a previous pass wrote (or reused as scratch).
//!
//! Wired into `EditPipeline` (QS-Task 4; whole-image preview tier) between
//! `contrast` and `tone_curve`. `TileEditPipeline` (QS-Task 5) still uses the
//! old single-pass `dehaze.wgsl` `PointOpNode` for now.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Node};

use crate::dehaze::{guided_radius, DEHAZE_ATMOS_MIN, DEHAZE_GUIDED_EPS, DEHAZE_OMEGA, DEHAZE_T0};
use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::op::Dehaze;
use crate::MAX_DEHAZE_RADIUS;

/// Single-channel intermediate plane format used by every transmission pass.
const PLANE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

/// GPU uniform shared by every transmission pass that needs it. `#[repr(C)]`,
/// 16-byte aligned; field order + explicit padding MIRROR the WGSL `struct P`
/// in each shader exactly (`dehaze_dark_channel.wgsl`, `dehaze_min_h.wgsl`,
/// `dehaze_min_v.wgsl`, `dehaze_box_h.wgsl`, `dehaze_box_v.wgsl`,
/// `dehaze_guided_ab.wgsl`). `radius` is OVERLOADED by the node: passes that
/// need the block-min radius (`dark_channel`'s `atmos` doesn't use it, but
/// `min_h`/`min_v` do) bind a uniform written with the block radius; passes
/// that need the guided-filter box radius (`box_h`/`box_v`/`guided_ab`'s
/// unused-here `eps`) bind a SEPARATE uniform of the same layout written with
/// `gr = guided_radius(radius)`. Two buffers, same struct.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct PassUniform {
    radius: i32,
    pad0: i32,
    pad1: [i32; 2],
    atmos: [f32; 4],
    omega: f32,
    eps: f32,
    pad2: [f32; 2],
}

// `PassUniform`'s explicit padding mirrors the WGSL `struct P`'s implicit
// std140-style alignment (the `vec4<f32> atmos` field forces 16-byte
// alignment); this compile-time check keeps the two from silently drifting.
const _: () = assert!(std::mem::size_of::<PassUniform>() == 48);
const _: () = assert!(std::mem::size_of::<PassUniform>().is_multiple_of(16));

/// Public params for `DehazeTransmissionNode`, read from a shared `Cell` each
/// `evaluate` (mirrors `PointOpNode`'s `Rc<Cell<U>>` pattern). `radius` is the
/// block-min patch radius (`Dehaze::radius`, UNCLAMPED — the node defensively
/// clamps to `MAX_DEHAZE_RADIUS` before use, since a prior review noted the
/// pure `transmission_map`/its loops don't self-clamp). `atmos` is `[r,g,b,pad]`
/// (floored to `DEHAZE_ATMOS_MIN` by `from_op`, mirroring `dehaze_uniform`).
/// `omega`/`eps` mirror `DEHAZE_OMEGA`/`DEHAZE_GUIDED_EPS`. `active` is 1 when a
/// `Dehaze` op with non-zero `amount` is present, else 0 — see `from_op` and
/// `DehazeTransmissionNode::evaluate`'s early-return gate. CRITICAL: `active`
/// deliberately does NOT carry the `amount` magnitude — it only flips on the
/// zero<->nonzero transition, so an amount-only drag (0.5 -> 0.9) leaves this
/// whole struct unchanged and `EditPipeline::set_stack` does not dirty this
/// node (see `amount_change_does_not_recompute_transmission`).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct TransmissionParams {
    pub radius: i32,
    pub atmos: [f32; 4],
    pub omega: f32,
    pub eps: f32,
    pub active: u32,
}

// `TransmissionParams` isn't itself uploaded as GPU bytes (its fields feed
// `PassUniform`, built fresh per-pass in `evaluate`), but it derives Pod +
// Zeroable like the GPU-facing structs in this file, so keep it 16-byte
// aligned/sized for consistency and to catch accidental field-size drift.
const _: () = assert!(std::mem::size_of::<TransmissionParams>() == 32);
const _: () = assert!(std::mem::size_of::<TransmissionParams>().is_multiple_of(16));

impl TransmissionParams {
    /// Seed from the op's `radius`/`amount` and the whole-image atmospheric
    /// light (QS-Task 4). `radius`/`atmos` are independent of `amount` —
    /// `EditPipeline::set_stack` only rebuilds these (and dirties
    /// `DehazeTransmissionNode`) when `radius`, `atmos`, or the active
    /// zero<->nonzero transition actually changes, so an amount-magnitude-only
    /// drag never re-seeds (and never dirties) this node.
    pub(crate) fn from_op(op: Option<Dehaze>, atmos: [f32; 3]) -> Self {
        let radius = op.map(|d| d.radius).unwrap_or(0) as i32;
        let active = u32::from(op.is_some_and(|d| d.amount != 0.0));
        Self {
            radius,
            atmos: [
                atmos[0].max(DEHAZE_ATMOS_MIN),
                atmos[1].max(DEHAZE_ATMOS_MIN),
                atmos[2].max(DEHAZE_ATMOS_MIN),
                0.0,
            ],
            omega: DEHAZE_OMEGA,
            eps: DEHAZE_GUIDED_EPS,
            active,
        }
    }
}

/// Public params for `DehazeRecoveryNode` (QS-Task 3), read from a shared `Cell`
/// each `evaluate`. `amount` drives the blend from I toward the recovered J
/// (amount >= 0) or toward the hazed version (amount < 0). `t0` is the transmission
/// floor (DEHAZE_T0), `atmos` is `[r,g,b,pad]`. Field order MIRRORS the WGSL
/// `struct P` in `dehaze_recovery.wgsl` exactly (both must be 16-byte aligned).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct RecoveryParams {
    pub amount: f32,
    pub t0: f32,
    pub pad0: f32,
    pub pad1: f32,
    pub atmos: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<RecoveryParams>() == 32);
const _: () = assert!(std::mem::size_of::<RecoveryParams>().is_multiple_of(16));

impl RecoveryParams {
    /// Seed from the op's `amount` and the whole-image atmospheric light
    /// (QS-Task 4). Independent of `radius` — an amount-only drag re-seeds
    /// only these params (and dirties `DehazeRecoveryNode`), leaving the
    /// cached `DehazeTransmissionNode` output untouched.
    pub(crate) fn from_op(op: Option<Dehaze>, atmos: [f32; 3]) -> Self {
        let amount = op.map(|d| d.amount).unwrap_or(0.0);
        Self {
            amount,
            t0: DEHAZE_T0,
            pad0: 0.0,
            pad1: 0.0,
            atmos: [
                atmos[0].max(DEHAZE_ATMOS_MIN),
                atmos[1].max(DEHAZE_ATMOS_MIN),
                atmos[2].max(DEHAZE_ATMOS_MIN),
                0.0,
            ],
        }
    }
}

/// All fifteen `R32Float` intermediate planes, keyed on `(w, h)` and
/// reallocated together when the input dims change (mirrors
/// `local_node.rs::CachedMasks`'s dims-keyed cache).
struct Intermediates {
    dims: (u32, u32),
    dc0: wgpu::Texture,
    guide: wgpu::Texture,
    dc_h: wgpu::Texture,
    praw: wgpu::Texture,
    gg: wgpu::Texture,
    gp: wgpu::Texture,
    box_scratch: wgpu::Texture,
    mean_g: wgpu::Texture,
    mean_p: wgpu::Texture,
    corr_g: wgpu::Texture,
    corr_gp: wgpu::Texture,
    a: wgpu::Texture,
    b: wgpu::Texture,
    mean_a: wgpu::Texture,
    mean_b: wgpu::Texture,
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
        format: PLANE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    })
}

impl Intermediates {
    fn new(ctx: &GpuContext, w: u32, h: u32) -> Self {
        Self {
            dims: (w, h),
            dc0: alloc_plane(ctx, w, h, "dehaze-dc0"),
            guide: alloc_plane(ctx, w, h, "dehaze-guide"),
            dc_h: alloc_plane(ctx, w, h, "dehaze-dc-h"),
            praw: alloc_plane(ctx, w, h, "dehaze-praw"),
            gg: alloc_plane(ctx, w, h, "dehaze-gg"),
            gp: alloc_plane(ctx, w, h, "dehaze-gp"),
            box_scratch: alloc_plane(ctx, w, h, "dehaze-box-scratch"),
            mean_g: alloc_plane(ctx, w, h, "dehaze-mean-g"),
            mean_p: alloc_plane(ctx, w, h, "dehaze-mean-p"),
            corr_g: alloc_plane(ctx, w, h, "dehaze-corr-g"),
            corr_gp: alloc_plane(ctx, w, h, "dehaze-corr-gp"),
            a: alloc_plane(ctx, w, h, "dehaze-a"),
            b: alloc_plane(ctx, w, h, "dehaze-b"),
            mean_a: alloc_plane(ctx, w, h, "dehaze-mean-a"),
            mean_b: alloc_plane(ctx, w, h, "dehaze-mean-b"),
        }
    }
}

/// Bind-group layout shared by every "one plane in, one plane out, uniform"
/// pass: `dehaze_min_h`, `dehaze_min_v`, `dehaze_box_h`, `dehaze_box_v`.
fn plane_bgl(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: PLANE_FORMAT,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

fn texture_entry(binding: u32, filterable: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
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

/// The refined dehaze transmission map, computed on the GPU across eight
/// compute passes (see module doc). Built once; pipelines + intermediate
/// textures cached, reallocated only on a `(w, h)` change.
pub(crate) struct DehazeTransmissionNode {
    ctx: Arc<GpuContext>,
    params: Rc<Cell<TransmissionParams>>,
    // Two uniform buffers of the SAME layout: `uniform_min` carries the
    // block-min radius (bound by dark_channel/min_h/min_v), `uniform_box`
    // carries the guided-filter radius `gr` (bound by box_h/box_v/guided_ab).
    // They must be distinct buffers: a single command encoder is submitted
    // once at the end of `evaluate`, so `queue.write_buffer` calls made before
    // that submit both land before ANY of its dispatches run — reusing one
    // buffer for two different radii within the same submit would make the
    // second write silently clobber the first for every pass, not just the
    // ones issued after it.
    uniform_min: wgpu::Buffer,
    uniform_box: wgpu::Buffer,

    dark_bgl: wgpu::BindGroupLayout,
    dark_pipeline: wgpu::ComputePipeline,

    plane_bgl: wgpu::BindGroupLayout,
    min_h_pipeline: wgpu::ComputePipeline,
    min_v_pipeline: wgpu::ComputePipeline,
    box_h_pipeline: wgpu::ComputePipeline,
    box_v_pipeline: wgpu::ComputePipeline,

    products_bgl: wgpu::BindGroupLayout,
    products_pipeline: wgpu::ComputePipeline,

    guided_ab_bgl: wgpu::BindGroupLayout,
    guided_ab_pipeline: wgpu::ComputePipeline,

    guided_q_bgl: wgpu::BindGroupLayout,
    guided_q_pipeline: wgpu::ComputePipeline,

    intermediates: RefCell<Option<Intermediates>>,
    out: RefCell<Option<PipelineImage>>,
    // Test hook: counts full multi-pass evaluates (QS-Task 4 asserts an
    // amount-only change on the downstream recovery node does NOT bump this).
    rebuilds: Cell<u32>,
}

impl DehazeTransmissionNode {
    pub(crate) fn new(ctx: Arc<GpuContext>, params: Rc<Cell<TransmissionParams>>) -> Self {
        let device = &ctx.device;

        let uniform_min = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dehaze-transmission-uniform-min"),
            size: std::mem::size_of::<PassUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_box = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dehaze-transmission-uniform-box"),
            size: std::mem::size_of::<PassUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let dark_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dehaze-dark-bgl"),
            entries: &[
                texture_entry(0, true),             // src rgba16float
                storage_out_entry(1, PLANE_FORMAT), // dc0
                storage_out_entry(2, PLANE_FORMAT), // guide
                uniform_entry(3),
            ],
        });
        let dark_pipeline = compute_pipeline(
            &ctx,
            &dark_bgl,
            "dehaze-dark-channel",
            include_str!("shaders/dehaze_dark_channel.wgsl"),
        );

        let plane_bgl_layout = plane_bgl(device, "dehaze-plane-bgl");
        let min_h_pipeline = compute_pipeline(
            &ctx,
            &plane_bgl_layout,
            "dehaze-min-h",
            include_str!("shaders/dehaze_min_h.wgsl"),
        );
        let min_v_pipeline = compute_pipeline(
            &ctx,
            &plane_bgl_layout,
            "dehaze-min-v",
            include_str!("shaders/dehaze_min_v.wgsl"),
        );
        let box_h_pipeline = compute_pipeline(
            &ctx,
            &plane_bgl_layout,
            "dehaze-box-h",
            include_str!("shaders/dehaze_box_h.wgsl"),
        );
        let box_v_pipeline = compute_pipeline(
            &ctx,
            &plane_bgl_layout,
            "dehaze-box-v",
            include_str!("shaders/dehaze_box_v.wgsl"),
        );

        let products_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dehaze-products-bgl"),
            entries: &[
                texture_entry(0, false),            // guide
                texture_entry(1, false),            // praw
                storage_out_entry(2, PLANE_FORMAT), // gg
                storage_out_entry(3, PLANE_FORMAT), // gp
            ],
        });
        let products_pipeline = compute_pipeline(
            &ctx,
            &products_bgl,
            "dehaze-products",
            include_str!("shaders/dehaze_products.wgsl"),
        );

        let guided_ab_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dehaze-guided-ab-bgl"),
            entries: &[
                texture_entry(0, false),            // mean_g
                texture_entry(1, false),            // mean_p
                texture_entry(2, false),            // corr_g
                texture_entry(3, false),            // corr_gp
                storage_out_entry(4, PLANE_FORMAT), // a
                storage_out_entry(5, PLANE_FORMAT), // b
                uniform_entry(6),
            ],
        });
        let guided_ab_pipeline = compute_pipeline(
            &ctx,
            &guided_ab_bgl,
            "dehaze-guided-ab",
            include_str!("shaders/dehaze_guided_ab.wgsl"),
        );

        let guided_q_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dehaze-guided-q-bgl"),
            entries: &[
                texture_entry(0, false),               // mean_a
                texture_entry(1, false),               // mean_b
                texture_entry(2, false),               // guide
                storage_out_entry(3, PIPELINE_FORMAT), // dst (rgba16float)
            ],
        });
        let guided_q_pipeline = compute_pipeline(
            &ctx,
            &guided_q_bgl,
            "dehaze-guided-q",
            include_str!("shaders/dehaze_guided_q.wgsl"),
        );

        Self {
            ctx,
            params,
            uniform_min,
            uniform_box,
            dark_bgl,
            dark_pipeline,
            plane_bgl: plane_bgl_layout,
            min_h_pipeline,
            min_v_pipeline,
            box_h_pipeline,
            box_v_pipeline,
            products_bgl,
            products_pipeline,
            guided_ab_bgl,
            guided_ab_pipeline,
            guided_q_bgl,
            guided_q_pipeline,
            intermediates: RefCell::new(None),
            out: RefCell::new(None),
            rebuilds: Cell::new(0),
        }
    }

    /// Number of full multi-pass evaluates (test hook; see the field doc).
    #[cfg(test)]
    pub(crate) fn transmission_rebuild_count(&self) -> u32 {
        self.rebuilds.get()
    }

    fn ensure_intermediates(&self, w: u32, h: u32) {
        let mut cur = self.intermediates.borrow_mut();
        let needs_alloc = match cur.as_ref() {
            Some(im) => im.dims != (w, h),
            None => true,
        };
        if needs_alloc {
            *cur = Some(Intermediates::new(&self.ctx, w, h));
        }
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dehaze-transmission-out"),
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
            *out = Some(PipelineImage {
                texture: Arc::new(tex),
                width: w,
                height: h,
            });
        }
        out.as_ref().unwrap().clone()
    }

    /// Bind group for the generic "one plane in, one plane out, uniform" shape.
    fn plane_bind(
        &self,
        in_view: &wgpu::TextureView,
        out_view: &wgpu::TextureView,
        uniform: &wgpu::Buffer,
        label: &str,
    ) -> wgpu::BindGroup {
        self.ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.plane_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(in_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(out_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            })
    }

    /// Run the box_h -> box_v pair (radius = guided radius, via `uniform_box`)
    /// over `input`, writing the normalized box mean into `output`, using
    /// `self`'s single reusable `box_scratch` plane as the H-pass landing spot.
    #[allow(clippy::too_many_arguments)]
    fn box_filter(
        &self,
        enc: &mut wgpu::CommandEncoder,
        label: &str,
        input: &wgpu::TextureView,
        scratch: &wgpu::TextureView,
        output: &wgpu::TextureView,
        w: u32,
        h: u32,
    ) {
        let h_bind = self.plane_bind(
            input,
            scratch,
            &self.uniform_box,
            &format!("{label}-h-bind"),
        );
        dispatch(
            enc,
            &format!("{label}-h"),
            &self.box_h_pipeline,
            &h_bind,
            w,
            h,
        );
        let v_bind = self.plane_bind(
            scratch,
            output,
            &self.uniform_box,
            &format!("{label}-v-bind"),
        );
        dispatch(
            enc,
            &format!("{label}-v"),
            &self.box_v_pipeline,
            &v_bind,
            w,
            h,
        );
    }
}

impl Node<PipelineImage> for DehazeTransmissionNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let raw = self.params.get();

        // Dehaze is off (no `Dehaze` op, or `amount == 0`): `DehazeRecoveryNode`
        // ignores this node's output entirely in that case (passthrough), so
        // running the ~8-pass guided filter here would be pure waste. This is
        // the QS-Task-4 regression fix — `amount`'s magnitude is deliberately
        // NOT part of `TransmissionParams` (see its doc), so this gate only
        // flips on the zero<->nonzero transition, preserving the amount-drag
        // cache proven by `amount_change_does_not_recompute_transmission`.
        // Cloning `PipelineImage` is an `Arc` clone (cheap); no compute passes
        // run and the rebuild-count test hook is NOT bumped.
        if raw.active == 0 {
            return src.clone();
        }

        let (w, h) = (src.width, src.height);
        self.ensure_intermediates(w, h);
        let out = self.ensure_out(w, h);
        self.rebuilds.set(self.rebuilds.get() + 1);

        let radius = (raw.radius.max(0) as u32).min(MAX_DEHAZE_RADIUS) as i32;
        let gr = guided_radius(radius as u32).min(MAX_DEHAZE_RADIUS.saturating_mul(3)) as i32;

        let min_uniform = PassUniform {
            radius,
            pad0: 0,
            pad1: [0; 2],
            atmos: raw.atmos,
            omega: raw.omega,
            eps: raw.eps,
            pad2: [0.0; 2],
        };
        let box_uniform = PassUniform {
            radius: gr,
            pad0: 0,
            pad1: [0; 2],
            atmos: raw.atmos,
            omega: raw.omega,
            eps: raw.eps,
            pad2: [0.0; 2],
        };
        self.ctx
            .queue
            .write_buffer(&self.uniform_min, 0, bytemuck::bytes_of(&min_uniform));
        self.ctx
            .queue
            .write_buffer(&self.uniform_box, 0, bytemuck::bytes_of(&box_uniform));

        let intermediates = self.intermediates.borrow();
        let im = intermediates.as_ref().expect("allocated above");

        let src_view = view(&src.texture);
        let dc0_view = view(&im.dc0);
        let guide_view = view(&im.guide);
        let dc_h_view = view(&im.dc_h);
        let praw_view = view(&im.praw);
        let gg_view = view(&im.gg);
        let gp_view = view(&im.gp);
        let box_scratch_view = view(&im.box_scratch);
        let mean_g_view = view(&im.mean_g);
        let mean_p_view = view(&im.mean_p);
        let corr_g_view = view(&im.corr_g);
        let corr_gp_view = view(&im.corr_gp);
        let a_view = view(&im.a);
        let b_view = view(&im.b);
        let mean_a_view = view(&im.mean_a);
        let mean_b_view = view(&im.mean_b);
        let out_view = view(&out.texture);

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dehaze-transmission"),
            });

        // 1. dark channel + guide.
        let dark_bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("dehaze-dark-bind"),
                layout: &self.dark_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&dc0_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&guide_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.uniform_min.as_entire_binding(),
                    },
                ],
            });
        dispatch(
            &mut enc,
            "dehaze-dark-channel",
            &self.dark_pipeline,
            &dark_bind,
            w,
            h,
        );

        // 2. separable block-min (H then V, folding in the praw transform).
        let min_h_bind = self.plane_bind(
            &dc0_view,
            &dc_h_view,
            &self.uniform_min,
            "dehaze-min-h-bind",
        );
        dispatch(
            &mut enc,
            "dehaze-min-h",
            &self.min_h_pipeline,
            &min_h_bind,
            w,
            h,
        );
        let min_v_bind = self.plane_bind(
            &dc_h_view,
            &praw_view,
            &self.uniform_min,
            "dehaze-min-v-bind",
        );
        dispatch(
            &mut enc,
            "dehaze-min-v",
            &self.min_v_pipeline,
            &min_v_bind,
            w,
            h,
        );

        // 3. products gg = guide^2, gp = guide*praw.
        let products_bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("dehaze-products-bind"),
                layout: &self.products_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&guide_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&praw_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&gg_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&gp_view),
                    },
                ],
            });
        dispatch(
            &mut enc,
            "dehaze-products",
            &self.products_pipeline,
            &products_bind,
            w,
            h,
        );

        // 4. guided-filter box means/correlations (reusing box_h/box_v).
        self.box_filter(
            &mut enc,
            "dehaze-box-mean-g",
            &guide_view,
            &box_scratch_view,
            &mean_g_view,
            w,
            h,
        );
        self.box_filter(
            &mut enc,
            "dehaze-box-mean-p",
            &praw_view,
            &box_scratch_view,
            &mean_p_view,
            w,
            h,
        );
        self.box_filter(
            &mut enc,
            "dehaze-box-corr-g",
            &gg_view,
            &box_scratch_view,
            &corr_g_view,
            w,
            h,
        );
        self.box_filter(
            &mut enc,
            "dehaze-box-corr-gp",
            &gp_view,
            &box_scratch_view,
            &corr_gp_view,
            w,
            h,
        );

        // 5. guided-filter linear coefficients a, b.
        let guided_ab_bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("dehaze-guided-ab-bind"),
                layout: &self.guided_ab_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&mean_g_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&mean_p_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&corr_g_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&corr_gp_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&a_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&b_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: self.uniform_box.as_entire_binding(),
                    },
                ],
            });
        dispatch(
            &mut enc,
            "dehaze-guided-ab",
            &self.guided_ab_pipeline,
            &guided_ab_bind,
            w,
            h,
        );

        // 6. box filter a, b -> mean_a, mean_b (reusing box_h/box_v again).
        self.box_filter(
            &mut enc,
            "dehaze-box-mean-a",
            &a_view,
            &box_scratch_view,
            &mean_a_view,
            w,
            h,
        );
        self.box_filter(
            &mut enc,
            "dehaze-box-mean-b",
            &b_view,
            &box_scratch_view,
            &mean_b_view,
            w,
            h,
        );

        // 7. combine into the final refined transmission q.
        let guided_q_bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("dehaze-guided-q-bind"),
                layout: &self.guided_q_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&mean_a_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&mean_b_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&guide_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&out_view),
                    },
                ],
            });
        dispatch(
            &mut enc,
            "dehaze-guided-q",
            &self.guided_q_pipeline,
            &guided_q_bind,
            w,
            h,
        );

        self.ctx.queue.submit([enc.finish()]);
        out
    }
}

/// Delegating `Node` impl so a `DehazeTransmissionNode` can be shared via `Rc`
/// (see `Rc<LocalAdjustmentsNode>` for the rationale: the pipeline keeps a
/// handle for the rebuild-count test hook while a boxed clone lives in the
/// graph).
impl Node<PipelineImage> for Rc<DehazeTransmissionNode> {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        (**self).evaluate(inputs)
    }
}

/// Two-input recovery + blend node (QS-Task 3): takes the original image `I`
/// and the refined transmission `q`, and produces the recovered/haze-adjusted
/// image by blending per-pixel via the `amount` parameter. Mirrors the pure
/// `dehaze_recover` reference exactly, but takes `q` directly in the shader
/// (while the CPU reference takes `dark` derived as `(1-q)/DEHAZE_OMEGA`).
pub(crate) struct DehazeRecoveryNode {
    ctx: Arc<GpuContext>,
    params: Rc<Cell<RecoveryParams>>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    out: RefCell<Option<PipelineImage>>,
}

impl DehazeRecoveryNode {
    pub(crate) fn new(ctx: Arc<GpuContext>, params: Rc<Cell<RecoveryParams>>) -> Self {
        let device = &ctx.device;

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dehaze-recovery-uniform"),
            size: std::mem::size_of::<RecoveryParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Two-input variant: 0 = img texture, 1 = trans texture, 2 = dst storage, 3 = uniform
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dehaze-recovery-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: PIPELINE_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let module = ctx.shader_module(
            "dehaze-recovery",
            include_str!("shaders/dehaze_recovery.wgsl"),
        );
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dehaze-recovery"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("dehaze-recovery"),
            layout: Some(&layout),
            module: &module,
            entry_point: "main",
            compilation_options: Default::default(),
            cache: None,
        });

        Self {
            ctx,
            params,
            pipeline,
            bgl,
            uniform_buf,
            out: RefCell::new(None),
        }
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dehaze-recovery-out"),
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
            *out = Some(PipelineImage {
                texture: Arc::new(tex),
                width: w,
                height: h,
            });
        }
        out.as_ref().unwrap().clone()
    }
}

impl Node<PipelineImage> for DehazeRecoveryNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let img = inputs[0];
        let trans = inputs[1];
        let (w, h) = (img.width, img.height);
        let out = self.ensure_out(w, h);

        self.ctx
            .queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&self.params.get()));

        let img_view = img
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let trans_view = trans
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let out_view = out
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("dehaze-recovery-bind"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&img_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&trans_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&out_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.uniform_buf.as_entire_binding(),
                    },
                ],
            });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dehaze-recovery"),
            });

        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("dehaze-recovery"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }

        self.ctx.queue.submit([enc.finish()]);
        out
    }
}

/// Delegating `Node` impl so a `DehazeRecoveryNode` can be shared via `Rc`.
impl Node<PipelineImage> for Rc<DehazeRecoveryNode> {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        (**self).evaluate(inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dehaze::DEHAZE_OMEGA;
    use crate::nodes::upload_source;
    use crate::transmission_map;
    use crate::DEHAZE_GUIDED_EPS;
    use ferrolite_image::LinearRgbaF32;

    /// Read an `Rgba16Float` `PipelineImage`'s `.r` channel back to f32 on the
    /// CPU (test-only; mirrors `local_node.rs::read_pixels` but keeps only the
    /// channel the recovery node will consume).
    fn read_r_channel(ctx: &GpuContext, img: &PipelineImage) -> Vec<f32> {
        let (w, h) = (img.width, img.height);
        let bpp = 8u32; // RGBA16F
        let bpr_unpadded = w * bpp;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bpr_padded = bpr_unpadded.div_ceil(align) * align;
        let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dehaze-node-test-readback"),
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
        let mut out = vec![0.0f32; (w * h) as usize];
        for row in 0..h {
            let start = (row * bpr_padded) as usize;
            for x in 0..w {
                // Channel 0 (R) of each RGBA16F texel is 2 bytes.
                let o = start + (x * 4) as usize * 2;
                let hf = half::f16::from_le_bytes([data[o], data[o + 1]]);
                out[(row * w + x) as usize] = hf.to_f32();
            }
        }
        drop(data);
        buf.unmap();
        out
    }

    /// Vertical dark/bright edge fixture (mirrors the CPU reference test
    /// `guided_refinement_removes_most_of_the_block_min_halo` in `dehaze.rs`):
    /// left half 0.05, right half 0.9.
    fn edge_image(w: u32, h: u32) -> (Vec<[f32; 3]>, LinearRgbaF32) {
        let mut planar = vec![[0.0f32; 3]; (w * h) as usize];
        let mut interleaved = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.05 } else { 0.9 };
                planar[(y * w + x) as usize] = [v, v, v];
                interleaved.extend_from_slice(&[v, v, v, 1.0]);
            }
        }
        let img = LinearRgbaF32::new(w, h, interleaved).expect("edge image length");
        (planar, img)
    }

    #[test]
    fn transmission_node_matches_cpu_reference() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (64u32, 8u32);
        let (planar, gpu_img) = edge_image(w, h);
        let a = [0.9f32, 0.9, 0.9];
        let radius = 6u32;

        let src = upload_source(&ctx, &gpu_img);
        let params = Rc::new(Cell::new(TransmissionParams {
            radius: radius as i32,
            atmos: [a[0], a[1], a[2], 0.0],
            omega: DEHAZE_OMEGA,
            eps: DEHAZE_GUIDED_EPS,
            active: 1,
        }));
        let node = DehazeTransmissionNode::new(ctx.clone(), params);
        let out = node.evaluate(&[&src]);
        assert_eq!((out.width, out.height), (w, h));
        assert_eq!(
            node.transmission_rebuild_count(),
            1,
            "one evaluate = one full multi-pass rebuild"
        );

        let gpu_q = read_r_channel(&ctx, &out);
        let cpu_q = transmission_map(&planar, w as usize, h as usize, a, radius);
        assert_eq!(gpu_q.len(), cpu_q.len());

        let mut max_d = 0.0f32;
        for (g, c) in gpu_q.iter().zip(cpu_q.iter()) {
            max_d = max_d.max((g - c).abs());
        }
        eprintln!("transmission_node_matches_cpu_reference: max abs diff = {max_d}");
        assert!(
            max_d < 2e-2,
            "GPU transmission drifted from CPU reference: max abs diff {max_d}"
        );
    }

    /// Read all four RGBA channels as f32 (for recovery node test).
    fn read_rgba_channels(ctx: &GpuContext, img: &PipelineImage) -> Vec<[f32; 4]> {
        let (w, h) = (img.width, img.height);
        let bpp = 8u32; // RGBA16F
        let bpr_unpadded = w * bpp;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bpr_padded = bpr_unpadded.div_ceil(align) * align;
        let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dehaze-recovery-test-readback"),
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
                // Each RGBA16F texel is 8 bytes (4 channels × 2 bytes).
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

    #[test]
    fn recovery_node_matches_dehaze_recover() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (16u32, 16u32);

        // Simple test image: grey, all pixels same.
        let mut img_pixels = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            img_pixels.extend_from_slice(&[0.5f32, 0.5, 0.5, 1.0]);
        }
        let img = LinearRgbaF32::new(w, h, img_pixels).expect("test image");
        let gpu_img = upload_source(&ctx, &img);

        // Constant transmission texture: fill with a constant q value.
        // We'll test with q=0.5 (bright), q=0.8 (foggy), and q=0.3 (dark).
        let test_cases = vec![
            (0.5f32, "mid-transmission"),
            (0.8f32, "high-transmission"),
            (0.3f32, "low-transmission"),
        ];

        let a = [0.9f32, 0.9, 0.9];
        const DEHAZE_T0: f32 = 0.1;
        use crate::dehaze::dehaze_recover;

        for (q_val, case_name) in test_cases {
            // Create constant transmission texture.
            let mut trans_pixels = Vec::with_capacity((w * h * 4) as usize);
            for _ in 0..(w * h) {
                trans_pixels.extend_from_slice(&[q_val, q_val, q_val, 1.0]);
            }
            let trans_img = LinearRgbaF32::new(w, h, trans_pixels).expect("transmission image");
            let gpu_trans = upload_source(&ctx, &trans_img);

            // Test amount = 0 (identity), positive, and negative.
            for amount in [0.0f32, 0.5, -0.5] {
                let params = Rc::new(Cell::new(RecoveryParams {
                    amount,
                    t0: DEHAZE_T0,
                    pad0: 0.0,
                    pad1: 0.0,
                    atmos: [a[0], a[1], a[2], 0.0],
                }));

                let node = DehazeRecoveryNode::new(ctx.clone(), params);
                let gpu_out = node.evaluate(&[&gpu_img, &gpu_trans]);

                let gpu_result = read_rgba_channels(&ctx, &gpu_out);

                // Reference: for each pixel, compute the expected output.
                // The CPU reference dehaze_recover takes dark = (1 - q) / DEHAZE_OMEGA,
                // while the GPU shader takes q directly.
                let orig_px = [0.5f32, 0.5, 0.5];
                let dark = (1.0 - q_val) / DEHAZE_OMEGA;
                let expected = dehaze_recover(orig_px, dark, a, amount);

                // Compare all pixels (they should all be identical since input is constant).
                for (i, &gpu_px) in gpu_result.iter().enumerate() {
                    let gpu_rgb = [gpu_px[0], gpu_px[1], gpu_px[2]];
                    for c in 0..3 {
                        let diff = (gpu_rgb[c] - expected[c]).abs();
                        assert!(
                            diff < 2e-3,
                            "recovery_node_matches_dehaze_recover ({}, amount={}, pixel {}, channel {}):\n\
                             GPU={:.6}, CPU={:.6}, diff={:.6}",
                            case_name, amount, i, c, gpu_rgb[c], expected[c], diff
                        );
                    }
                    // Alpha should pass through unchanged.
                    assert!(
                        (gpu_px[3] - 1.0).abs() < 1e-6,
                        "alpha mismatch at pixel {}",
                        i
                    );
                }
                eprintln!(
                    "recovery_node_matches_dehaze_recover: {} amount={} PASS",
                    case_name, amount
                );
            }
        }
    }
}
