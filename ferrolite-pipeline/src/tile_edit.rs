//! `TileEditPipeline` — the per-tile, full-res GPU edit producer. For each
//! requested tile it runs geometry-at-the-head (resampling the GPU-resident
//! source for the haloed output tile) then the color chain (exposure→WB→contrast
//! →dehaze→tone-curve→HSL→LocalAdjustments→sharpen) over the haloed buffer, and returns
//! the interior `TILE_SIZE`² as an `Rgba16Float` `COPY_SRC` texture for the VT to
//! copy into a pool slot. No CPU readback (spec §5.2).
//!
//! Geometry is applied at the head (spec §8.4). For identity geometry the head is
//! a 1:1 haloed copy, so the result is identical to the whole-image Plan-2 chain
//! and to a whole-image render — this is what the tile-seam golden asserts. For
//! non-identity geometry, Sharpen operates in output space rather than source
//! space, an accepted pragmatic difference (architecture map §2).
//!
//! **Dehaze (ST-Task 3): shared whole-image transmission, no per-tile cost, no
//! halo.** Unlike Sharpen, dehaze does NOT compute its own per-tile
//! neighbourhood map. The guided-filter-refined transmission (design §5.2, the
//! ~14-dispatch dark-channel + guided filter) is computed exactly ONCE by the
//! whole-image `EditPipeline` (source space, bounded to
//! `DEHAZE_MAX_TRANSMISSION_DIM`) and handed to this pipeline via
//! `set_shared_transmission` (the app re-wires it whenever the preview
//! re-evaluates). `DehazeRecoveryNode` — the ONLY dehaze node here — just
//! SAMPLES that shared texture at each output pixel's SOURCE UV (the same
//! `m·out+off` mapping `GeometryHeadNode` uses, via `set_geometry` + the head's
//! shared `TileFrame`), a cheap single per-pixel pass. This is what fixed the
//! integrated-GPU OOM: the old per-tile `DehazeTransmissionNode` (removed) ran
//! its full multi-pass guided filter, haloed `7r` px, for every tile streamed
//! across a full-res image — exhausting a memory-constrained GPU's buffer
//! budget. A per-pixel sample has no neighbourhood to over-fetch, so
//! `dehaze_halo` is always 0 (see that fn's doc) and an amount/radius drag is a
//! cheap uniform-only update, same as any other color op. Under identity
//! geometry, source UV == output UV, so tiled == whole-image within
//! `SEAM_TOL` (the parity golden); under crop/rotate, sampling at the source
//! coordinate keeps the transmission aligned to the same source content the
//! geometry head resampled (correct-by-construction, not an accepted
//! difference like Sharpen's).
//!
//! **LocalAdjustments — per-tile mask, output space:** because geometry runs at
//! the head, the entire color chain (including `LocalAdjustments`) operates in
//! **output space**. Each `produce_tile` composites the layer masks at that
//! tile's own (haloed) resolution against the tile's edited content, placed via
//! `set_tile_transform` (haloed origin + LOD level dims). Content-dependent
//! components (Color/Luminance range) therefore sample the correct full-res
//! pixels; spatial components (gradient/radial/brush) are mapped to full-image
//! uv by the placement. For identity/translation geometry this matches the
//! whole-image preview render within float tolerance. Under crop/rotate the mask
//! anchors to the cropped/rotated **output** frame — the same accepted
//! difference already noted for Sharpen. Per-tile masks are tile-sized (bounded,
//! no full-frame mask buffer).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Graph, NodeId};
use ferrolite_image::{haloed_tile_origin, level_size, TileCoord, TILE_SIZE};
use ferrolite_mask::TileTransform;

use crate::dehaze::{DEHAZE_ATMOS_MIN, DEHAZE_ATMOS_NEUTRAL};
use crate::dehaze_node::{DehazeRecoveryNode, RecoveryParams};
use crate::gpu_pyramid::GpuPyramidSource;
use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::lens_gpu::{VignetteTexture, WarpGridTexture};
use crate::local::{AdjustmentSet, LocalAdjustments};
use crate::local_node::{EngineStage, LocalAdjustmentsNode};
use crate::nodes::{GeometryHeadNode, PointOpNode, TileFrame, TileRequest, VignetteNode};
use crate::op::{Aspect, CropRect, Geometry, LensCorrection, OpStack};
use crate::uniforms::{
    color_matrix_uniform, geometry_uniform, lens_halo_px, sharpen_halo, sharpen_uniform,
    ColorMatrixUniform, LensUniform, SharpenUniform, VignetteUniform,
};
use ferrolite_lens::{VignetteMap, WarpGrid};

pub struct TileEditPipeline {
    ctx: Arc<GpuContext>,
    graph: Graph<PipelineImage>,
    output_id: NodeId,
    request: Rc<Cell<TileRequest>>,
    head_id: NodeId,
    head: Rc<GeometryHeadNode>,
    color_matrix_id: NodeId,
    color_matrix: Rc<Cell<ColorMatrixUniform>>,
    vignette_id: NodeId,
    vignette: Rc<Cell<VignetteUniform>>,
    vignette_node: Rc<VignetteNode>,
    halo: u32,
    out_w: u32,
    out_h: u32,
    src_w: u32,
    src_h: u32,
    // Phase 3 (fused layer engine): the shared global two-segment `AdjustmentSet`
    // both engine-stage nodes below read from — mirrors `EditPipeline`'s field
    // of the same name exactly.
    global_set: Rc<RefCell<AdjustmentSet>>,
    light_engine_id: NodeId,
    // Handle to the Light-stage engine node (mirrors `EditPipeline`'s handle;
    // no test hook here today, but retained for parity/future use).
    #[allow(dead_code)]
    light_engine_node: Rc<LocalAdjustmentsNode>,
    // Halo-free dehaze (ST-Task 3): no per-tile transmission node here anymore
    // — `DehazeRecoveryNode` is the ONLY dehaze node, sampling a shared
    // whole-image transmission set via `set_shared_transmission` (see the
    // module doc). `dehaze_recovery_id`'s only graph input is `light_engine_id`.
    dehaze_recovery_id: NodeId,
    recovery_params: Rc<Cell<RecoveryParams>>,
    // Handle to the recovery node, retained for `set_shared_transmission` /
    // `set_dehaze_atmos`. Constructed with the head's SHARED `TileFrame`
    // (`frame`, same `Rc` the head writes and the vignette node reads) so its
    // `frame_origin` tracks each produced tile's real output-space origin.
    dehaze_recovery_node: Rc<DehazeRecoveryNode>,
    /// Whole-image atmospheric light (design §5.3). Unlike `EditPipeline`,
    /// `TileEditPipeline` has no CPU source to estimate this from directly — it
    /// starts at `DEHAZE_ATMOS_NEUTRAL` and the app hands it the real estimate
    /// (computed once from the preview-resolution image) via `set_dehaze_atmos`
    /// right after construction.
    dehaze_atmos: [f32; 3],
    local_adjust_id: NodeId,
    local_layers: Rc<RefCell<LocalAdjustments>>,
    local_node: Rc<LocalAdjustmentsNode>,
    sharpen: Rc<Cell<SharpenUniform>>,
}

impl TileEditPipeline {
    /// Construct the per-tile producer, baking the geometry transform and the
    /// halo (max of the sharpen halo and the lens-warp halo) at construction.
    ///
    /// `warp_grid` / `vignette_map` are the app's CURRENT lens bake products (from
    /// `ferrolite-lens`); pass `None` when no lens is matched or the bake has not
    /// completed — the head then binds the identity warp / vignette defaults and
    /// the shader takes the byte-identical no-correction path. When a grid is
    /// present, the lens halo (over-fetch for the distortion displacement) is
    /// folded into the haloed tile extent so per-tile borders stay seamless.
    pub fn new(
        ctx: Arc<GpuContext>,
        source: Arc<GpuPyramidSource>,
        stack: OpStack,
        camera_to_working: [[f32; 3]; 3],
        warp_grid: Option<&WarpGrid>,
        vignette_map: Option<&VignetteMap>,
    ) -> Self {
        let (src_w, src_h) = source.level_size(0);
        let lc: Option<LensCorrection> = stack.lens_correction();
        // Dehaze contributes NO halo (ST-Task 3, `dehaze_halo` always 0): the
        // recovery is a per-pixel sample of a shared whole-image transmission,
        // not a per-tile neighbourhood filter. Halo is just sharpen + lens-warp.
        let halo = sharpen_halo(stack.sharpen()).max(lens_halo_px(lc.as_ref(), warp_grid));
        let geometry = stack.geometry().unwrap_or(Geometry {
            crop: CropRect::full(),
            angle_deg: 0.0,
            aspect: Aspect::Original,
        });
        let request = Rc::new(Cell::new(TileRequest {
            coord: TileCoord { lod: 0, x: 0, y: 0 },
            halo,
        }));

        // Shared output-space frame: the geometry head WRITES the current tile's
        // origin + full output dims each evaluate; the vignette node and the
        // dehaze recovery node (ST-Task 3) both READ it — the vignette so its
        // radius is measured in full-image space (seamless, not per-tile), the
        // recovery so its shared-transmission source-UV sample uses this tile's
        // real output-space origin. The graph runs head → (vignette, recovery)
        // in the same evaluate, so the frame is always current.
        let frame = Rc::new(Cell::new(TileFrame::default()));

        let mut graph = Graph::new();
        let head = Rc::new(GeometryHeadNode::new(
            ctx.clone(),
            source,
            geometry,
            request.clone(),
            frame.clone(),
        ));
        let head_id = graph.add_node(Box::new(head.clone()), vec![]);

        let color_matrix = Rc::new(Cell::new(color_matrix_uniform(camera_to_working)));
        let color_matrix_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/color_matrix.wgsl"),
                "color-matrix",
                color_matrix.clone(),
            )),
            vec![head_id],
        );

        // Vignetting: scene-linear point op, before exposure (spec §6.2). It is
        // point-wise, so its position in the per-tile color chain only needs to be
        // scene-linear. Default `vig_amount = 0` → identity (tile-seam golden safe).
        let vignette = Rc::new(Cell::new(VignetteUniform::default()));
        let vignette_node = Rc::new(VignetteNode::new(
            ctx.clone(),
            vignette.clone(),
            Some(frame.clone()),
        ));
        let vignette_id = graph.add_node(Box::new(vignette_node.clone()), vec![color_matrix_id]);

        // Phase 3 (fused layer engine): the Light-stage engine node replaces the
        // old exposure/white-balance/contrast `PointOpNode` trio at this exact
        // graph position — mirrors `EditPipeline::new`'s wiring exactly.
        let global_set = Rc::new(RefCell::new(stack.global.clone()));
        let light_engine_node = Rc::new(LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            Rc::new(RefCell::new(LocalAdjustments::default())),
            EngineStage::Light,
            global_set.clone(),
        ));
        let light_engine_id =
            graph.add_node(Box::new(light_engine_node.clone()), vec![vignette_id]);

        let dehaze_atmos = DEHAZE_ATMOS_NEUTRAL;
        let recovery_params = Rc::new(Cell::new(RecoveryParams::from_op(
            stack.dehaze(),
            dehaze_atmos,
        )));
        // ST-Task 3: no per-tile `DehazeTransmissionNode` anymore — the
        // recovery's only graph input is `I` (now `light_engine_id`); its
        // shared transmission is set out-of-band via `set_shared_transmission`
        // (see that fn + `produce_tile`). Constructed with the SAME shared
        // `frame` the head writes/vignette reads, so `frame_origin` is this
        // tile's real output-space origin (not a local per-tile identity
        // origin) — required for the source-UV mapping to be correct across
        // tiles.
        let dehaze_recovery_node = Rc::new(DehazeRecoveryNode::new(
            ctx.clone(),
            recovery_params.clone(),
            frame.clone(),
        ));
        let (geo_uniform, _, _) = geometry_uniform(stack.geometry(), src_w, src_h);
        dehaze_recovery_node.set_geometry(geo_uniform);
        let dehaze_recovery_id = graph.add_node(
            Box::new(dehaze_recovery_node.clone()),
            vec![light_engine_id],
        );

        // Phase 3: the Color-stage engine node replaces the old tone-curve →
        // hsl → color-grade → local-adjust chain in one node at this exact
        // graph position — mirrors `EditPipeline::new`'s wiring exactly. Its
        // per-tile mask compositing gets the same post-global-color-segment
        // treatment as the whole-image node (see `evaluate_color`'s doc);
        // `set_tile_transform` (called per `produce_tile`, below) is unchanged.
        let local_layers = Rc::new(RefCell::new(stack.local_adjustments().unwrap_or_default()));
        let local_node = Rc::new(LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            local_layers.clone(),
            EngineStage::Color,
            global_set.clone(),
        ));
        let (out_w, out_h) = crate::edited_output_dims(&stack, src_w, src_h);
        let local_adjust_id =
            graph.add_node(Box::new(local_node.clone()), vec![dehaze_recovery_id]);

        let sharpen = Rc::new(Cell::new(sharpen_uniform(stack.sharpen())));
        let sharpen_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/sharpen.wgsl"),
                "sharpen",
                sharpen.clone(),
            )),
            vec![local_adjust_id],
        );

        // Bind the lens bake products (or leave the identity defaults). The head
        // owns the warp grid + `LensUniform`; the vignette node owns the gain LUT
        // and reads its lerp amount from the `vignette` cell. `set_warp`/
        // `set_vignette` rebuild only the cached views (bake-time, not per frame),
        // and the uniform writes are buffer-only — no pipeline rebuild.
        if let Some(grid) = warp_grid {
            head.set_warp(WarpGridTexture::upload(&ctx, grid));
        }
        head.set_lens_uniform(crate::uniforms::lens_uniform(
            lc.as_ref(),
            warp_grid.is_some(),
        ));
        if let Some(map) = vignette_map {
            vignette_node.set_vignette(VignetteTexture::upload(&ctx, map));
        }
        vignette.set(VignetteUniform {
            vig_amount: crate::uniforms::vignette_amount(lc.as_ref()),
            ..VignetteUniform::default()
        });

        Self {
            ctx,
            graph,
            output_id: sharpen_id,
            request,
            head_id,
            head,
            color_matrix_id,
            color_matrix,
            vignette_id,
            vignette,
            vignette_node,
            halo,
            out_w,
            out_h,
            src_w,
            src_h,
            global_set,
            light_engine_id,
            light_engine_node,
            dehaze_recovery_id,
            recovery_params,
            dehaze_recovery_node,
            dehaze_atmos,
            local_adjust_id,
            local_layers,
            local_node,
            sharpen,
        }
    }

    pub fn halo(&self) -> u32 {
        self.halo
    }

    /// Re-derive the color-op param cells (exposure, white balance, contrast,
    /// tone curve, HSL, local adjustments, sharpen amount) from `stack` and
    /// dirty the chain so the next `produce_tile` re-renders.
    ///
    /// LIMITATION: the geometry transform (crop/rotate), the halo (max of the
    /// sharpen and lens-warp halos), and the baked lens warp grid are fixed at
    /// construction (baked into the `GeometryHeadNode` and the haloed extent).
    /// `set_stack` does NOT update them. If `stack.geometry()` changes, the halo
    /// changes, or the rebuild-relevant lens key changes (lens id / focal / aperture
    /// / crop / enabled flags — anything that re-bakes the grid), this pipeline must
    /// be DISCARDED and rebuilt with `TileEditPipeline::new` — calling `set_stack`
    /// alone will silently keep the old geometry/halo/grid. `needs_full_rebuild` in
    /// the app makes that decision. Amount-only lens changes are uniform updates via
    /// the lens/vignette setters (no rebuild). The `LocalAdjustments` output
    /// dims used to place each tile's mask are likewise derived from the stack's
    /// geometry at construction time and fixed thereafter — a geometry/output-dims
    /// change requires the same full rebuild, not just a `set_stack` call. The
    /// per-tile mask placement is set per `produce_tile` via `set_tile_transform`.
    /// `dehaze_recovery_node.set_geometry` is re-derived here too (ST-Task 3):
    /// this pipeline's geometry never actually changes across `set_stack` calls
    /// (see the LIMITATION above — a real geometry change needs a full rebuild),
    /// so this is a no-op in practice, but keeps the recovery's source-UV mapping
    /// explicitly in sync with the head's rather than relying on that invariant.
    pub fn set_stack(&mut self, stack: OpStack) {
        // Phase 3 (fused layer engine): same segment-wise dirty routing as
        // `EditPipeline::set_stack` (see its doc) — a light-segment change
        // dirties only `light_engine_id`, a color-segment change only
        // `local_adjust_id`. In THIS pipeline both are subsumed by the
        // unconditional `mark_dirty(self.head_id)` below (every `produce_tile`
        // call re-renders the whole per-tile chain regardless), but the
        // explicit routing is kept for parity with `EditPipeline` and so it
        // stays correct if that unconditional dirty is ever narrowed.
        if stack.global.light_segment() != self.global_set.borrow().light_segment() {
            self.graph.mark_dirty(self.light_engine_id);
        }
        if stack.global.color_segment() != self.global_set.borrow().color_segment() {
            self.graph.mark_dirty(self.local_adjust_id);
        }
        *self.global_set.borrow_mut() = stack.global.clone();
        // ST-Task 3: no transmission node here anymore — only `amount`/`atmos`
        // feed the (single) recovery node, a cheap uniform-only update.
        let r = RecoveryParams::from_op(stack.dehaze(), self.dehaze_atmos);
        if r != self.recovery_params.get() {
            self.recovery_params.set(r);
            self.graph.mark_dirty(self.dehaze_recovery_id);
        }
        let (geo_uniform, _, _) = geometry_uniform(stack.geometry(), self.src_w, self.src_h);
        self.dehaze_recovery_node.set_geometry(geo_uniform);
        let la = stack.local_adjustments().unwrap_or_default();
        if *self.local_layers.borrow() != la {
            *self.local_layers.borrow_mut() = la;
            // See EditPipeline::set_stack: the node re-composites only on mask-DEF
            // changes (keyed on `mask_defs`), so an adjustment-only change reuses
            // the cached masks. No blanket invalidate here.
            self.graph.mark_dirty(self.local_adjust_id);
        }
        self.sharpen.set(sharpen_uniform(stack.sharpen()));
        self.graph.mark_dirty(self.head_id);
    }

    /// Update the camera→working matrix (working-space change) and dirty the head.
    pub fn set_color_matrix(&mut self, m: [[f32; 3]; 3]) {
        let u = color_matrix_uniform(m);
        if u != self.color_matrix.get() {
            self.color_matrix.set(u);
            self.graph.mark_dirty(self.color_matrix_id);
        }
    }

    /// Bind a freshly baked lens warp grid to the geometry head (bake-time; no
    /// pipeline rebuild). Dirties the head so the next `produce_tile` re-samples.
    pub fn set_warp(&mut self, warp: WarpGridTexture) {
        self.head.set_warp(warp);
        self.graph.mark_dirty(self.head_id);
    }

    /// Set the lens correction amounts + `use_warp` flag on the geometry head
    /// (buffer write; no rebuild). Dirties the head so the next tile applies it.
    pub fn set_lens_uniform(&mut self, lens: LensUniform) {
        self.head.set_lens_uniform(lens);
        self.graph.mark_dirty(self.head_id);
    }

    /// Bind a freshly baked vignette gain LUT to the per-tile vignette pass
    /// (bake-time; rebuilds the cached view, no pipeline rebuild).
    pub fn set_vignette(&mut self, lut: VignetteTexture) {
        self.vignette_node.set_vignette(lut);
        self.graph.mark_dirty(self.vignette_id);
    }

    /// Set the vignette lerp amount (buffer write; no rebuild). 0 = identity.
    /// Read-modify-write so an independent `manual` setting is preserved.
    pub fn set_vig_amount(&mut self, amount: f32) {
        let u = VignetteUniform {
            vig_amount: amount,
            ..self.vignette.get()
        };
        if u != self.vignette.get() {
            self.vignette.set(u);
            self.graph.mark_dirty(self.vignette_id);
        }
    }

    /// Set the parametric manual (lens-free) vignette strength (buffer write; no
    /// rebuild). 0 = identity; negative darkens corners, positive brightens them.
    /// Read-modify-write so the independent `vig_amount` (profile) setting is
    /// preserved.
    pub fn set_vig_manual(&mut self, manual: f32) {
        let u = VignetteUniform {
            manual,
            ..self.vignette.get()
        };
        if u != self.vignette.get() {
            self.vignette.set(u);
            self.graph.mark_dirty(self.vignette_id);
        }
    }

    /// Set the whole-image atmospheric light `A` for the dehaze pass (design
    /// §5.3). Computed ONCE by the caller from the preview-resolution image and
    /// handed to every tile as a uniform — never estimated per tile. Buffer write
    /// only (no rebuild); re-derives the dehaze uniform from the current stack's
    /// amount + this `A`. Call right after construction (like `set_vig_amount`).
    pub fn set_dehaze_atmos(&mut self, atmos: [f32; 3]) {
        if atmos != self.dehaze_atmos {
            self.dehaze_atmos = atmos;
            let floored = [
                atmos[0].max(DEHAZE_ATMOS_MIN),
                atmos[1].max(DEHAZE_ATMOS_MIN),
                atmos[2].max(DEHAZE_ATMOS_MIN),
                0.0,
            ];
            // ST-Task 3: only the recovery pass needs `A` (for its
            // `(I-A)/t + A` recovery) — there is no transmission node here
            // anymore to also update.
            let r = RecoveryParams {
                atmos: floored,
                ..self.recovery_params.get()
            };
            self.recovery_params.set(r);
            self.graph.mark_dirty(self.dehaze_recovery_id);
        }
    }

    /// Bind (or clear) the externally-computed shared whole-image dehaze
    /// transmission (source space, bounded to `DEHAZE_MAX_TRANSMISSION_DIM`) —
    /// e.g. `EditPipeline::transmission_texture()`. ST-Task 3: this pipeline no
    /// longer computes its own per-tile transmission; `produce_tile` samples
    /// whatever was last bound here. `None` (or never called) falls back to a
    /// passthrough (identity) recovery, same as `amount == 0`. Buffer/view
    /// update only — never a pipeline rebuild (CLAUDE.md GPU rule); a no-op
    /// when `tex` is already the bound texture, so the caller (the app, after
    /// every preview re-evaluate) can call this unconditionally.
    pub fn set_shared_transmission(&mut self, tex: Option<Arc<wgpu::Texture>>) {
        self.dehaze_recovery_node.set_shared_transmission(tex);
    }

    /// Render the edited interior `TILE_SIZE`² for `coord` as an `Rgba16Float`
    /// `COPY_SRC` texture. Re-runs the whole per-tile chain (the geometry head is
    /// dirtied each call because the tile coord changed).
    ///
    /// ST-Task 3: there is no per-tile transmission to force-evaluate anymore —
    /// `dehaze_recovery_node` just samples whatever shared transmission was
    /// last bound via `set_shared_transmission`, at this tile's real
    /// output-space origin (the shared `frame` the head writes just above, in
    /// the SAME evaluate). A single `graph.evaluate(output_id)` runs the whole
    /// chain, head through the (now halo-free) dehaze recovery to sharpen.
    pub fn produce_tile(&mut self, coord: TileCoord) -> wgpu::Texture {
        self.request.set(TileRequest {
            coord,
            halo: self.halo,
        });
        // Composite the mask at THIS tile's resolution (the haloed color-chain
        // buffer), so content-dependent components (Color/Luminance range) read
        // the tile's own edited pixels. `origin` is the haloed tile origin in
        // the tile's LOD level (output) pixel space; `level_dims` is that level's
        // full size — together they map spatial components to full-image uv. The
        // apply pass then samples the mask 1:1 (no origin/LOD offset).
        let (lw, lh) = level_size(self.out_w, self.out_h, coord.lod);
        let (ox, oy) = haloed_tile_origin(coord, self.halo);
        self.local_node.set_tile_transform(Some(TileTransform {
            origin: [ox as i32, oy as i32],
            level_dims: [lw, lh],
        }));
        self.graph.mark_dirty(self.head_id);
        self.graph.mark_dirty(self.local_adjust_id);
        let haloed = self.graph.evaluate(self.output_id).clone();
        self.extract_interior(&haloed)
    }

    /// Copy the central `TILE_SIZE`² (offset by `halo`) of the haloed chain output
    /// into a fresh `COPY_SRC` texture. GPU→GPU; no readback.
    fn extract_interior(&self, haloed: &PipelineImage) -> wgpu::Texture {
        let out = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile-edit-interior"),
            size: wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PIPELINE_FORMAT,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: &haloed.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: self.halo,
                    y: self.halo,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: &out,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        self.ctx.queue.submit([enc.finish()]);
        out
    }
}
