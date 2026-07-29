//! `DehazeTransmissionNode` — the refined dehaze transmission map as a
//! multi-pass `Node<PipelineImage>`. Mirrors `crate::dehaze::transmission_map`
//! (separable dark-channel block-min + guided-filter refinement) exactly, on
//! the GPU. Independent of `amount`: the `Graph`'s dirty-caching means an
//! amount-only drag never re-triggers this node's (relatively) expensive
//! multi-pass evaluate — Phase 4 Task 2 fused the amount/atmos recovery+blend
//! step directly into `local_node.rs`'s Color-stage engine node (the retired
//! `DehazeRecoveryNode` used to be a separate cheap single-pass node here;
//! see that node's git history / `dehaze_recovery.wgsl`, kept in-tree as
//! reference math for `local_adjust.wgsl`'s port), so this file now only
//! computes the transmission map.
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
//! Wired into `EditPipeline` (QS-Task 4; whole-image preview tier) AND
//! `TileEditPipeline` (QS-Task 5; tiled full-res tier), both fed from the
//! Light-stage engine node's output — the old single-pass `dehaze.wgsl`
//! `PointOpNode` is gone.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Node};

use crate::dehaze::{
    guided_radius, transmission_mip_level_count, transmission_working_dims, DEHAZE_ATMOS_MIN,
    DEHAZE_GUIDED_EPS, DEHAZE_OMEGA,
};
use crate::image::{PipelineImage, PIPELINE_FORMAT};
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
/// GLOBAL op's block-min patch radius (`Dehaze::radius`, UNCLAMPED — the node
/// defensively clamps to `MAX_DEHAZE_RADIUS` before use, since a prior review
/// noted the pure `transmission_map`/its loops don't self-clamp). `atmos` is
/// `[r,g,b,pad]` (floored to `DEHAZE_ATMOS_MIN` by `from_stack`). `omega`/`eps`
/// mirror `DEHAZE_OMEGA`/`DEHAZE_GUIDED_EPS`. `active` is 1 when dehaze is
/// active ANYWHERE in the document (global op OR any visible mask layer's
/// amount — Phase 4 Task 3, see `EditDoc::dehaze_active_anywhere`), else 0 —
/// see `from_stack` and `DehazeTransmissionNode::evaluate`'s early-return
/// gate. CRITICAL: `active` deliberately does NOT carry any amount's
/// magnitude — it only flips on the zero<->nonzero transition, so an
/// amount-only drag (0.5 -> 0.9, global or per-mask) leaves this whole struct
/// unchanged and `EditPipeline::set_stack` does not dirty this node (see
/// `amount_change_does_not_recompute_transmission`).
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
    /// Seed from the FULL document (Phase 4 Task 3) — not just the global
    /// `Dehaze` op — since per-mask dehaze amounts share this ONE whole-image
    /// transmission map too: `active` is true when the global op is active OR
    /// any visible mask layer's `dehaze.amount != 0.0` (see
    /// `EditDoc::dehaze_active_anywhere`), so the map is computed even when
    /// the global amount is 0. `radius` always comes from the GLOBAL op's
    /// radius field — per-mask radius is not exposed; every layer recovers
    /// from the SAME shared map — and `AdjustmentSet`'s `Dehaze::default()`
    /// already carries `DEHAZE_DEFAULT_RADIUS`, so a stack with no global op
    /// (or one whose amount is 0) still seeds a sane default radius rather
    /// than 0. `radius`/`atmos` are independent of `active`'s magnitude
    /// (QS-Task 4): `EditPipeline::set_stack` only rebuilds these (and dirties
    /// `DehazeTransmissionNode`) when `radius`, `atmos`, or the active
    /// zero<->nonzero transition actually changes, so an amount-magnitude-only
    /// drag (global OR per-mask) never re-seeds (and never dirties) this node.
    pub(crate) fn from_stack(stack: &crate::op::OpStack, atmos: [f32; 3]) -> Self {
        Self {
            radius: stack.global.dehaze.radius as i32,
            atmos: [
                atmos[0].max(DEHAZE_ATMOS_MIN),
                atmos[1].max(DEHAZE_ATMOS_MIN),
                atmos[2].max(DEHAZE_ATMOS_MIN),
                0.0,
            ],
            omega: DEHAZE_OMEGA,
            eps: DEHAZE_GUIDED_EPS,
            active: u32::from(stack.dehaze_active_anywhere()),
        }
    }
}

/// Every bind group one full transmission evaluate dispatches, prebuilt ONCE
/// and reused until the input texture identity, the output texture, or the
/// working dims change (profiled: rebuilding all ~17 bind groups + ~16 texture
/// views per evaluate was the pipeline's single largest CPU-encode cost on an
/// exposure drag with dehaze active). Everything referenced is persistent: the
/// `Intermediates` planes, the two uniform buffers (whose CONTENTS are still
/// written fresh via `queue.write_buffer` each evaluate — cached binds don't
/// pin stale params), the sampler, and the cached `out` texture's mip views.
struct CachedBinds {
    /// `Arc::as_ptr` identity of the source texture the `dark` bind samples.
    src_ptr: usize,
    /// `Arc::as_ptr` identity of the `out` texture the q/mip binds write.
    out_ptr: usize,
    /// Working dims the `Intermediates` these binds reference were built for.
    dims: (u32, u32),
    dark: wgpu::BindGroup,
    min_h: wgpu::BindGroup,
    min_v: wgpu::BindGroup,
    products: wgpu::BindGroup,
    /// `(h, v)` bind pair per guided-filter box run, in dispatch order:
    /// `mean_g, mean_p, corr_g, corr_gp, mean_a, mean_b`.
    boxes: [(wgpu::BindGroup, wgpu::BindGroup); 6],
    guided_ab: wgpu::BindGroup,
    guided_q: wgpu::BindGroup,
    /// One bind per mip level 1..N (reads level-1, writes level).
    mips: Vec<wgpu::BindGroup>,
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

/// Filtering-sampler bind-group-layout entry, for passes that bilinearly
/// upsample/downsample a plane rather than `textureLoad` it 1:1 (the dehaze
/// dark-channel downsample and recovery's transmission upsample).
fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

/// Linear, clamp-to-edge sampler shared by the passes that bilinearly sample a
/// texture at a normalized UV (built once in each node's `new`, never per-evaluate).
/// `pub(crate)`: also reused by `local_node.rs`'s Color-stage engine node
/// (Phase 4 Task 2 — the fused dehaze recovery samples the shared transmission
/// bilinearly, mirroring the recovered approach).
pub(crate) fn linear_clamp_sampler(ctx: &GpuContext) -> wgpu::Sampler {
    ctx.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("dehaze-linear-clamp-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        // Trilinear across mips: the recovery samples the mip-mapped shared
        // transmission at an explicit LOD (`transmission_sample_lod`) so a
        // zoomed-out tile fetches a band-limited level instead of aliasing the
        // base map. Harmless for the transmission node's own `src` downsample
        // (single-mip input, sampled at level 0 only).
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
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

/// A single-mip-level view of `tex` (a storage bind — and the read side of the
/// mip-downsample chain — must target exactly one level, never the default
/// all-levels view).
fn mip_view(tex: &wgpu::Texture, level: u32) -> wgpu::TextureView {
    tex.create_view(&wgpu::TextureViewDescriptor {
        base_mip_level: level,
        mip_level_count: Some(1),
        ..Default::default()
    })
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
    // Linear, clamp-to-edge sampler for the dark-channel pass's downsample of
    // `src` (full res) into the working-res `dc0`/`guide` planes. Built once
    // here, never per-evaluate (CLAUDE.md GPU rule).
    sampler: wgpu::Sampler,

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

    // Mip-chain downsample (LOD fix): builds transmission mip levels 1..N from
    // level 0 (a 2x2 box average per level) so the tiled recovery can sample a
    // band-limited level when zoomed out past fit. Built once; run in a small
    // loop at the tail of `evaluate`, inside the same command buffer.
    mip_bgl: wgpu::BindGroupLayout,
    mip_pipeline: wgpu::ComputePipeline,

    intermediates: RefCell<Option<Intermediates>>,
    out: RefCell<Option<PipelineImage>>,
    // Prebuilt bind groups for one full evaluate (see `CachedBinds`), rebuilt
    // only when the source/out texture identity or the working dims change.
    binds: RefCell<Option<CachedBinds>>,
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
        let sampler = linear_clamp_sampler(&ctx);

        let dark_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dehaze-dark-bgl"),
            entries: &[
                texture_entry(0, true),             // src rgba16float (sampled, filterable)
                storage_out_entry(1, PLANE_FORMAT), // dc0 (working res)
                storage_out_entry(2, PLANE_FORMAT), // guide (working res)
                uniform_entry(3),
                sampler_entry(4), // linear, clamp-to-edge: downsamples src -> working res
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

        let mip_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dehaze-transmission-mip-bgl"),
            entries: &[
                texture_entry(0, false),               // previous mip level (textureLoad)
                storage_out_entry(1, PIPELINE_FORMAT), // next mip level
            ],
        });
        let mip_pipeline = compute_pipeline(
            &ctx,
            &mip_bgl,
            "dehaze-transmission-mip",
            include_str!("shaders/dehaze_transmission_mip.wgsl"),
        );

        Self {
            ctx,
            params,
            uniform_min,
            uniform_box,
            sampler,
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
            mip_bgl,
            mip_pipeline,
            intermediates: RefCell::new(None),
            out: RefCell::new(None),
            binds: RefCell::new(None),
            rebuilds: Cell::new(0),
        }
    }

    /// Number of full multi-pass evaluates (test hook; see the field doc).
    #[cfg(test)]
    pub(crate) fn transmission_rebuild_count(&self) -> u32 {
        self.rebuilds.get()
    }

    /// The current whole-image dehaze transmission texture (source space, bounded
    /// to DEHAZE_MAX_TRANSMISSION_DIM), or None when dehaze is inactive. The cached
    /// `out` texture's `Arc` is returned when the last evaluate ran the passes
    /// (dehaze active, `active == 1`); when inactive (early-return path), returns
    /// `None` to reflect that the cached `out` was never populated.
    pub(crate) fn current_output_texture(&self) -> Option<Arc<wgpu::Texture>> {
        let params = self.params.get();
        if params.active != 0 {
            self.out.borrow().as_ref().map(|p| p.texture.clone())
        } else {
            None
        }
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
                // Full mip chain (LOD fix): level 0 holds the guided-filter
                // result; levels 1..N are 2x2 box downsamples generated at the
                // tail of `evaluate`, so the recovery can sample a band-limited
                // level when the display LOD is coarser than this map.
                mip_level_count: transmission_mip_level_count(w, h),
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

    /// Build (or reuse) the full evaluate's bind-group set (see `CachedBinds`).
    /// Rebuilds only when the source texture identity, the `out` texture, or
    /// the working dims changed — the steady state of a slider drag reuses the
    /// whole set with zero view/bind-group creation.
    fn ensure_binds(&self, src: &PipelineImage, out: &PipelineImage) {
        let src_ptr = Arc::as_ptr(&src.texture) as usize;
        let out_ptr = Arc::as_ptr(&out.texture) as usize;
        let intermediates = self.intermediates.borrow();
        let im = intermediates.as_ref().expect("intermediates allocated");
        {
            let cur = self.binds.borrow();
            if matches!(&*cur, Some(b) if b.src_ptr == src_ptr
                && b.out_ptr == out_ptr
                && b.dims == im.dims)
            {
                return;
            }
        }

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
        // Level 0 only: `out` carries a mip chain, and a storage bind must
        // target a single level. Levels 1..N get their own binds below.
        let out_view = mip_view(&out.texture, 0);

        let dark = self
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
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
        let min_h = self.plane_bind(
            &dc0_view,
            &dc_h_view,
            &self.uniform_min,
            "dehaze-min-h-bind",
        );
        let min_v = self.plane_bind(
            &dc_h_view,
            &praw_view,
            &self.uniform_min,
            "dehaze-min-v-bind",
        );
        let products = self
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
        // (input, output) per guided-filter box run, in dispatch order; each
        // becomes an (input->scratch H, scratch->output V) bind pair.
        let box_io: [(&wgpu::TextureView, &wgpu::TextureView, &str); 6] = [
            (&guide_view, &mean_g_view, "dehaze-box-mean-g"),
            (&praw_view, &mean_p_view, "dehaze-box-mean-p"),
            (&gg_view, &corr_g_view, "dehaze-box-corr-g"),
            (&gp_view, &corr_gp_view, "dehaze-box-corr-gp"),
            (&a_view, &mean_a_view, "dehaze-box-mean-a"),
            (&b_view, &mean_b_view, "dehaze-box-mean-b"),
        ];
        let boxes = box_io.map(|(input, output, label)| {
            (
                self.plane_bind(
                    input,
                    &box_scratch_view,
                    &self.uniform_box,
                    &format!("{label}-h-bind"),
                ),
                self.plane_bind(
                    &box_scratch_view,
                    output,
                    &self.uniform_box,
                    &format!("{label}-v-bind"),
                ),
            )
        });
        let guided_ab = self
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
        let guided_q = self
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
        let mips = (1..out.texture.mip_level_count())
            .map(|level| {
                let src_mip = mip_view(&out.texture, level - 1);
                let dst_mip = mip_view(&out.texture, level);
                self.ctx
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("dehaze-transmission-mip-bind"),
                        layout: &self.mip_bgl,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&src_mip),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(&dst_mip),
                            },
                        ],
                    })
            })
            .collect();

        *self.binds.borrow_mut() = Some(CachedBinds {
            src_ptr,
            out_ptr,
            dims: im.dims,
            dark,
            min_h,
            min_v,
            products,
            boxes,
            guided_ab,
            guided_q,
            mips,
        });
    }
}

impl Node<PipelineImage> for DehazeTransmissionNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let raw = self.params.get();

        // Dehaze is off (no `Dehaze` op, or `amount == 0`): the Color-stage
        // engine node's fused recovery ignores this node's output entirely in
        // that case (passthrough), so running the ~8-pass guided filter here
        // would be pure waste. This is the QS-Task-4 regression fix — `amount`'s
        // magnitude is deliberately NOT part of `TransmissionParams` (see its doc),
        // so this gate only flips on the zero<->nonzero transition, preserving
        // the amount-drag cache proven by `amount_change_does_not_recompute_transmission`.
        // Cloning `PipelineImage` is an `Arc` clone (cheap); no compute passes
        // run and the rebuild-count test hook is NOT bumped.
        if raw.active == 0 {
            return src.clone();
        }

        // Compute the transmission at a capped WORKING resolution (not the
        // input dims) — the transmission map is low-frequency, so this bounds
        // the fifteen `R32Float` intermediate planes' VRAM regardless of the
        // full input size (the QS-Task fix for the full-res preview-tier OOM).
        // The Color-stage engine node's fused recovery upsamples this smaller
        // `out` back to the image resolution via a bilinear sample.
        let (w, h) = (src.width, src.height);
        let (ww, wh, scale) = transmission_working_dims(w, h);
        self.ensure_intermediates(ww, wh);
        let out = self.ensure_out(ww, wh);
        self.rebuilds.set(self.rebuilds.get() + 1);

        // Radii are defined in FULL-RES pixels (`Dehaze::radius`); scale them
        // down to working-res pixels so the patch covers the same image
        // fraction there as it would at 1:1 (clamped to >=1 px). At scale==1
        // this is `radius`/`gr` unchanged.
        let radius_full = (raw.radius.max(0) as u32).min(MAX_DEHAZE_RADIUS);
        // Cap mirrors `guided_radius`'s own multiplier (5r, widened from 3r —
        // see that fn's doc) so this defensive `.min` doesn't silently clip the
        // widened window back down to the old 3r for radii near
        // `MAX_DEHAZE_RADIUS`; it's still a real bound in case `guided_radius`
        // ever grows the multiplier further than this call site expects.
        let gr_full = guided_radius(radius_full).min(MAX_DEHAZE_RADIUS.saturating_mul(5));
        let scale_down = |r: u32| -> i32 { ((r as f32 / scale as f32).round() as i32).max(1) };
        let radius = scale_down(radius_full);
        let gr = scale_down(gr_full);

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

        // Prebuilt bind groups (rebuilt only on source/out/dims change — see
        // `CachedBinds`); the uniform CONTENTS above are refreshed every
        // evaluate regardless, so cached binds never pin stale params.
        self.ensure_binds(src, &out);
        let binds = self.binds.borrow();
        let b = binds.as_ref().expect("built above");

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dehaze-transmission"),
            });

        // 1. dark channel + guide.
        dispatch(
            &mut enc,
            "dehaze-dark-channel",
            &self.dark_pipeline,
            &b.dark,
            ww,
            wh,
        );

        // 2. separable block-min (H then V, folding in the praw transform).
        dispatch(
            &mut enc,
            "dehaze-min-h",
            &self.min_h_pipeline,
            &b.min_h,
            ww,
            wh,
        );
        dispatch(
            &mut enc,
            "dehaze-min-v",
            &self.min_v_pipeline,
            &b.min_v,
            ww,
            wh,
        );

        // 3. products gg = guide^2, gp = guide*praw.
        dispatch(
            &mut enc,
            "dehaze-products",
            &self.products_pipeline,
            &b.products,
            ww,
            wh,
        );

        // 4. guided-filter box means/correlations (mean_g, mean_p, corr_g,
        // corr_gp — `boxes[..4]`), each an H pass into the shared scratch then
        // a V pass into its output plane.
        for (h_bind, v_bind) in &b.boxes[..4] {
            dispatch(
                &mut enc,
                "dehaze-box-h",
                &self.box_h_pipeline,
                h_bind,
                ww,
                wh,
            );
            dispatch(
                &mut enc,
                "dehaze-box-v",
                &self.box_v_pipeline,
                v_bind,
                ww,
                wh,
            );
        }

        // 5. guided-filter linear coefficients a, b.
        dispatch(
            &mut enc,
            "dehaze-guided-ab",
            &self.guided_ab_pipeline,
            &b.guided_ab,
            ww,
            wh,
        );

        // 6. box filter a, b -> mean_a, mean_b (`boxes[4..]`).
        for (h_bind, v_bind) in &b.boxes[4..] {
            dispatch(
                &mut enc,
                "dehaze-box-h",
                &self.box_h_pipeline,
                h_bind,
                ww,
                wh,
            );
            dispatch(
                &mut enc,
                "dehaze-box-v",
                &self.box_v_pipeline,
                v_bind,
                ww,
                wh,
            );
        }

        // 7. combine into the final refined transmission q.
        dispatch(
            &mut enc,
            "dehaze-guided-q",
            &self.guided_q_pipeline,
            &b.guided_q,
            ww,
            wh,
        );

        // 8. Build the mip chain (LOD fix): each level is a 2x2 box downsample
        // of the one above. wgpu inserts the storage-write -> texture-read
        // barrier between the guided-q write of level 0 and the first read
        // here, and between each successive level, since they are distinct
        // subresources of the same texture within this one command buffer.
        let (mut lw, mut lh) = (ww, wh);
        for mip_bind in &b.mips {
            let dw = (lw / 2).max(1);
            let dh = (lh / 2).max(1);
            dispatch(
                &mut enc,
                "dehaze-transmission-mip",
                &self.mip_pipeline,
                mip_bind,
                dw,
                dh,
            );
            lw = dw;
            lh = dh;
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dehaze::DEHAZE_OMEGA;
    use crate::nodes::upload_source;
    use crate::transmission_map;
    use crate::{DEHAZE_DEFAULT_RADIUS, DEHAZE_GUIDED_EPS, DEHAZE_MAX_TRANSMISSION_DIM};
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

    /// Regression guard for the full-res preview-tier OOM (QS-Task fix): a
    /// moderately large input (big enough to force `scale >= 2`, cheap enough
    /// to allocate in a test) must NOT allocate the fifteen intermediate planes
    /// (nor the output `q`) at the input's full resolution. Before the fix,
    /// `DehazeTransmissionNode` allocated all fifteen `R32Float` planes at
    /// (3200, 2400) — this asserts the node's actual output dims are capped at
    /// `DEHAZE_MAX_TRANSMISSION_DIM` instead.
    #[test]
    fn large_input_transmission_is_capped() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (3200u32, 2400u32);
        // Flat mid-grey image: only the dims matter for this test, so a flat
        // fill avoids building/uploading a structured fixture at this size.
        let pixels = vec![0.5f32; (w * h * 4) as usize];
        let img = LinearRgbaF32::new(w, h, pixels).expect("flat fixture");
        let src = upload_source(&ctx, &img);

        let params = Rc::new(Cell::new(TransmissionParams {
            radius: DEHAZE_DEFAULT_RADIUS as i32,
            atmos: [0.9, 0.9, 0.9, 0.0],
            omega: DEHAZE_OMEGA,
            eps: DEHAZE_GUIDED_EPS,
            active: 1,
        }));
        let node = DehazeTransmissionNode::new(ctx.clone(), params);
        let out = node.evaluate(&[&src]);

        assert!(
            out.width <= DEHAZE_MAX_TRANSMISSION_DIM && out.height <= DEHAZE_MAX_TRANSMISSION_DIM,
            "transmission output must be capped at {DEHAZE_MAX_TRANSMISSION_DIM}px, got {}x{}",
            out.width,
            out.height
        );
        assert!(
            out.width < w && out.height < h,
            "a {w}x{h} input must actually be downsampled, not just clamped to itself"
        );
    }
}
