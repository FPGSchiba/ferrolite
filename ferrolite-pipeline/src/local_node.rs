//! `LocalAdjustmentsNode` — the fused Light/Color engine, one instance per
//! `EngineStage` (see that type's doc). Per visible layer: (engine) composite
//! the `MaskDefinition` into a single `MaskBuffer`, then (photo) apply the
//! Light+Color point op blended by the mask. The `Light`-stage instance sits at
//! the old exposure position (before dehaze); the `Color`-stage instance sits
//! at the old tone-curve…local-adjust position (before `Sharpen`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Node};
use ferrolite_mask::{MaskBuffer, MaskCompositor, RasterStore, TileTransform};

use crate::dehaze::DEHAZE_ATMOS_MIN;
use crate::dehaze_node::linear_clamp_sampler;
use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::local::{AdjustmentSet, LocalAdjustments};
use crate::nodes::TileFrame;
use crate::op::Dehaze;
use crate::uniforms::{local_adjust_uniform, local_layer_lut, GeometryUniform, LocalAdjustUniform};

/// Which fused-engine pass this node instance is. `Light` runs once, at the
/// exposure position (before dehaze), applying only the global set's
/// `light_segment()` in global order with full coverage — no mask compositing
/// at all. `Color` runs at the old tone-curve…local-adjust position: first the
/// global set's `color_segment()` (global order, full coverage) as a pseudo-
/// layer, then the existing per-mask-layer loop (its mask compositing keys off
/// the pseudo-layer's output — see `evaluate_color`'s doc). `EditPipeline` and
/// `TileEditPipeline` each construct one `Light`-stage and one `Color`-stage
/// instance via `new_engine`, sharing one `Rc<RefCell<AdjustmentSet>>` between
/// them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EngineStage {
    Light,
    Color,
}

struct CachedMasks {
    // Keyed on the mask DEFINITIONS only (not the adjustments) so an
    // adjustment-only change (exposure/contrast/...) reuses the cached
    // composited masks instead of re-compositing at full resolution.
    mask_defs: Vec<ferrolite_mask::MaskDefinition>,
    dims: (u32, u32),
    // Phase 3 (Task 3 binding ruling): masks now composite against `current`
    // (post-global-color-segment), not the node's raw `input` — a content-
    // dependent range component (Luma/Color) can therefore select different
    // pixels when the global color segment changes even though no mask
    // DEFINITION did. `Some(seg)` pins the color-segment params the cache was
    // built against, and is only populated when at least one visible layer's
    // mask actually has a content-dependent component (see
    // `has_content_dependent_component`) — purely spatial masks (gradient/
    // radial/brush) don't care what color the pixels are, so they keep the
    // original defs+dims-only cache key (and its adjustment-only-change reuse
    // guarantee) untouched. Upstream light-segment (the `Light`-stage node) and
    // dehaze changes also alter what `current` contains at this node, but are
    // deliberately NOT folded into this key: that preserves the pre-fusion
    // stale-mask-across-upstream-edit semantics (an upstream-only edit reused
    // the old composited mask rather than re-compositing against the new
    // content), and applies to the whole-image path only — the tiled path
    // (`use_cache == false` below) composites fresh on every evaluate, so it
    // never goes stale regardless.
    color_seg_key: Option<AdjustmentSet>,
    masks: Vec<MaskBuffer>, // one per visible layer, in visible order
}

/// True when any component of `def` samples the underlying pixel content
/// (`LumaRange`/`ColorRange`) rather than being purely spatial (gradient/
/// radial/brush/imported). Content-dependent components composite against
/// whatever image they're given — see `evaluate_color`'s doc for why that
/// image is `current` (post-global-color-segment), and `CachedMasks::
/// color_seg_key` for why the compositing cache must key on the color segment
/// too when this is true.
fn has_content_dependent_component(def: &ferrolite_mask::MaskDefinition) -> bool {
    def.components.iter().any(|(c, _)| {
        matches!(
            c,
            ferrolite_mask::MaskComponent::LumaRange { .. }
                | ferrolite_mask::MaskComponent::ColorRange { .. }
        )
    })
}

/// Phase 4 Task 2: the base (amount/atmos) params for the dehaze recovery now
/// fused into the Color-stage engine node's global pseudo-layer. Read from a
/// shared `Cell` each `evaluate` (mirrors the retired `DehazeRecoveryNode`'s
/// `RecoveryParams`, minus `t0` — hardcoded as a WGSL const — and the
/// geometry/frame/has_transmission fields, which live on THIS node's own
/// private `dehaze_geometry`/`dehaze_frame`/`dehaze_has_transmission` instead
/// (see those fields' docs) so a `set_stack`-driven reseed of this shared cell
/// never clobbers them.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct ColorDehazeParams {
    pub amount: f32,
    pub atmos: [f32; 4],
}

impl ColorDehazeParams {
    /// Seed from the op's `amount` and the whole-image atmospheric light
    /// (mirrors the retired `RecoveryParams::from_op` exactly, minus the
    /// inert geometry/frame fields that struct also carried).
    pub(crate) fn from_op(op: Option<Dehaze>, atmos: [f32; 3]) -> Self {
        Self {
            amount: op.map(|d| d.amount).unwrap_or(0.0),
            atmos: [
                atmos[0].max(DEHAZE_ATMOS_MIN),
                atmos[1].max(DEHAZE_ATMOS_MIN),
                atmos[2].max(DEHAZE_ATMOS_MIN),
                0.0,
            ],
        }
    }
}

impl Default for ColorDehazeParams {
    /// Identity: `amount == 0.0` gates the whole recovery step off regardless
    /// of `atmos`'s (harmless placeholder) content.
    fn default() -> Self {
        Self {
            amount: 0.0,
            atmos: [DEHAZE_ATMOS_MIN; 4],
        }
    }
}

/// Output→source geometry mapping for the fused dehaze recovery step, held
/// internally by `LocalAdjustmentsNode` (set via `set_geometry`, merged into
/// the uniform at each `evaluate_color`). Mirrors the retired
/// `DehazeRecoveryNode`'s `RecoveryGeometry` exactly — kept OUT of the
/// pipeline-visible `ColorDehazeParams` cell so a `set_stack`-driven reseed of
/// that cell can never clobber it.
#[derive(Clone, Copy)]
struct EngineDehazeGeometry {
    m: [f32; 4],
    off: [f32; 2],
    src_dims: [f32; 2],
    out_dims: [f32; 2],
}

impl Default for EngineDehazeGeometry {
    /// Identity mapping over a nominal 1×1 source. Harmless even before the
    /// first `set_geometry` call: `has_transmission` (default false, from the
    /// neutral-texture fallback) gates the recovery step, so this mapping is
    /// never actually used for real math in that state.
    fn default() -> Self {
        Self {
            m: [1.0, 0.0, 0.0, 1.0],
            off: [0.0, 0.0],
            src_dims: [1.0, 1.0],
            out_dims: [1.0, 1.0],
        }
    }
}

pub(crate) struct LocalAdjustmentsNode {
    ctx: Arc<GpuContext>,
    layers: Rc<RefCell<LocalAdjustments>>,
    // Phase 3 (fused layer engine): which pass this node instance runs, and the
    // shared global `AdjustmentSet` its pseudo-layer(s) apply from. See
    // `EngineStage`'s doc for the per-stage dispatch shape.
    stage: EngineStage,
    global_set: Rc<RefCell<AdjustmentSet>>,
    // A 1x1 mask bound for full-coverage (pseudo-layer) dispatches. Its CONTENT
    // is irrelevant — the shader's coverage flag skips the mask fetch entirely
    // for these dispatches (see `local_adjust.wgsl`'s `main`) — it exists only
    // so the bind group has a structurally valid binding 1.
    full_coverage_mask: MaskBuffer,
    // build-once mask compositing (shared source of truth w/ the UI overlay)
    compositor: MaskCompositor,
    // apply pass
    apply_bgl: wgpu::BindGroupLayout,
    apply_pipeline: wgpu::ComputePipeline,
    // A/B ping-pong output textures. Two (not one) are required: within a single
    // `evaluate`, `apply` is called once per visible layer, chaining
    // `current = apply(&current, ...)`. If the read (input) and write (dst)
    // texture were ever the same texture, the compute shader would bind it
    // simultaneously as a sampled `texture_2d` (binding 0) and a write-only
    // `texture_storage_2d` (binding 2) in one dispatch — wgpu validation panics
    // on that usage conflict. With two cached buffers, `ensure_out` always
    // picks whichever of A/B is NOT the current `input` (by `Arc::ptr_eq` on the
    // underlying texture), so read-tex != write-tex on every dispatch regardless
    // of layer count. Two buffers suffice (no full pool needed) because within
    // one `evaluate` the ping-pong only ever needs to look one step back, and the
    // `Graph` executor fully finishes this node's `evaluate` (producing one
    // final `current`) before feeding it to the next node (Sharpen); this node's
    // `apply_out` slots are not read again until the *next* `evaluate`, by which
    // time any previously-returned `current` has already been consumed
    // downstream in that same call.
    apply_out: RefCell<Option<[PipelineImage; 2]>>,
    // Persistent per-dispatch (uniform, LUT) buffer pairs, reused across
    // evaluates via `queue.write_buffer` instead of a fresh `create_buffer_init`
    // pair per `apply` (profiled: the per-evaluate allocation churn was pure
    // CPU-encode overhead). A POOL (not one pair) because a single `evaluate`
    // calls `apply` once per pseudo-layer/mask layer, and each dispatch needs
    // its own live contents: slot `i` serves the i-th `apply` of the current
    // evaluate (`apply_buf_cursor`, reset at the top of `evaluate`), so the
    // writes for a later dispatch can never clobber an earlier one — even if
    // the per-apply submits are ever batched into one. Grows to the max layer
    // count seen; entries are `LocalAdjustUniform`-sized + 3 KiB LUT each.
    apply_bufs: RefCell<Vec<(wgpu::Buffer, wgpu::Buffer)>>,
    apply_buf_cursor: std::cell::Cell<usize>,
    // tile-tier placement: None = whole-image (cached, identity); Some = tiled
    // (composite fresh at input dims with this placement so range components
    // sample the tile's own content and spatial components map to full-image uv).
    tile: RefCell<Option<TileTransform>>,
    cache: RefCell<Option<CachedMasks>>,
    // Test hook: counts mask-composite rebuilds (proves adjustment-only
    // changes reuse the cache instead of re-compositing).
    rebuilds: std::cell::Cell<u32>,
    // Test hook: counts `evaluate` calls (proves the graph's dirty-tracking
    // skips a stage whose relevant segment/layers didn't change — e.g. a
    // color-segment-only edit must not tick the Light-stage node's counter).
    evals: std::cell::Cell<u32>,
    // Phase 4 Task 2: dehaze recovery fused into the Color-stage global
    // pseudo-layer (see `evaluate_color`). Present on every instance (incl.
    // the Light stage) for a uniform type, but only ever populated/read for
    // the Color stage — the Light-stage constructor is handed fresh,
    // never-mutated placeholders (mirrors `layers`' own placeholder pattern).
    dehaze_params: Rc<Cell<ColorDehazeParams>>,
    dehaze_geometry: Cell<EngineDehazeGeometry>,
    // Shared with the tile pipeline's `GeometryHeadNode`/`VignetteNode` (the
    // head writes this each evaluate); a dedicated default-origin `Rc` on the
    // whole-image tier (no per-tile frame there) — mirrors the retired
    // `DehazeRecoveryNode`'s `frame` field exactly.
    dehaze_frame: Rc<Cell<TileFrame>>,
    // Linear, clamp-to-edge sampler for the shared transmission's source-UV
    // sample. Built once here, never per-evaluate (CLAUDE.md GPU rule).
    dehaze_sampler: wgpu::Sampler,
    // 1x1 neutral fallback (source-space) transmission so the `apply` bind
    // group always validates before `set_shared_transmission` is ever called
    // (or after it is cleared back to `None`); `dehaze_has_transmission` is
    // false in that state, so the shader's recovery step is a no-op
    // regardless of this texture's (unused) content.
    dehaze_neutral_tex: Arc<wgpu::Texture>,
    dehaze_shared_tex: RefCell<Arc<wgpu::Texture>>,
    dehaze_shared_view: RefCell<wgpu::TextureView>,
    dehaze_has_transmission: Cell<bool>,
}

impl LocalAdjustmentsNode {
    /// Full constructor: which fused-engine `stage` this node instance runs,
    /// plus the shared `global_set` its pseudo-layer(s) read from (see
    /// `EngineStage`'s doc for the per-stage dispatch shape).
    pub(crate) fn new_engine(
        ctx: Arc<GpuContext>,
        layers: Rc<RefCell<LocalAdjustments>>,
        stage: EngineStage,
        global_set: Rc<RefCell<AdjustmentSet>>,
        // Phase 4 Task 2: dehaze recovery state (see the field docs). The Light
        // stage never reads these (see `evaluate_light`) — callers pass fresh,
        // never-mutated placeholders for that instance, mirroring `layers`'
        // own placeholder pattern for the Light stage.
        dehaze_params: Rc<Cell<ColorDehazeParams>>,
        dehaze_frame: Rc<Cell<TileFrame>>,
    ) -> Self {
        let apply_bgl = ctx
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("local-adjust-bgl"),
                entries: &[
                    // 0: src color (filterable ok; we textureLoad)
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    // 1: mask (R32Float, non-filterable, textureLoad)
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
                    // 2: dst storage
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
                    // 3: uniform
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
                    // 4: per-layer 3x256 tone-curve LUT (R,G,B rows), read-only storage
                    // buffer — same binding style as `tone_curve.wgsl`'s retired
                    // global LUT binding.
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // 5/6 (Phase 4 Task 2): the shared whole-image dehaze
                    // transmission (source space, possibly mip-mapped) + its
                    // sampler — mirrors the retired `DehazeRecoveryNode`'s
                    // bindings 1/4 exactly. Always bound (a 1x1 neutral
                    // fallback when no real transmission is set — see
                    // `dehaze_neutral_tex`), so every dispatch (Light stage,
                    // mask layers, global pseudo-layer) validates regardless
                    // of whether it ever samples this.
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let module = ctx.shader_module("local-adjust", include_str!("shaders/local_adjust.wgsl"));
        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("local-adjust"),
                bind_group_layouts: &[&apply_bgl],
                push_constant_ranges: &[],
            });
        let apply_pipeline = ctx
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("local-adjust"),
                layout: Some(&layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            });
        let full_coverage_mask = Self::alloc_full_coverage_mask(&ctx);

        // Phase 4 Task 2: the fused dehaze recovery's shared-transmission
        // binding — mirrors the retired `DehazeRecoveryNode`'s neutral-texture
        // fallback pattern exactly.
        let dehaze_sampler = linear_clamp_sampler(&ctx);
        let dehaze_neutral_tex = Arc::new(ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("local-adjust-neutral-transmission"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PIPELINE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        }));
        let dehaze_neutral_view =
            dehaze_neutral_tex.create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            compositor: MaskCompositor::new(ctx.clone()),
            apply_bgl,
            apply_pipeline,
            apply_out: RefCell::new(None),
            apply_bufs: RefCell::new(Vec::new()),
            apply_buf_cursor: std::cell::Cell::new(0),
            tile: RefCell::new(None),
            cache: RefCell::new(None),
            rebuilds: std::cell::Cell::new(0),
            evals: std::cell::Cell::new(0),
            stage,
            global_set,
            full_coverage_mask,
            dehaze_params,
            dehaze_geometry: Cell::new(EngineDehazeGeometry::default()),
            dehaze_frame,
            dehaze_sampler,
            dehaze_neutral_tex: dehaze_neutral_tex.clone(),
            dehaze_shared_tex: RefCell::new(dehaze_neutral_tex),
            dehaze_shared_view: RefCell::new(dehaze_neutral_view),
            dehaze_has_transmission: Cell::new(false),
            ctx,
            layers,
        }
    }

    /// Set the output→source geometry mapping for the fused dehaze recovery
    /// step (mirrors the retired `DehazeRecoveryNode::set_geometry` exactly).
    /// `out_origin` is ignored — this node's own output-space origin comes
    /// from the shared `TileFrame` (`dehaze_frame`) instead. Harmless to call
    /// on a Light-stage instance (never read there).
    pub(crate) fn set_geometry(&self, g: GeometryUniform) {
        self.dehaze_geometry.set(EngineDehazeGeometry {
            m: g.m,
            off: g.off,
            src_dims: g.src_dims,
            out_dims: g.out_dims,
        });
    }

    /// Bind (or clear) the externally-supplied shared dehaze transmission
    /// texture (source space; e.g. `DehazeTransmissionNode::current_output_texture()`).
    /// `None` falls back to the 1×1 neutral texture with `has_transmission =
    /// false`, so the fused recovery step passes pixels through unchanged.
    /// Rebuilds only the cached view — NEVER the pipeline (CLAUDE.md GPU
    /// rule). A no-op when `tex` is already the currently-bound texture (`Arc`
    /// pointer equality), so the owning pipeline can call this unconditionally
    /// every evaluate. Mirrors the retired `DehazeRecoveryNode::set_shared_transmission`.
    pub(crate) fn set_shared_transmission(&self, tex: Option<Arc<wgpu::Texture>>) {
        let next = tex.unwrap_or_else(|| self.dehaze_neutral_tex.clone());
        if Arc::ptr_eq(&self.dehaze_shared_tex.borrow(), &next) {
            return;
        }
        let view = next.create_view(&wgpu::TextureViewDescriptor::default());
        self.dehaze_has_transmission
            .set(!Arc::ptr_eq(&next, &self.dehaze_neutral_tex));
        *self.dehaze_shared_view.borrow_mut() = view;
        *self.dehaze_shared_tex.borrow_mut() = next;
    }

    /// Allocate the 1x1 mask bound (but never sampled — see the field doc) for
    /// full-coverage pseudo-layer dispatches. Written to 1.0 for documentation/
    /// debug-inspection purposes only; the shader's coverage flag bypasses the
    /// `textureLoad` entirely for these dispatches, so the content is inert.
    fn alloc_full_coverage_mask(ctx: &GpuContext) -> MaskBuffer {
        let buf = MaskBuffer::alloc(ctx, 1, 1);
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &buf.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&[1.0f32]),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        buf
    }

    /// Number of times the composited-mask cache has been rebuilt (test hook).
    #[cfg(test)]
    pub(crate) fn rebuild_count(&self) -> u32 {
        self.rebuilds.get()
    }

    /// Number of times this node's `evaluate` has run (test hook): proves the
    /// graph's dirty-tracking skips a stage whose relevant segment/layers are
    /// unchanged (see `pipeline.rs`'s `set_stack` dirty-routing tests).
    #[cfg(test)]
    pub(crate) fn eval_count(&self) -> u32 {
        self.evals.get()
    }

    /// Set the tile-tier placement. `None` = whole-image (identity, cached);
    /// `Some(t)` = tiled: the mask is composited fresh each evaluate at the
    /// input (tile) resolution with placement `t`, so content-dependent
    /// components (Color/Luminance range) sample this tile's own edited pixels
    /// and spatial components map to full-image uv. Clears the cache on change.
    pub(crate) fn set_tile_transform(&self, tile: Option<TileTransform>) {
        let mut cur = self.tile.borrow_mut();
        if *cur != tile {
            *cur = tile;
            self.cache.borrow_mut().take();
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

    /// Return the A/B slot that is NOT `input` (by texture identity), allocating
    /// or reallocating both slots together if dims changed. This guarantees the
    /// dispatch's sampled (read) texture and write-storage (dst) texture are
    /// never the same resource — see the `apply_out` field doc for why.
    fn ensure_out(&self, input: &PipelineImage, w: u32, h: u32) -> PipelineImage {
        let mut out = self.apply_out.borrow_mut();
        let needs_alloc = match out.as_ref() {
            Some([a, _]) => (a.width, a.height) != (w, h),
            None => true,
        };
        if needs_alloc {
            *out = Some([
                self.alloc_out(w, h, "local-adjust-out-a"),
                self.alloc_out(w, h, "local-adjust-out-b"),
            ]);
        }
        let [a, b] = out.as_ref().unwrap();
        if Arc::ptr_eq(&a.texture, &input.texture) {
            b.clone()
        } else {
            a.clone()
        }
    }

    fn apply(
        &self,
        input: &PipelineImage,
        mask: &MaskBuffer,
        u: LocalAdjustUniform,
        lut: &[[f32; 256]; 3],
    ) -> PipelineImage {
        let dst = self.ensure_out(input, input.width, input.height);
        // Reuse the pooled per-dispatch (uniform, LUT) pair for this apply's
        // slot (see the `apply_bufs` field doc), writing fresh contents via the
        // queue instead of allocating new buffers every dispatch.
        let slot = self.apply_buf_cursor.get();
        self.apply_buf_cursor.set(slot + 1);
        {
            let mut bufs = self.apply_bufs.borrow_mut();
            while bufs.len() <= slot {
                let ubuf = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("local-adjust-uniform"),
                    size: std::mem::size_of::<LocalAdjustUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let lut_buf = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("local-adjust-lut"),
                    size: std::mem::size_of::<[[f32; 256]; 3]>() as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                bufs.push((ubuf, lut_buf));
            }
        }
        let bufs = self.apply_bufs.borrow();
        let (ubuf, lut_buf) = &bufs[slot];
        self.ctx.queue.write_buffer(ubuf, 0, bytemuck::bytes_of(&u));
        self.ctx
            .queue
            .write_buffer(lut_buf, 0, bytemuck::bytes_of(lut));
        let src_view = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mask_view = mask
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // Phase 4 Task 2: the shared dehaze transmission binding — always
        // present (the 1x1 neutral fallback when unset), so this bind group
        // validates for every apply() call (Light stage, mask layers, global
        // pseudo-layer) regardless of whether the shader's recovery step
        // actually samples it (see `dehaze_recover_step`'s gate).
        let trans_view = self.dehaze_shared_view.borrow();
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("local-adjust-bind"),
                layout: &self.apply_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&mask_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&dst_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: ubuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: lut_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&trans_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::Sampler(&self.dehaze_sampler),
                    },
                ],
            });
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("local-adjust-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.apply_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(input.width.div_ceil(8), input.height.div_ceil(8), 1);
        }
        self.ctx.queue.submit([enc.finish()]);
        dst
    }

    /// Overwrite `u`'s dehaze fields for a dispatch with the fused recovery
    /// step active, from THIS node's shared geometry/frame (mirrors the
    /// retired `DehazeRecoveryNode`'s per-dispatch uniform fill). Phase 4
    /// Task 3: shared by BOTH the global Color-stage pseudo-layer dispatch
    /// (`evaluate_color`'s first `apply`) and a per-mask-layer dispatch (the
    /// layer loop below) — the two differ only in WHICH `amount` drives the
    /// step (the global op's vs. `layer.adjustments.dehaze.amount`) and
    /// nothing else: geometry, frame, and atmospheric light are whole-image
    /// properties shared by every dispatch, never per-layer. Callers must
    /// gate on `amount != 0.0 && self.dehaze_has_transmission.get()`
    /// themselves before calling this — it unconditionally overwrites.
    fn fill_dehaze_uniform(&self, u: &mut LocalAdjustUniform, amount: f32, atmos: [f32; 4]) {
        let geo = self.dehaze_geometry.get();
        let frame = self.dehaze_frame.get();
        u.dehaze_amount_atmos = [amount, atmos[0], atmos[1], atmos[2]];
        u.dehaze_geo_m = geo.m;
        u.dehaze_geo_off_src_dims = [geo.off[0], geo.off[1], geo.src_dims[0], geo.src_dims[1]];
        u.dehaze_frame = [
            frame.origin[0],
            frame.origin[1],
            frame.full_dims[0],
            frame.full_dims[1],
        ];
        u.dehaze_out_dims_flags = [geo.out_dims[0], geo.out_dims[1], 1.0, 0.0];
    }
}

impl Node<PipelineImage> for LocalAdjustmentsNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        self.evals.set(self.evals.get() + 1);
        // Rewind the pooled per-dispatch buffer cursor: each `apply` of THIS
        // evaluate takes the next slot (see the `apply_bufs` field doc).
        self.apply_buf_cursor.set(0);
        let input = inputs[0];
        match self.stage {
            EngineStage::Light => self.evaluate_light(input),
            EngineStage::Color => self.evaluate_color(input),
        }
    }
}

impl LocalAdjustmentsNode {
    /// `Stage::Light`: exactly one dispatch (or zero, if the global set's
    /// light segment is identity) — the global set's `light_segment()`, global
    /// order, full coverage. No mask compositing at all.
    fn evaluate_light(&self, input: &PipelineImage) -> PipelineImage {
        let seg = self.global_set.borrow().light_segment();
        if seg.is_identity() {
            return input.clone();
        }
        let u = local_adjust_uniform(&seg, true, true);
        let lut = local_layer_lut(&seg);
        self.apply(input, &self.full_coverage_mask, u, &lut)
    }

    /// `Stage::Color`: dispatch 1 (skipped when identity) is the global set's
    /// `color_segment()` pseudo-layer (global order, full coverage), then the
    /// existing per-mask-layer loop continues from whatever `current` the
    /// pseudo-layer left behind.
    ///
    /// **Task 3 binding ruling:** mask compositing keys off `current` (post-
    /// global-color-segment), NOT the node's raw `input`. Pre-fusion, the
    /// standalone tone-curve/HSL/color-grade nodes ran UPSTREAM of this node in
    /// the graph, so this node's raw `input` was already post-color-segment —
    /// that's what the committed `luma_range_mask`/`color_range_mask` parity
    /// goldens were rendered against (see their fixture doc comments). Now that
    /// those ops run INSIDE this node as the pseudo-layer dispatch below,
    /// reproducing that same content means sampling `current` after it runs,
    /// not the node's now-pre-color-segment `input`.
    ///
    /// **Phase 4 Task 3:** the per-mask-layer loop's `apply` call ALSO gets
    /// the fused dehaze recovery step as the first part of its `adjust()`,
    /// driven by that layer's own `adjustments.dehaze.amount` (not the global
    /// op) — see the loop body + `fill_dehaze_uniform`'s doc.
    fn evaluate_color(&self, input: &PipelineImage) -> PipelineImage {
        let global_seg = self.global_set.borrow().color_segment();
        let layers = self.layers.borrow();

        // Phase 4 Task 2: the fused dehaze recovery is the FIRST step of this
        // node's global pseudo-layer dispatch. Active only when a real
        // `Dehaze` op is present (`amount != 0.0`) AND a real shared
        // transmission is bound (`dehaze_has_transmission`) — mirrors the
        // retired `DehazeRecoveryNode`'s own `amount == 0.0 || !has_transmission`
        // passthrough gate exactly, so this reproduces bit-identical output to
        // the pre-fusion engine whenever dehaze is inactive, at zero extra
        // dispatch cost.
        let dehaze = self.dehaze_params.get();
        let dehaze_active = dehaze.amount != 0.0 && self.dehaze_has_transmission.get();

        if global_seg.is_identity() && !dehaze_active && layers.is_identity() {
            return input.clone();
        }

        let mut current = input.clone();
        if !global_seg.is_identity() || dehaze_active {
            let mut u = local_adjust_uniform(&global_seg, true, true);
            if dehaze_active {
                self.fill_dehaze_uniform(&mut u, dehaze.amount, dehaze.atmos);
            }
            let lut = local_layer_lut(&global_seg);
            current = self.apply(&current, &self.full_coverage_mask, u, &lut);
        }

        if layers.is_identity() {
            return current;
        }

        // Composite the mask at CURRENT's resolution (identical to input's —
        // the pseudo-layer apply pass never changes dims): whole image for
        // preview, one (haloed) tile for the tiled tier. Range components read
        // this exact (post-global-color-segment) content; the tile placement
        // maps spatial components to full-image uv.
        let (mw, mh) = (current.width, current.height);
        let tile = self.tile.borrow();
        let placement = tile.unwrap_or_else(|| TileTransform::whole_image(mw, mh));
        let current_view = current
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let cur_defs: Vec<ferrolite_mask::MaskDefinition> =
            layers.visible_layers().map(|l| l.mask.clone()).collect();

        // The compositing cache is keyed on mask DEFINITIONS + dims (so an
        // adjustment-only change reuses it), but a content-dependent range
        // component now samples `current`, whose pixels depend on the global
        // color segment — a grade/curve/HSL/saturation/hue/vibrance change can
        // shift what such a component selects even with identical mask defs.
        // Fold the color segment into the key ONLY when at least one visible
        // layer's mask actually has a content-dependent component, so a purely
        // spatial mask set (gradient/radial/brush) keeps the original
        // adjustment-only-change reuse guarantee untouched.
        let content_dependent = cur_defs.iter().any(has_content_dependent_component);
        let color_seg_key = content_dependent.then(|| global_seg.clone());

        // The whole-image path caches masks (keyed on defs+dims+color-seg) so an
        // adjustment-only change reuses them. The tiled path composites fresh
        // every evaluate: each produced tile has different content/placement,
        // and content-dependent components would otherwise go stale across
        // upstream edits. Tile masks are tile-sized, so this is cheap + bounded.
        let use_cache = tile.is_none();
        let composite_all = || -> Vec<MaskBuffer> {
            self.rebuilds.set(self.rebuilds.get() + 1);
            layers
                .visible_layers()
                .map(|l| {
                    self.compositor.composite(
                        &l.mask,
                        &current_view,
                        mw,
                        mh,
                        &RasterStore::default(),
                        placement,
                    )
                })
                .collect()
        };

        let masks: Vec<MaskBuffer> = if use_cache {
            let hit = {
                let c = self.cache.borrow();
                matches!(&*c, Some(cm) if cm.mask_defs == cur_defs
                    && cm.dims == (mw, mh)
                    && cm.color_seg_key == color_seg_key)
            };
            if !hit {
                let masks = composite_all();
                *self.cache.borrow_mut() = Some(CachedMasks {
                    mask_defs: cur_defs.clone(),
                    dims: (mw, mh),
                    color_seg_key,
                    masks,
                });
            }
            self.cache.borrow().as_ref().unwrap().masks.clone()
        } else {
            composite_all()
        };

        for (layer, mask) in layers.visible_layers().zip(masks.iter()) {
            let mut u = local_adjust_uniform(&layer.adjustments, false, false);
            // Phase 4 Task 3: per-mask dehaze recovery, driven by THIS layer's
            // own `dehaze.amount` (per-mask radius is not exposed — every
            // layer recovers from the SAME shared whole-image transmission
            // map/atmos as the global pseudo-layer, reused via
            // `self.dehaze_params.get().atmos`). Gated exactly like the
            // pseudo-layer dispatch above (`amount != 0.0 &&
            // dehaze_has_transmission`), so a layer with a zero (identity)
            // dehaze amount — every existing fixture — takes NONE of this and
            // keeps `u`'s dehaze fields at `local_adjust_uniform`'s all-zero
            // default, i.e. bit-identical to pre-Task-3 output.
            let layer_amount = layer.adjustments.dehaze.amount;
            if layer_amount != 0.0 && self.dehaze_has_transmission.get() {
                let atmos = self.dehaze_params.get().atmos;
                self.fill_dehaze_uniform(&mut u, layer_amount, atmos);
            }
            let lut = local_layer_lut(&layer.adjustments);
            current = self.apply(&current, mask, u, &lut);
        }
        current
    }
}

impl Node<PipelineImage> for Rc<LocalAdjustmentsNode> {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        (**self).evaluate(inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::{AdjustmentSet, MaskLayer};
    use crate::nodes::upload_source;
    use crate::op::{ColorGrade, Dehaze, GradeWheel, Hsl, ToneCurve};
    use ferrolite_image::LinearRgbaF32;
    use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition, Vec2 as MVec2};

    /// Tiny 8x8 display-linear gradient source, uploaded to a GPU texture.
    fn gradient_source(ctx: &GpuContext) -> PipelineImage {
        let (w, h) = (8u32, 8u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[x as f32 / w as f32, y as f32 / h as f32, 0.25, 1.0]);
            }
        }
        let img = LinearRgbaF32::new(w, h, px).expect("gradient length");
        upload_source(ctx, &img)
    }

    /// Placeholder Phase 4 Task 2 dehaze state for tests that don't exercise
    /// the fused recovery — never mutated, so `evaluate_color`'s
    /// `dehaze_active` gate stays false (`ColorDehazeParams::default().amount
    /// == 0.0`) and these tests are unaffected by the fusion.
    fn no_dehaze() -> (Rc<Cell<ColorDehazeParams>>, Rc<Cell<TileFrame>>) {
        (
            Rc::new(Cell::new(ColorDehazeParams::default())),
            Rc::new(Cell::new(TileFrame::default())),
        )
    }

    /// Read an `Rgba16Float` `PipelineImage` back to display-linear f32 RGBA on
    /// the CPU (test-only; minimal inline readback, mirroring the integration
    /// tests' `read_image_linear` helper which unit tests in this crate cannot
    /// reach since it lives in `tests/common`).
    fn read_pixels(ctx: &GpuContext, img: &PipelineImage) -> Vec<f32> {
        let (w, h) = (img.width, img.height);
        let bpp = 8u32; // RGBA16F
        let bpr_unpadded = w * bpp;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bpr_padded = bpr_unpadded.div_ceil(align) * align;
        let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("local-node-test-readback"),
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
        let mut out = vec![0.0f32; (w * h * 4) as usize];
        for row in 0..h {
            let start = (row * bpr_padded) as usize;
            for px in 0..(w * 4) {
                let o = start + px as usize * 2;
                let hf = half::f16::from_le_bytes([data[o], data[o + 1]]);
                out[(row * w * 4 + px) as usize] = hf.to_f32();
            }
        }
        drop(data);
        buf.unmap();
        out
    }

    /// CPU reference for the 8x8 gradient run through both visible layers'
    /// `AdjustmentSet`s (full mask = every pixel gets the full effect), using the
    /// same `light_color_apply` the GPU shader mirrors (see `uniforms.rs`).
    fn expected_pixels(la: &LocalAdjustments) -> Vec<f32> {
        let (w, h) = (8u32, 8u32);
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let mut rgb = [x as f32 / w as f32, y as f32 / h as f32, 0.25];
                for l in la.visible_layers() {
                    rgb = crate::uniforms::light_color_apply(rgb, &l.adjustments, false);
                }
                out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
            }
        }
        out
    }

    fn layer(name: &str, exposure: f32, temp: f32) -> MaskLayer {
        MaskLayer {
            name: name.into(),
            visible: true,
            mask: MaskDefinition::default(), // no components -> full (all-ones) mask
            adjustments: AdjustmentSet {
                exposure,
                temp,
                ..Default::default()
            },
        }
    }

    /// Regression test for the texture-aliasing panic: with 2+ visible layers,
    /// `apply`'s ping-ponged `current` used to collide with the single cached
    /// `apply_out` texture on the second dispatch (same dims -> same cached
    /// texture bound as both sampled input and write-storage output in one
    /// dispatch), which wgpu's validation layer rejects with a
    /// conflicting-usages panic. The A/B ensure_out fix must let this evaluate
    /// cleanly on a real GPU.
    #[test]
    fn two_visible_layers_evaluate_without_panicking() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);

        let la = LocalAdjustments {
            layers: vec![layer("layer1", 0.5, 0.0), layer("layer2", -0.3, 0.4)],
        };
        let (dehaze_params, dehaze_frame) = no_dehaze();
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(la)),
            EngineStage::Color,
            Rc::new(RefCell::new(AdjustmentSet::default())),
            dehaze_params,
            dehaze_frame,
        );

        // Reaching this line without a wgpu validation panic already proves the
        // aliasing fix; the pixel-value assertion below is a bonus check. (The
        // upload_source texture itself lacks COPY_SRC, so we compare against
        // the CPU reference `light_color_apply` composition rather than reading
        // the source back.)
        let out = node.evaluate(&[&src]);
        assert_eq!((out.width, out.height), (src.width, src.height));

        let out_px = read_pixels(&ctx, &out);
        let la = node.layers.borrow();
        let expected = expected_pixels(&la);
        for (got, want) in out_px.iter().zip(expected.iter()) {
            assert!(
                (got - want).abs() < 5e-3,
                "pixel mismatch: got {got}, want {want}"
            );
        }
    }

    /// Same two-layer document evaluated twice in a row (simulating consecutive
    /// `Graph` evaluates) must keep working: the A/B slots are reused across
    /// calls, and a stable single-shot evaluate must not corrupt itself.
    #[test]
    fn repeated_evaluate_is_stable_across_calls() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);

        let la = LocalAdjustments {
            layers: vec![layer("layer1", 0.5, 0.0), layer("layer2", -0.3, 0.4)],
        };
        let (dehaze_params, dehaze_frame) = no_dehaze();
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(la)),
            EngineStage::Color,
            Rc::new(RefCell::new(AdjustmentSet::default())),
            dehaze_params,
            dehaze_frame,
        );

        let out1 = node.evaluate(&[&src]);
        let px1 = read_pixels(&ctx, &out1);
        let out2 = node.evaluate(&[&src]);
        let px2 = read_pixels(&ctx, &out2);
        assert_eq!(px1, px2, "repeated evaluate of the same inputs is stable");
    }

    /// Regression for the perf bug: dragging a per-mask adjustment slider (e.g.
    /// exposure) used to re-composite ALL masks at full resolution every frame,
    /// because the cache invalidated on the whole `LocalAdjustments` (masks +
    /// adjustments). The cache must be keyed on the mask DEFINITIONS only, so
    /// adjustment-only changes reuse the cached masks and only the (cheap) apply
    /// pass re-runs.
    #[test]
    fn adjustment_only_change_does_not_recomposite_masks() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);
        // One visible layer with a real mask component (so compositing does work).
        let mut la = LocalAdjustments {
            layers: vec![MaskLayer {
                name: "m".into(),
                visible: true,
                mask: MaskDefinition {
                    components: vec![(
                        ferrolite_mask::MaskComponent::LinearGradient {
                            start: ferrolite_mask::Vec2::new(0.0, 0.5),
                            end: ferrolite_mask::Vec2::new(1.0, 0.5),
                        },
                        ferrolite_mask::CompositeMode::Add,
                    )],
                    invert: false,
                },
                adjustments: AdjustmentSet {
                    exposure: 0.2,
                    ..Default::default()
                },
            }],
        };
        let layers_rc = Rc::new(RefCell::new(la.clone()));
        let (dehaze_params, dehaze_frame) = no_dehaze();
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            layers_rc.clone(),
            EngineStage::Color,
            Rc::new(RefCell::new(AdjustmentSet::default())),
            dehaze_params,
            dehaze_frame,
        );

        let _ = node.evaluate(&[&src]);
        assert_eq!(
            node.rebuild_count(),
            1,
            "first evaluate composites masks once"
        );

        // Change ONLY the adjustment (masks identical) and re-evaluate.
        la.layers[0].adjustments.exposure = 0.9;
        *layers_rc.borrow_mut() = la.clone();
        let _ = node.evaluate(&[&src]);
        assert_eq!(
            node.rebuild_count(),
            1,
            "adjustment-only change must REUSE cached masks"
        );

        // Now change the mask itself -> must recomposite.
        la.layers[0].mask.components[0] = (
            ferrolite_mask::MaskComponent::LinearGradient {
                start: ferrolite_mask::Vec2::new(0.0, 0.0),
                end: ferrolite_mask::Vec2::new(0.0, 1.0),
            },
            ferrolite_mask::CompositeMode::Add,
        );
        *layers_rc.borrow_mut() = la.clone();
        let _ = node.evaluate(&[&src]);
        assert_eq!(node.rebuild_count(), 2, "mask change recomposites");
    }

    /// Phase 2b parity: a layer with a non-identity tone curve, one non-identity
    /// HSL band, and a non-identity color grade must match the CPU reference
    /// `light_color_apply`, which composes curve -> HSL bands -> grade in the
    /// same order the WGSL now does (right after hue, before the color swatch).
    #[test]
    fn curve_hsl_grade_layer_matches_cpu_reference() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);

        let mut hsl = Hsl::default();
        hsl.bands[0].sat = 0.4;
        let grade = ColorGrade {
            shadows: GradeWheel {
                hue: 210.0,
                sat: 0.5,
                lum: 0.0,
            },
            ..Default::default()
        };
        let adjustments = AdjustmentSet {
            tone_curve: ToneCurve {
                points: vec![(0.0, 0.2), (1.0, 1.0)],
                ..Default::default()
            },
            hsl,
            color_grade: grade,
            ..Default::default()
        };
        let la = LocalAdjustments {
            layers: vec![MaskLayer {
                name: "curve-hsl-grade".into(),
                visible: true,
                mask: MaskDefinition::default(),
                adjustments,
            }],
        };
        let (dehaze_params, dehaze_frame) = no_dehaze();
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(la)),
            EngineStage::Color,
            Rc::new(RefCell::new(AdjustmentSet::default())),
            dehaze_params,
            dehaze_frame,
        );

        let out = node.evaluate(&[&src]);
        let out_px = read_pixels(&ctx, &out);
        let la = node.layers.borrow();
        let expected = expected_pixels(&la);
        for (got, want) in out_px.iter().zip(expected.iter()) {
            assert!(
                (got - want).abs() < 5e-3,
                "pixel mismatch: got {got}, want {want}"
            );
        }
    }

    /// Identity-extension guard: a layer using ONLY the pre-Phase-2b Light+Color
    /// fields (curve/hsl/grade left at their default identity) must produce the
    /// same output as before this task added the curve/HSL/grade fields to the
    /// uniform + shader — asserted against `light_color_apply`, which Task 1 kept
    /// bit-stable for identity curve/hsl/grade. Guards against the new
    /// `active_flags`-gated branches leaking into layers that don't use them.
    #[test]
    fn light_color_only_layer_is_unaffected_by_phase_2b_fields() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);

        let la = LocalAdjustments {
            layers: vec![layer("legacy", 0.35, -0.2)],
        };
        let (dehaze_params, dehaze_frame) = no_dehaze();
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(la)),
            EngineStage::Color,
            Rc::new(RefCell::new(AdjustmentSet::default())),
            dehaze_params,
            dehaze_frame,
        );

        let out = node.evaluate(&[&src]);
        let out_px = read_pixels(&ctx, &out);
        let la = node.layers.borrow();
        let expected = expected_pixels(&la);
        for (got, want) in out_px.iter().zip(expected.iter()) {
            assert!(
                (got - want).abs() < 5e-3,
                "pixel mismatch: got {got}, want {want}"
            );
        }
    }

    /// Phase 3: a `Stage::Light` node dispatches exactly the global set's
    /// `light_segment()` in global order (WB before contrast) with full
    /// coverage — matched against the CPU reference called the same way.
    #[test]
    fn light_stage_node_matches_cpu_reference_with_global_order() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);

        // `light_trio`-style params (design doc fixture 2): exercises the
        // WB<->contrast order directly.
        let global = AdjustmentSet {
            exposure: 0.8,
            contrast: 0.35,
            temp: 0.4,
            tint: -0.2,
            ..Default::default()
        };
        let (dehaze_params, dehaze_frame) = no_dehaze();
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(LocalAdjustments::default())),
            EngineStage::Light,
            Rc::new(RefCell::new(global.clone())),
            dehaze_params,
            dehaze_frame,
        );

        let out = node.evaluate(&[&src]);
        let out_px = read_pixels(&ctx, &out);

        let (w, h) = (8u32, 8u32);
        let seg = global.light_segment();
        let mut expected = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let rgb = [x as f32 / w as f32, y as f32 / h as f32, 0.25];
                let out_rgb = crate::uniforms::light_color_apply(rgb, &seg, true);
                expected.extend_from_slice(&[out_rgb[0], out_rgb[1], out_rgb[2], 1.0]);
            }
        }
        for (got, want) in out_px.iter().zip(expected.iter()) {
            assert!(
                (got - want).abs() < 5e-3,
                "pixel mismatch: got {got}, want {want}"
            );
        }
    }

    /// Phase 3: a `Stage::Color` node with a non-identity global set and one
    /// mask layer must dispatch the global color-segment pseudo-layer FIRST
    /// (global order, full coverage), then the mask layer (mask order,
    /// composited mask) — matched against the CPU composition applied in the
    /// same sequence.
    #[test]
    fn color_stage_node_composes_global_segment_then_mask_layer() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);

        let global = AdjustmentSet {
            saturation: 0.4,
            hue: 0.1,
            ..Default::default()
        };
        let la = LocalAdjustments {
            layers: vec![layer("m", 0.3, 0.2)],
        };
        let (dehaze_params, dehaze_frame) = no_dehaze();
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(la.clone())),
            EngineStage::Color,
            Rc::new(RefCell::new(global.clone())),
            dehaze_params,
            dehaze_frame,
        );

        let out = node.evaluate(&[&src]);
        let out_px = read_pixels(&ctx, &out);

        let (w, h) = (8u32, 8u32);
        let seg = global.color_segment();
        let mut expected = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let mut rgb = [x as f32 / w as f32, y as f32 / h as f32, 0.25];
                rgb = crate::uniforms::light_color_apply(rgb, &seg, true);
                for l in la.visible_layers() {
                    rgb = crate::uniforms::light_color_apply(rgb, &l.adjustments, false);
                }
                expected.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
            }
        }
        for (got, want) in out_px.iter().zip(expected.iter()) {
            assert!(
                (got - want).abs() < 5e-3,
                "pixel mismatch: got {got}, want {want}"
            );
        }
    }

    /// A default (identity) global set must add ZERO dispatches: `evaluate`
    /// returns a clone of the exact input texture (proven via `Arc::ptr_eq`),
    /// not merely a pixel-identical copy — this is what keeps the `mask_only`
    /// parity fixture (Task 1) trivially exact once the engine is wired in.
    #[test]
    fn default_global_set_adds_no_dispatch_for_color_stage() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);

        let (dehaze_params, dehaze_frame) = no_dehaze();
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(LocalAdjustments::default())),
            EngineStage::Color,
            Rc::new(RefCell::new(AdjustmentSet::default())),
            dehaze_params,
            dehaze_frame,
        );
        let out = node.evaluate(&[&src]);
        assert!(
            Arc::ptr_eq(&out.texture, &src.texture),
            "default global set + no layers must add zero dispatches"
        );
    }

    /// Same guarantee as above, for the `Light` stage.
    #[test]
    fn default_global_set_adds_no_dispatch_for_light_stage() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);

        let (dehaze_params, dehaze_frame) = no_dehaze();
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(LocalAdjustments::default())),
            EngineStage::Light,
            Rc::new(RefCell::new(AdjustmentSet::default())),
            dehaze_params,
            dehaze_frame,
        );
        let out = node.evaluate(&[&src]);
        assert!(
            Arc::ptr_eq(&out.texture, &src.texture),
            "default global set must add zero dispatches for the Light stage too"
        );
    }

    /// Phase 4 Task 2 Step 1 (TDD): the Color-stage engine node with a
    /// synthetic constant transmission (q = 0.5) bound and a non-zero global
    /// dehaze amount matches the CPU reference `light_color_apply_with_dehaze`
    /// (recovery step with an injected constant `t`), within the same
    /// tolerance every other node-vs-CPU parity test in this module uses.
    /// Identity geometry + a `TileFrame` spanning the whole 8x8 fixture means
    /// source UV == local UV, so the constant transmission is sampled
    /// uniformly across every pixel.
    #[test]
    fn color_engine_dehaze_recovery_matches_cpu_reference() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);
        let (w, h) = (src.width, src.height);

        let q = 0.5f32;
        let mut trans_px = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            trans_px.extend_from_slice(&[q, q, q, 1.0]);
        }
        let trans_img = LinearRgbaF32::new(w, h, trans_px).expect("transmission fixture");
        let gpu_trans = upload_source(&ctx, &trans_img);

        let atmos = [0.9f32, 0.9, 0.9];
        let amount = 0.4f32;
        let dehaze_params = Rc::new(Cell::new(ColorDehazeParams {
            amount,
            atmos: [atmos[0], atmos[1], atmos[2], 0.0],
        }));
        let dehaze_frame = Rc::new(Cell::new(TileFrame {
            origin: [0.0, 0.0],
            full_dims: [w as f32, h as f32],
        }));
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(LocalAdjustments::default())),
            EngineStage::Color,
            Rc::new(RefCell::new(AdjustmentSet::default())),
            dehaze_params,
            dehaze_frame,
        );
        let (identity_geo, _, _) = crate::uniforms::geometry_uniform(None, w, h);
        node.set_geometry(identity_geo);
        node.set_shared_transmission(Some(gpu_trans.texture.clone()));

        let out = node.evaluate(&[&src]);
        let out_px = read_pixels(&ctx, &out);

        let mut expected = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let rgb = [x as f32 / w as f32, y as f32 / h as f32, 0.25];
                let out_rgb = crate::uniforms::light_color_apply_with_dehaze(
                    rgb,
                    &AdjustmentSet::default(),
                    true,
                    Some((amount, atmos, q)),
                );
                expected.extend_from_slice(&[out_rgb[0], out_rgb[1], out_rgb[2], 1.0]);
            }
        }
        for (got, want) in out_px.iter().zip(expected.iter()) {
            assert!(
                (got - want).abs() < 5e-3,
                "pixel mismatch: got {got}, want {want}"
            );
        }
    }

    /// Phase 4 Task 2: identity amount (0.0) must add ZERO dispatches — the
    /// SAME `Arc::ptr_eq` passthrough as a fully-default global set — even
    /// with a real shared transmission bound and non-default geometry/frame
    /// set. This is what makes the fusion bit-identical to the pre-change
    /// engine whenever dehaze is inactive (the brief's "flag-gated, zero
    /// extra work" requirement) rather than merely pixel-identical.
    #[test]
    fn color_engine_dehaze_identity_amount_adds_no_dispatch() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);
        let (w, h) = (src.width, src.height);

        let mut trans_px = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            trans_px.extend_from_slice(&[0.5f32, 0.5, 0.5, 1.0]);
        }
        let trans_img = LinearRgbaF32::new(w, h, trans_px).expect("transmission fixture");
        let gpu_trans = upload_source(&ctx, &trans_img);

        let dehaze_params = Rc::new(Cell::new(ColorDehazeParams {
            amount: 0.0,
            atmos: [0.9, 0.9, 0.9, 0.0],
        }));
        let dehaze_frame = Rc::new(Cell::new(TileFrame {
            origin: [0.0, 0.0],
            full_dims: [w as f32, h as f32],
        }));
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(LocalAdjustments::default())),
            EngineStage::Color,
            Rc::new(RefCell::new(AdjustmentSet::default())),
            dehaze_params,
            dehaze_frame,
        );
        let (identity_geo, _, _) = crate::uniforms::geometry_uniform(None, w, h);
        node.set_geometry(identity_geo);
        node.set_shared_transmission(Some(gpu_trans.texture.clone()));

        let out = node.evaluate(&[&src]);
        assert!(
            Arc::ptr_eq(&out.texture, &src.texture),
            "amount == 0.0 must add zero dispatches even with a real transmission bound"
        );
    }

    /// Pure-Rust CPU reference for `linear_gradient.wgsl`'s analytic mask
    /// formula (whole-image path: `uv_scale = [1,1]`, `uv_offset = [0,0]`),
    /// used to compute the EXACT per-pixel mask value the GPU compositor
    /// produces for a `MaskComponent::LinearGradient` — needed to build a
    /// correct expected buffer for a PARTIALLY-masked layer (every other
    /// dehaze parity test in this module uses a full, all-ones mask, so it
    /// never needed this).
    fn linear_gradient_mask_value(
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        start: (f32, f32),
        end: (f32, f32),
    ) -> f32 {
        let uv = ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
        let axis = (end.0 - start.0, end.1 - start.1);
        let len2 = axis.0 * axis.0 + axis.1 * axis.1;
        if len2 <= 1e-12 {
            return 0.0;
        }
        let dot = (uv.0 - start.0) * axis.0 + (uv.1 - start.1) * axis.1;
        (dot / len2).clamp(0.0, 1.0)
    }

    /// Phase 4 Task 3 Step 1 (TDD): a mask layer with a non-zero
    /// `dehaze.amount` over a synthetic (constant) transmission changes ONLY
    /// pixels the mask actually covers. `start`/`end` are chosen so columns
    /// x=0..3 get an EXACT 0.0 mask (unmasked — must be bit-identical to the
    /// layer's input) and x=4..7 get a partial (non-zero, non-one) mask
    /// (masked — must match the CPU reference, mask-order, blended by that
    /// exact mask value). The transmission is a spatially-CONSTANT `q`
    /// (sidesteps the LOD-independent sampling's own bilinear-blend math,
    /// already covered by `radius_change_propagates_to_recovered_output` at
    /// the pipeline level), isolating the per-mask WIRING under test: uniform
    /// fill (`fill_dehaze_uniform`) + the shader's existing mix-by-mask.
    #[test]
    fn mask_layer_dehaze_amount_changes_only_masked_pixels() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);
        let (w, h) = (src.width, src.height);

        let q = 0.6f32;
        let mut trans_px = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            trans_px.extend_from_slice(&[q, q, q, 1.0]);
        }
        let trans_img = LinearRgbaF32::new(w, h, trans_px).expect("transmission fixture");
        let gpu_trans = upload_source(&ctx, &trans_img);

        let atmos = [0.9f32, 0.9, 0.9];
        let amount = 0.5f32;
        // uv.x < 0.5 (columns 0..3) clamps to exactly 0.0; uv.x in (0.5, 1.0)
        // (columns 4..7) gives a partial, non-clamped value — end.x = 1.5 is
        // past the image's uv range, so no column reaches an exact 1.0 either.
        let (start, end) = ((0.5, 0.5), (1.5, 0.5));
        let layer_adjustments = AdjustmentSet {
            dehaze: Dehaze { amount, radius: 8 },
            ..Default::default()
        };
        let la = LocalAdjustments {
            layers: vec![MaskLayer {
                name: "dehaze-mask".into(),
                visible: true,
                mask: MaskDefinition {
                    components: vec![(
                        MaskComponent::LinearGradient {
                            start: MVec2::new(start.0, start.1),
                            end: MVec2::new(end.0, end.1),
                        },
                        CompositeMode::Add,
                    )],
                    invert: false,
                },
                adjustments: layer_adjustments.clone(),
            }],
        };
        let dehaze_params = Rc::new(Cell::new(ColorDehazeParams {
            amount: 0.0, // global dehaze inactive — only the LAYER's amount drives this
            atmos: [atmos[0], atmos[1], atmos[2], 0.0],
        }));
        let dehaze_frame = Rc::new(Cell::new(TileFrame {
            origin: [0.0, 0.0],
            full_dims: [w as f32, h as f32],
        }));
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(la)),
            EngineStage::Color,
            Rc::new(RefCell::new(AdjustmentSet::default())),
            dehaze_params,
            dehaze_frame,
        );
        let (identity_geo, _, _) = crate::uniforms::geometry_uniform(None, w, h);
        node.set_geometry(identity_geo);
        node.set_shared_transmission(Some(gpu_trans.texture.clone()));

        let out = node.evaluate(&[&src]);
        let out_px = read_pixels(&ctx, &out);

        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                let rgb = [x as f32 / w as f32, y as f32 / h as f32, 0.25];
                let m = linear_gradient_mask_value(x, y, w, h, start, end);
                if m == 0.0 {
                    for c in 0..3 {
                        assert!(
                            (out_px[idx + c] - rgb[c]).abs() < 1e-5,
                            "unmasked pixel ({x},{y}) channel {c}: got {}, want bit-identical \
                             input {}",
                            out_px[idx + c],
                            rgb[c]
                        );
                    }
                } else {
                    let adjusted = crate::uniforms::light_color_apply_with_dehaze(
                        rgb,
                        &layer_adjustments,
                        false,
                        Some((amount, atmos, q)),
                    );
                    let want = [
                        rgb[0] + (adjusted[0] - rgb[0]) * m,
                        rgb[1] + (adjusted[1] - rgb[1]) * m,
                        rgb[2] + (adjusted[2] - rgb[2]) * m,
                    ];
                    for c in 0..3 {
                        assert!(
                            (out_px[idx + c] - want[c]).abs() < 5e-3,
                            "masked pixel ({x},{y}) channel {c}: got {}, want {}",
                            out_px[idx + c],
                            want[c]
                        );
                    }
                }
            }
        }
    }

    /// Phase 4 Task 3: the SAME partially-masked layer as above but with the
    /// layer's `dehaze.amount` at identity (0.0) — even with a real shared
    /// transmission bound — must reproduce the plain (no-dehaze) CPU
    /// reference `light_color_apply` exactly (mask order), proving the
    /// per-layer gate (`layer_amount != 0.0 && dehaze_has_transmission`) keeps
    /// a zero-amount layer's uniform byte-identical to pre-Task-3, i.e. the
    /// bound transmission never leaks into an inactive layer's output.
    #[test]
    fn mask_layer_zero_dehaze_amount_ignores_bound_transmission() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = gradient_source(&ctx);
        let (w, h) = (src.width, src.height);

        let mut trans_px = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            trans_px.extend_from_slice(&[0.6f32, 0.6, 0.6, 1.0]);
        }
        let trans_img = LinearRgbaF32::new(w, h, trans_px).expect("transmission fixture");
        let gpu_trans = upload_source(&ctx, &trans_img);

        let (start, end) = ((0.5, 0.5), (1.5, 0.5));
        let layer_adjustments = AdjustmentSet {
            exposure: 0.2,
            dehaze: Dehaze {
                amount: 0.0,
                radius: 8,
            },
            ..Default::default()
        };
        let la = LocalAdjustments {
            layers: vec![MaskLayer {
                name: "dehaze-mask-inactive".into(),
                visible: true,
                mask: MaskDefinition {
                    components: vec![(
                        MaskComponent::LinearGradient {
                            start: MVec2::new(start.0, start.1),
                            end: MVec2::new(end.0, end.1),
                        },
                        CompositeMode::Add,
                    )],
                    invert: false,
                },
                adjustments: layer_adjustments.clone(),
            }],
        };
        let dehaze_params = Rc::new(Cell::new(ColorDehazeParams {
            amount: 0.0,
            atmos: [0.9, 0.9, 0.9, 0.0],
        }));
        let dehaze_frame = Rc::new(Cell::new(TileFrame {
            origin: [0.0, 0.0],
            full_dims: [w as f32, h as f32],
        }));
        let node = LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(la)),
            EngineStage::Color,
            Rc::new(RefCell::new(AdjustmentSet::default())),
            dehaze_params,
            dehaze_frame,
        );
        let (identity_geo, _, _) = crate::uniforms::geometry_uniform(None, w, h);
        node.set_geometry(identity_geo);
        node.set_shared_transmission(Some(gpu_trans.texture.clone()));

        let out = node.evaluate(&[&src]);
        let out_px = read_pixels(&ctx, &out);

        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                let rgb = [x as f32 / w as f32, y as f32 / h as f32, 0.25];
                let m = linear_gradient_mask_value(x, y, w, h, start, end);
                let adjusted = crate::uniforms::light_color_apply(rgb, &layer_adjustments, false);
                let want = [
                    rgb[0] + (adjusted[0] - rgb[0]) * m,
                    rgb[1] + (adjusted[1] - rgb[1]) * m,
                    rgb[2] + (adjusted[2] - rgb[2]) * m,
                ];
                for c in 0..3 {
                    assert!(
                        (out_px[idx + c] - want[c]).abs() < 5e-3,
                        "pixel ({x},{y}) channel {c}: got {}, want {}",
                        out_px[idx + c],
                        want[c]
                    );
                }
            }
        }
    }
}
