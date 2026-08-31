//! `EditPipeline` + the `blit_to_rgba8` display/readback helper.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Graph, NodeId};
use ferrolite_image::LinearRgbaF32;
use wgpu::util::DeviceExt;

use crate::dehaze::estimate_atmospheric_light;
use crate::dehaze_node::{DehazeTransmissionNode, TransmissionParams};
use crate::image::PipelineImage;
use crate::lens_gpu::{VignetteTexture, WarpGridTexture};
use crate::local::{AdjustmentSet, LocalAdjustments, NoiseReduction};
use crate::local_node::{ColorDehazeParams, EngineStage, LocalAdjustmentsNode, SharedMasks};
use crate::nodes::{GeometryNode, PointOpNode, SourceNode, TileFrame, VignetteNode};
use crate::nr_node::NoiseReductionNode;
use crate::op::OpStack;
use crate::sharpen_node::SharpenNode;
use crate::uniforms::{
    color_matrix_uniform, geometry_uniform, sharpen_uniform, ColorMatrixUniform, GeometryUniform,
    LensUniform, SharpenUniform, VignetteUniform,
};

/// The retained photo edit pipeline: a `Graph<PipelineImage>` of a source node
/// feeding the fixed canonical op chain. Editing updates a shared param cell and
/// marks that op's node dirty, so only it + downstream re-evaluate.
pub struct EditPipeline {
    ctx: Arc<GpuContext>,
    graph: Graph<PipelineImage>,
    output_id: NodeId,
    color_matrix_id: NodeId,
    color_matrix: Rc<Cell<ColorMatrixUniform>>,
    // P4: noise reduction sits between `color_matrix` and `vignette` (see
    // `new`'s doc comment at the insertion point for the rationale). Global-only
    // — `nr_params` is reseeded straight from `stack.global.noise_reduction` in
    // both `new` and `set_stack`.
    nr_id: NodeId,
    nr_params: Rc<Cell<NoiseReduction>>,
    // Handle to `NoiseReductionNode`, retained for the `nr_eval_count`/
    // `nr_live_bytes` test hooks (spec §7.2/§7.4). The graph owns its own `Rc`
    // clone for evaluation — mirrors `vignette_node`'s retention rationale.
    nr_node: Rc<NoiseReductionNode>,
    vignette_id: NodeId,
    vignette: Rc<Cell<VignetteUniform>>,
    vignette_node: Rc<VignetteNode>,
    // Phase 3 (fused layer engine): the shared global two-segment `AdjustmentSet`
    // both engine-stage nodes below read from (`light_engine_node`'s
    // `light_segment()` and `local_node`'s `color_segment()` pseudo-layer) — one
    // `Rc<RefCell<_>>` so a `set_stack` write is visible to both without any
    // extra plumbing.
    global_set: Rc<RefCell<AdjustmentSet>>,
    light_engine_id: NodeId,
    // Handle to the Light-stage engine node, retained for the
    // `light_engine_eval_count` test hook (dirty-routing regression: a
    // color-segment-only or layers-only `set_stack` must NOT tick this). The
    // graph owns its own `Rc` clone for evaluation.
    #[cfg_attr(not(test), allow(dead_code))]
    light_engine_node: Rc<LocalAdjustmentsNode>,
    dehaze_transmission_id: NodeId,
    transmission_params: Rc<Cell<TransmissionParams>>,
    // Handle to the transmission node, retained only for the
    // `transmission_rebuild_count` test hook (QS-Task 4's amount-drag-caches-
    // transmission proof) — the graph owns its own `Rc` clone for evaluation.
    // Mirrors `local_node`'s retention rationale.
    #[cfg_attr(not(test), allow(dead_code))]
    dehaze_transmission_node: Rc<DehazeTransmissionNode>,
    // Phase 4 Task 2: dehaze recovery is now fused into the Color-stage engine
    // node (`local_node`, below) — there is no standalone recovery node/id
    // anymore. `color_dehaze_params` is the shared amount/atmos cell
    // `set_stack` reseeds (mirrors the retired `recovery_params` field);
    // `evaluate` still hands `local_node` the transmission node's fresh output
    // every call the same way it used to hand it to the (now-gone)
    // `DehazeRecoveryNode` (see `evaluate`'s doc — the hand-off is out-of-band,
    // not a graph edge, so it can't happen via the graph itself).
    color_dehaze_params: Rc<Cell<ColorDehazeParams>>,
    /// Whole-image atmospheric light, estimated once from the CPU source at
    /// construction (design §5.3) and reused by every `set_stack` (it is an image
    /// property, independent of the edit stack).
    dehaze_atmos: [f32; 3],
    local_adjust_id: NodeId,
    local_layers: Rc<RefCell<LocalAdjustments>>,
    // Handle to the Color-stage engine node (the old tone-curve…local-adjust
    // position, fused): the global set's `color_segment()` pseudo-layer, then
    // the per-mask-layer loop. The graph owns its own `Rc` clone for
    // evaluation; this handle is retained for the `local_rebuild_count` test
    // hook (and parity with `TileEditPipeline`, which drives the node's tile
    // controls). Read only under `cfg(test)` now that `set_stack` no longer
    // invalidates it.
    #[cfg_attr(not(test), allow(dead_code))]
    local_node: Rc<LocalAdjustmentsNode>,
    sharpen_id: NodeId,
    sharpen: Rc<Cell<SharpenUniform>>,
    // Handle to `SharpenNode`, retained for the `sharpen_eval_count` test hook
    // (Phase 4 Task 4 dirty-routing regression: a mask-layer sharpen-amount-
    // only `set_stack` must still re-run this node). The graph owns its own
    // `Box` for evaluation — mirrors `local_node`'s retention rationale.
    #[cfg_attr(not(test), allow(dead_code))]
    sharpen_node: Rc<SharpenNode>,
    geometry_id: NodeId,
    geometry: Rc<Cell<GeometryUniform>>,
    geometry_node: Rc<GeometryNode>,
    src_w: u32,
    src_h: u32,
    node_count: usize,
    stack: OpStack,
}

impl EditPipeline {
    pub fn new(
        ctx: Arc<GpuContext>,
        source: &LinearRgbaF32,
        stack: OpStack,
        camera_to_working: [[f32; 3]; 3],
    ) -> Self {
        let mut graph = Graph::new();
        let (src_w, src_h) = (source.width, source.height);
        let source_id = graph.add_node(Box::new(SourceNode::new(&ctx, source)), vec![]);

        let color_matrix = Rc::new(Cell::new(color_matrix_uniform(camera_to_working)));
        let color_matrix_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/color_matrix.wgsl"),
            "color-matrix",
            color_matrix.clone(),
        );
        let color_matrix_id = graph.add_node(Box::new(color_matrix_node), vec![source_id]);

        // P4 (design §3.1): noise reduction sits AFTER the camera→working
        // color-matrix (so the luma/chroma decomposition is in a well-defined
        // space) and BEFORE vignette (which multiplies the corners up and would
        // otherwise hand NR spatially-varying noise variance). Global-only:
        // masks are composited downstream in the Color-stage engine, so no
        // composited mask exists at this position (design §3.5).
        let nr_params = Rc::new(Cell::new(stack.global.noise_reduction));
        let nr_node = Rc::new(NoiseReductionNode::new(ctx.clone(), nr_params.clone()));
        let nr_id = graph.add_node(Box::new(nr_node.clone()), vec![color_matrix_id]);

        // Vignetting sits scene-linear at the head, before exposure (spec §6.2).
        // Default `vig_amount = 0` → identity, so an uncorrected image is unchanged.
        let vignette = Rc::new(Cell::new(VignetteUniform::default()));
        // Preview is a single whole-image texture, so it passes `None` for the
        // tile frame → the vignette shader keeps its per-texture (whole-image)
        // radius path, byte-identical to before the tiled fix.
        let vignette_node = Rc::new(VignetteNode::new(ctx.clone(), vignette.clone(), None));
        let vignette_id = graph.add_node(Box::new(vignette_node.clone()), vec![nr_id]);

        // Phase 3 (fused layer engine): the Light-stage engine node replaces the
        // old exposure/white-balance/contrast `PointOpNode` trio at this exact
        // graph position. One shared `global_set` feeds both this node's
        // `light_segment()` and the Color-stage node's `color_segment()` below.
        let global_set = Rc::new(RefCell::new(stack.global.clone()));
        // The Light stage never reads dehaze state (recovery is fused into the
        // COLOR stage only — see Task 2) — a fresh, never-mutated placeholder
        // pair is a valid stand-in, mirroring `layers`' own placeholder just
        // below.
        let light_engine_node = Rc::new(LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            // The Light stage never reads `layers` (see `evaluate_light`) — a
            // fresh, never-mutated `LocalAdjustments` is a valid placeholder.
            Rc::new(RefCell::new(LocalAdjustments::default())),
            EngineStage::Light,
            global_set.clone(),
            Rc::new(Cell::new(ColorDehazeParams::default())),
            Rc::new(Cell::new(TileFrame::default())),
            // The Light stage never populates/reads the shared-masks handle
            // (see `evaluate_light`) — a fresh, never-shared placeholder.
            Rc::new(RefCell::new(SharedMasks::default())),
        ));
        let light_engine_id =
            graph.add_node(Box::new(light_engine_node.clone()), vec![vignette_id]);

        // Halo-free dehaze (QS-Task 4, Phase 4 Task 2): the refined transmission
        // map (guided filter, expensive multi-pass) stays a separate graph node
        // so an amount-only drag never recomputes it (see
        // `transmission_rebuild_count`/`amount_change_does_not_recompute_transmission`);
        // the amount/atmos recovery+blend step is now FUSED into the Color-stage
        // engine node below (no standalone recovery node/id anymore — one less
        // full-res pass whenever both dehaze and a color-segment/mask edit are
        // active).
        let dehaze_atmos = estimate_atmospheric_light(source);
        let transmission_params = Rc::new(Cell::new(TransmissionParams::from_stack(
            &stack,
            dehaze_atmos,
        )));
        let dehaze_transmission_node = Rc::new(DehazeTransmissionNode::new(
            ctx.clone(),
            transmission_params.clone(),
        ));
        let dehaze_transmission_id = graph.add_node(
            Box::new(dehaze_transmission_node.clone()),
            vec![light_engine_id],
        );

        let color_dehaze_params = Rc::new(Cell::new(ColorDehazeParams::from_op(
            stack.dehaze(),
            dehaze_atmos,
        )));
        // Phase 4 Task 2: the fused recovery step's shared `TileFrame` — no
        // tiling here, so a dedicated frame, but NOT `TileFrame::default()`
        // (`full_dims = [0,0]`, which the shader's LOD-independent mapping
        // would divide by zero on): the whole-image tier has no LOD tiers, so
        // its "full output dims" is simply the source dims, origin `[0,0]`.
        // Mirrors the retired `DehazeRecoveryNode`'s own construction exactly.
        let color_dehaze_frame = Rc::new(Cell::new(TileFrame {
            origin: [0.0, 0.0],
            full_dims: [src_w as f32, src_h as f32],
        }));

        // Phase 3: the Color-stage engine node replaces the old tone-curve → hsl
        // → color-grade → local-adjust chain in one node at this exact graph
        // position: the global set's `color_segment()` pseudo-layer first
        // (fused dehaze recovery, THEN the color-segment point ops — Task 2),
        // then the per-mask-layer loop (unchanged mask-compositing math, now
        // keyed off this node's post-pseudo-layer `current` — see
        // `evaluate_color`). Its only graph input is `light_engine_id` directly
        // now that the standalone recovery node is gone.
        let local_layers = Rc::new(RefCell::new(stack.local_adjustments().unwrap_or_default()));
        // Phase 4 Task 4: the Color engine's composited-masks handle, shared
        // with `SharpenNode` below (constructed with a clone of this same
        // `Rc`) — see `SharedMasks`'s doc.
        let shared_masks = Rc::new(RefCell::new(SharedMasks::default()));
        let local_node = Rc::new(LocalAdjustmentsNode::new_engine(
            ctx.clone(),
            local_layers.clone(),
            EngineStage::Color,
            global_set.clone(),
            color_dehaze_params.clone(),
            color_dehaze_frame,
            shared_masks.clone(),
        ));
        // Geometry (crop/rotate) runs downstream of dehaze/color in this graph
        // (at `geometry_id`, the very end), so the fused recovery step always
        // sees the FULL source dims here — identity mapping makes source UV ==
        // whole-image UV, exactly matching the retired `DehazeRecoveryNode`'s
        // pre-fusion `(xy+0.5)/dims(img)` sampling.
        let (identity_geo, _, _) = geometry_uniform(None, src_w, src_h);
        local_node.set_geometry(identity_geo);
        let local_adjust_id = graph.add_node(Box::new(local_node.clone()), vec![light_engine_id]);

        let sharpen = Rc::new(Cell::new(sharpen_uniform(stack.sharpen())));
        // Phase 4 Task 4: `local_layers` (the SAME shared Rc the Color engine
        // reads) so SharpenNode can look up each mask layer's own
        // `adjustments.sharpen`, keyed by the index `shared_masks` carries.
        let sharpen_node = Rc::new(SharpenNode::new(
            ctx.clone(),
            sharpen.clone(),
            local_layers.clone(),
            shared_masks.clone(),
        ));
        let sharpen_id = graph.add_node(Box::new(sharpen_node.clone()), vec![local_adjust_id]);

        let (geo_uniform, _, _) = geometry_uniform(stack.geometry(), src_w, src_h);
        let geometry = Rc::new(Cell::new(geo_uniform));
        let geometry_node = Rc::new(GeometryNode::new(ctx.clone(), geometry.clone()));
        let geometry_id = graph.add_node(Box::new(geometry_node.clone()), vec![sharpen_id]);

        Self {
            ctx,
            graph,
            output_id: geometry_id,
            color_matrix_id,
            color_matrix,
            nr_id,
            nr_params,
            nr_node,
            vignette_id,
            vignette,
            vignette_node,
            global_set,
            light_engine_id,
            light_engine_node,
            dehaze_transmission_id,
            transmission_params,
            dehaze_transmission_node,
            color_dehaze_params,
            dehaze_atmos,
            local_adjust_id,
            local_layers,
            local_node,
            sharpen_id,
            sharpen,
            sharpen_node,
            geometry_id,
            geometry,
            geometry_node,
            src_w,
            src_h,
            // source, color-matrix, NR, vignette, light-engine,
            // dehaze-transmission, color-engine (recovery fused in), sharpen,
            // geometry.
            node_count: 9,
            stack,
        }
    }

    /// Bind a freshly baked lens warp grid to the geometry pass (bake-time; no
    /// pipeline rebuild). Dirties geometry so the next evaluate re-samples.
    pub fn set_warp(&mut self, warp: WarpGridTexture) {
        self.geometry_node.set_warp(warp);
        self.graph.mark_dirty(self.geometry_id);
    }

    /// Set the lens correction amounts + `use_warp` flag on the geometry pass
    /// (buffer write; no rebuild). Dirties geometry so the next evaluate applies.
    pub fn set_lens_uniform(&mut self, lens: LensUniform) {
        self.geometry_node.set_lens_uniform(lens);
        self.graph.mark_dirty(self.geometry_id);
    }

    /// Bind a freshly baked vignette gain LUT (bake-time; rebuilds the cached
    /// view, no pipeline rebuild). Dirties the vignette pass.
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

    /// Update the camera→working matrix (working-space change) and dirty the head
    /// so the chain re-runs. `m` is a row-major 3×3.
    pub fn set_color_matrix(&mut self, m: [[f32; 3]; 3]) {
        let u = color_matrix_uniform(m);
        if u != self.color_matrix.get() {
            self.color_matrix.set(u);
            self.graph.mark_dirty(self.color_matrix_id);
        }
    }

    /// Apply a new op stack, dirtying only the nodes whose params changed.
    ///
    /// Phase 3 (fused layer engine) dirty routing: `stack.global` is compared
    /// segment-wise against `self.stack.global` (the doc BEFORE this call) —
    /// a light-segment change dirties only `light_engine_id` (+ its
    /// downstream dehaze/color-engine/sharpen/geometry via the graph's own
    /// dependent-propagation); a color-segment change dirties only
    /// `local_adjust_id` (the Color-stage engine node) — a grade-only drag
    /// must NOT re-run the Light engine or the dehaze transmission node. Both
    /// comparisons happen before `global_set` is overwritten, and `global_set`
    /// is written UNCONDITIONALLY (even when neither segment changed) so it
    /// always mirrors `self.stack.global` for the next call's comparison.
    pub fn set_stack(&mut self, stack: OpStack) {
        if stack.global.light_segment() != self.stack.global.light_segment() {
            self.graph.mark_dirty(self.light_engine_id);
        }
        if stack.global.color_segment() != self.stack.global.color_segment() {
            self.graph.mark_dirty(self.local_adjust_id);
        }
        // P4: NR is global-only and lives outside the light/color segment
        // split, so it gets its own direct comparison — dirtying ONLY the NR
        // node (its downstream, vignette onward, follows via the graph's own
        // dependent-propagation).
        if self.stack.global.noise_reduction != stack.global.noise_reduction {
            self.nr_params.set(stack.global.noise_reduction);
            self.graph.mark_dirty(self.nr_id);
        }
        *self.global_set.borrow_mut() = stack.global.clone();
        // Phase 4 Task 2/3: route `radius`/`active-anywhere` to the
        // transmission node (dirtying it only when one of those actually
        // changed) and `amount`/`atmos` to the Color-stage engine node's fused
        // recovery step, independently — an amount-only change (global OR
        // per-mask) leaves the transmission MAP unchanged, so the (expensive)
        // transmission node is NOT dirtied; the Color engine still re-runs
        // (it now applies `amount`) because `color_dehaze_params` changed
        // below (global) or the layers diff changed below (per-mask).
        // `TransmissionParams::from_stack` widens `active` past the global-
        // only `dehaze()` gate — see `EditDoc::dehaze_active_anywhere` — so a
        // mask-only dehaze layer still gets a computed transmission map.
        let t = TransmissionParams::from_stack(&stack, self.dehaze_atmos);
        if t != self.transmission_params.get() {
            self.transmission_params.set(t);
            self.graph.mark_dirty(self.dehaze_transmission_id);
            // The Color engine reads the transmission's OUTPUT via an
            // out-of-band shared-texture handle, not a graph edge, so
            // `mark_dirty`'s automatic dependent-propagation no longer reaches
            // it. A transmission change (radius/atmos/active) can change that
            // texture's CONTENT in place (same `Arc`, same dims) without
            // changing its identity, so dirty the Color engine explicitly here
            // too.
            self.graph.mark_dirty(self.local_adjust_id);
        }
        let cd = ColorDehazeParams::from_op(stack.dehaze(), self.dehaze_atmos);
        if cd != self.color_dehaze_params.get() {
            self.color_dehaze_params.set(cd);
            self.graph.mark_dirty(self.local_adjust_id);
        }
        let la = stack.local_adjustments().unwrap_or_default();
        if *self.local_layers.borrow() != la {
            *self.local_layers.borrow_mut() = la;
            // NOTE: do NOT blanket-invalidate the node's mask cache here. The node
            // re-composites masks only when the mask DEFINITIONS change (keyed on
            // `mask_defs`); a mask-adjustment-only change (exposure/contrast/...)
            // must reuse the cached masks and re-run just the apply pass. A prior
            // `local_node.invalidate()` here cleared the whole cache on ANY
            // LocalAdjustments change, forcing a full re-composite every frame of a
            // mask-adjustment drag (~40-90ms/frame measured). `mark_dirty` still
            // re-runs the node so the new adjustment takes effect via `apply`.
            self.graph.mark_dirty(self.local_adjust_id);
        }
        let sh = sharpen_uniform(stack.sharpen());
        if sh != self.sharpen.get() {
            self.sharpen.set(sh);
            self.graph.mark_dirty(self.sharpen_id);
        }
        let (geo_uniform, _, _) = geometry_uniform(stack.geometry(), self.src_w, self.src_h);
        if geo_uniform != self.geometry.get() {
            self.geometry.set(geo_uniform);
            self.graph.mark_dirty(self.geometry_id);
        }
        self.stack = stack;
    }

    /// Evaluate the pipeline output (re-running only dirty nodes).
    ///
    /// `dehaze_transmission_id` is not a graph-edge ancestor of `output_id`
    /// (the Color-stage engine node reads its output via an out-of-band
    /// shared-texture handle, not a graph edge — so the same hand-off can also
    /// serve the tiled tier without a redundant per-tile transmission
    /// compute), so the graph's own lazy pull would never evaluate it. Force
    /// it via the graph (reusing its own dirty-cache — cheap when clean) and
    /// hand its current output to the Color engine BEFORE evaluating the rest
    /// of the chain, so the fused recovery step always samples the up-to-date
    /// transmission.
    pub fn evaluate(&mut self) -> PipelineImage {
        self.graph.evaluate(self.dehaze_transmission_id);
        self.local_node
            .set_shared_transmission(self.dehaze_transmission_node.current_output_texture());
        self.graph.evaluate(self.output_id).clone()
    }

    /// Total node evaluations so far (for per-op invalidation tests).
    pub fn eval_count(&self) -> usize {
        self.graph.eval_count()
    }

    /// Number of times the NR node actually dispatched (test hook: proves the
    /// identity passthrough runs no passes).
    pub fn nr_eval_count(&self) -> u32 {
        self.nr_node.eval_count()
    }

    /// GPU bytes held by the NR node's intermediates + output. Zero until the
    /// first non-identity evaluate. Instruments the spec §7.4 memory gate.
    pub fn nr_live_bytes(&self) -> u64 {
        self.nr_node.live_bytes()
    }

    /// The shared GPU context (for building overlay compositors, etc.).
    pub fn gpu_context(&self) -> Arc<GpuContext> {
        self.ctx.clone()
    }

    /// Total nodes in the graph (source + one per op). Used by invalidation tests.
    pub fn node_count(&self) -> usize {
        self.node_count
    }

    /// Number of times the `LocalAdjustmentsNode` has re-composited its masks
    /// (test hook): guards that a mask-adjustment-only `set_stack` reuses the
    /// cached masks rather than re-compositing.
    #[cfg(test)]
    pub(crate) fn local_rebuild_count(&self) -> u32 {
        self.local_node.rebuild_count()
    }

    /// Number of times the Light-stage engine node's `evaluate` has run (test
    /// hook; Phase 3 dirty-routing regression): a color-segment-only or
    /// layers-only `set_stack` must NOT tick this.
    #[cfg(test)]
    pub(crate) fn light_engine_eval_count(&self) -> u32 {
        self.light_engine_node.eval_count()
    }

    /// Number of times the Color-stage engine node's `evaluate` has run (test
    /// hook; Phase 3 dirty-routing regression): a light-segment-only
    /// `set_stack` must NOT tick this.
    #[cfg(test)]
    pub(crate) fn color_engine_eval_count(&self) -> u32 {
        self.local_node.eval_count()
    }

    /// Number of times `SharpenNode`'s `evaluate` has run (test hook; Phase 4
    /// Task 4 dirty-routing regression): a mask-layer sharpen-amount-only
    /// `set_stack` must still re-run this node (it's downstream of the Color
    /// engine, which itself re-runs since a layer-list change always dirties
    /// `local_adjust_id`), while the Color engine's mask-compositing CACHE
    /// (`local_rebuild_count`) must stay untouched (mask defs didn't change).
    #[cfg(test)]
    pub(crate) fn sharpen_eval_count(&self) -> u32 {
        self.sharpen_node.eval_count()
    }

    /// Number of times `DehazeTransmissionNode` has run its full multi-pass
    /// guided-filter evaluate (test hook; QS-Task 4): guards that an
    /// amount-only `set_stack` reuses the cached transmission map instead of
    /// recomputing it (only the cheap recovery node re-runs).
    #[cfg(test)]
    pub(crate) fn transmission_rebuild_count(&self) -> u32 {
        self.dehaze_transmission_node.transmission_rebuild_count()
    }

    /// Evaluate and read back to an sRGB Rgba8 buffer (golden tests).
    pub fn render_to_image(&mut self) -> Vec<u8> {
        let out = self.evaluate();
        blit_to_rgba8(&self.ctx, &out)
    }

    /// The current whole-image dehaze transmission texture (source space, bounded
    /// to DEHAZE_MAX_TRANSMISSION_DIM), or None when dehaze is inactive. Shared with
    /// the tiled producer so it does not recompute the transmission per tile.
    pub fn transmission_texture(&self) -> Option<std::sync::Arc<wgpu::Texture>> {
        self.dehaze_transmission_node.current_output_texture()
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlitMatrix {
    m: [[f32; 4]; 3],
}

/// Identity-matrix blit (working≡display, i.e. sRGB working space). Existing
/// golden/readback callers use this; it reduces to the old sRGB OETF path exactly.
pub fn blit_to_rgba8(ctx: &GpuContext, img: &PipelineImage) -> Vec<u8> {
    blit_to_rgba8_with_matrix(
        ctx,
        img,
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    )
}

/// Render a display-linear `PipelineImage` to an sRGB `Rgba8Unorm` buffer at 1:1,
/// applying `working_to_display` (row-major 3×3) before the sRGB OETF. Builds its
/// pipeline per call — for the test/readback path, not per-frame.
pub fn blit_to_rgba8_with_matrix(
    ctx: &GpuContext,
    img: &PipelineImage,
    working_to_display: [[f32; 3]; 3],
) -> Vec<u8> {
    let device = &ctx.device;
    let (w, h) = (img.width, img.height);

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pipeline-blit"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blit.wgsl").into()),
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("pipeline-blit-samp"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("pipeline-blit-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-blit-pl"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipeline-blit-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    let matrix_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("pipeline-blit-matrix"),
        contents: bytemuck::bytes_of(&BlitMatrix {
            m: crate::uniforms::pack_mat3(working_to_display),
        }),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let src_view = img
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("pipeline-blit-bind"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&src_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: matrix_buf.as_entire_binding(),
            },
        ],
    });

    let target = ctx.render_target(w, h, wgpu::TextureFormat::Rgba8Unorm);
    let tview = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pipeline-blit-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &tview,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
    ctx.queue.submit([enc.finish()]);
    ctx.read_rgba8(&target, w, h)
}

#[cfg(test)]
mod edit_pipeline_tests {
    use super::*;
    use crate::local::{AdjustmentSet, MaskLayer};
    use crate::op::Op;
    use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition, Vec2};

    const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    /// A stack with one mask (a linear-gradient component) whose adjustment set
    /// carries `exposure`. The mask DEFINITION is identical for every `exposure`.
    fn masked_stack(exposure: f32) -> OpStack {
        let la = LocalAdjustments {
            layers: vec![MaskLayer {
                name: "m".into(),
                visible: true,
                mask: MaskDefinition {
                    components: vec![(
                        MaskComponent::LinearGradient {
                            start: Vec2::new(0.0, 0.5),
                            end: Vec2::new(1.0, 0.5),
                        },
                        CompositeMode::Add,
                    )],
                    invert: false,
                },
                adjustments: AdjustmentSet {
                    exposure,
                    ..Default::default()
                },
            }],
        };
        OpStack::default().set_op(Op::LocalAdjustments(la))
    }

    /// A stack with one mask (a linear-gradient component, IDENTICAL across
    /// every `amount`) whose adjustment set carries a per-mask `sharpen`
    /// (Phase 4 Task 4).
    fn masked_sharpen_stack(amount: f32, radius: u32) -> OpStack {
        let la = LocalAdjustments {
            layers: vec![MaskLayer {
                name: "m".into(),
                visible: true,
                mask: MaskDefinition {
                    components: vec![(
                        MaskComponent::LinearGradient {
                            start: Vec2::new(0.0, 0.5),
                            end: Vec2::new(1.0, 0.5),
                        },
                        CompositeMode::Add,
                    )],
                    invert: false,
                },
                adjustments: AdjustmentSet {
                    sharpen: crate::op::Sharpen {
                        amount,
                        radius,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            }],
        };
        OpStack::default().set_op(Op::LocalAdjustments(la))
    }

    /// Phase 4 Task 4 dirty-routing regression: a mask-layer sharpen-AMOUNT-
    /// only change (mask def AND radius unchanged) must NOT recomposite the
    /// Color engine's masks (its cache stays keyed on mask defs, which didn't
    /// change) — but `SharpenNode` must still re-evaluate (it reads the
    /// layer's live `adjustments.sharpen.amount` fresh every time, and the
    /// Color engine's own re-run, forced by the layers-list diff in
    /// `set_stack`, refreshes the `SharedMasks` handle it depends on).
    #[test]
    fn mask_sharpen_amount_only_change_reuses_masks_but_reevaluates_sharpen() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        // Sharpen has zero effect on a FLAT source (blur == src everywhere,
        // so `amount*(src-blur) == 0` regardless of amount) — needs real
        // spatial variance, unlike the flat fixtures the exposure-only
        // `masked_stack` tests use.
        let (w, h) = (8u32, 8u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 0.2 } else { 0.8 };
                px.extend_from_slice(&[v, v, v, 1.0]);
            }
        }
        let src = LinearRgbaF32::new(w, h, px).unwrap();
        let mut ep = EditPipeline::new(ctx, &src, masked_sharpen_stack(0.5, 2), IDENTITY);

        let out1 = ep.render_to_image();
        assert_eq!(
            ep.local_rebuild_count(),
            1,
            "first evaluate composites the mask once"
        );
        assert_eq!(
            ep.sharpen_eval_count(),
            1,
            "first evaluate runs SharpenNode once"
        );

        // Change ONLY the mask layer's sharpen amount; mask def AND radius
        // are unchanged.
        ep.set_stack(masked_sharpen_stack(1.5, 2));
        let out2 = ep.render_to_image();
        assert_eq!(
            ep.local_rebuild_count(),
            1,
            "sharpen-amount-only change must REUSE the cached masks"
        );
        assert_eq!(
            ep.sharpen_eval_count(),
            2,
            "sharpen-amount-only change must still re-run SharpenNode"
        );
        assert_ne!(
            out1, out2,
            "the new sharpen amount must actually change the rendered output"
        );
    }

    /// Regression for the mask-adjustment lag: `set_stack` must NOT blanket-clear
    /// the composited-mask cache on a mask-adjustment-only change (it did via
    /// `local_node.invalidate()`, forcing a full re-composite every frame of an
    /// Exposure/Contrast drag). This exercises the REAL `set_stack` path — the
    /// Task-1 node-level test mutated the layers Rc directly and so missed it.
    #[test]
    fn mask_adjustment_only_change_reuses_composited_masks() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = LinearRgbaF32::new(8, 8, vec![0.5; 8 * 8 * 4]).unwrap();
        let mut ep = EditPipeline::new(ctx, &src, masked_stack(0.2), IDENTITY);

        let _ = ep.evaluate();
        assert_eq!(
            ep.local_rebuild_count(),
            1,
            "first evaluate composites the mask once"
        );

        // Change ONLY the mask's adjustment (exposure); mask def is identical.
        ep.set_stack(masked_stack(0.9));
        let _ = ep.evaluate();
        assert_eq!(
            ep.local_rebuild_count(),
            1,
            "mask-adjustment-only change must REUSE the cached masks"
        );

        // Sanity: a mask-DEFINITION change still recomposites.
        let mut la = masked_stack(0.9).local_adjustments().unwrap();
        la.layers[0].mask.components[0].0 = MaskComponent::LinearGradient {
            start: Vec2::new(0.0, 0.0),
            end: Vec2::new(0.0, 1.0),
        };
        ep.set_stack(OpStack::default().set_op(Op::LocalAdjustments(la)));
        let _ = ep.evaluate();
        assert_eq!(
            ep.local_rebuild_count(),
            2,
            "a mask-def change recomposites"
        );
    }

    /// QS-Task 4: an `amount`-only dehaze edit must reuse the cached refined
    /// transmission map (the expensive multi-pass guided filter) and re-run
    /// only the cheap recovery/blend step in the Color-stage engine node; a
    /// `radius` change must recompute the transmission map. This is the "amount
    /// drag skips transmission" proof that motivated keeping the transmission
    /// computation separate (now with the recovery fused into the Color-stage).
    #[test]
    fn amount_change_does_not_recompute_transmission() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = LinearRgbaF32::new(16, 16, vec![0.6; 16 * 16 * 4]).unwrap();

        let dehaze_stack = |amount: f32, radius: u32| {
            OpStack::default().set_op(Op::Dehaze(crate::op::Dehaze { amount, radius }))
        };

        let mut ep = EditPipeline::new(ctx, &src, dehaze_stack(0.5, 8), IDENTITY);
        let _ = ep.evaluate();
        assert_eq!(
            ep.transmission_rebuild_count(),
            1,
            "first evaluate computes the transmission map once"
        );

        // Amount-only change (same radius): transmission must be REUSED.
        ep.set_stack(dehaze_stack(0.9, 8));
        let _ = ep.evaluate();
        assert_eq!(
            ep.transmission_rebuild_count(),
            1,
            "amount-only change must NOT recompute the transmission map"
        );

        // Radius change: transmission must recompute.
        ep.set_stack(dehaze_stack(0.9, 12));
        let _ = ep.evaluate();
        assert_eq!(
            ep.transmission_rebuild_count(),
            2,
            "a radius change must recompute the transmission map"
        );
    }

    /// Phase 4 Task 2 Step 3 (dirty-routing regression): the fused recovery
    /// step now lives on the Color-stage engine node, so a dehaze AMOUNT-only
    /// change must dirty ONLY that node — NOT the (expensive) transmission
    /// node, and NOT the Light-stage engine node (dehaze is not a light-
    /// segment field). A RADIUS change must dirty the transmission node (and,
    /// transitively, the Color engine, since its input's content changed) but
    /// must still leave the Light engine untouched.
    #[test]
    fn dehaze_amount_dirties_color_engine_only_radius_also_dirties_transmission() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = LinearRgbaF32::new(16, 16, vec![0.6; 16 * 16 * 4]).unwrap();

        let dehaze_stack = |amount: f32, radius: u32| {
            OpStack::default().set_op(Op::Dehaze(crate::op::Dehaze { amount, radius }))
        };

        let mut ep = EditPipeline::new(ctx, &src, dehaze_stack(0.5, 8), IDENTITY);
        let _ = ep.evaluate();
        let (light_before, color_before, trans_before) = (
            ep.light_engine_eval_count(),
            ep.color_engine_eval_count(),
            ep.transmission_rebuild_count(),
        );

        // Amount-only change: transmission must NOT recompute; the Light
        // engine must NOT re-run; the Color engine (where the fused recovery
        // now applies `amount`) MUST re-run.
        ep.set_stack(dehaze_stack(0.9, 8));
        let _ = ep.evaluate();
        assert_eq!(
            ep.transmission_rebuild_count(),
            trans_before,
            "amount-only change must not recompute the transmission map"
        );
        assert_eq!(
            ep.light_engine_eval_count(),
            light_before,
            "amount-only change must not re-run the Light engine"
        );
        assert_eq!(
            ep.color_engine_eval_count(),
            color_before + 1,
            "amount-only change must re-run the Color engine (fused recovery)"
        );

        // Radius change: transmission recomputes (and the Color engine
        // re-runs as its downstream consumer), but the Light engine still
        // must not re-run.
        let (light_before2, color_before2, trans_before2) = (
            ep.light_engine_eval_count(),
            ep.color_engine_eval_count(),
            ep.transmission_rebuild_count(),
        );
        ep.set_stack(dehaze_stack(0.9, 16));
        let _ = ep.evaluate();
        assert_eq!(
            ep.transmission_rebuild_count(),
            trans_before2 + 1,
            "radius change must recompute the transmission map"
        );
        assert_eq!(
            ep.light_engine_eval_count(),
            light_before2,
            "radius change must not re-run the Light engine"
        );
        assert_eq!(
            ep.color_engine_eval_count(),
            color_before2 + 1,
            "radius change must re-run the Color engine (samples the new transmission)"
        );
    }

    /// Regression for the dehaze two-node-split perf bug: with NO `Dehaze` op in
    /// the stack, `DehazeTransmissionNode` must never run its expensive
    /// multi-pass guided filter, even as unrelated upstream ops (exposure,
    /// contrast) keep changing on every `set_stack`. Before this fix,
    /// `TransmissionParams` had no way to know dehaze was off, so ANY upstream
    /// change (which reaches this node only via the graph's dirty-propagation,
    /// but `set_stack` also re-seeds `TransmissionParams` from the stack's
    /// dehaze-active state every call) looked identical to "dehaze just got
    /// enabled" and re-ran the
    /// full guided filter for nothing.
    #[test]
    fn no_dehaze_op_skips_transmission_passes() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = LinearRgbaF32::new(16, 16, vec![0.6; 16 * 16 * 4]).unwrap();

        let stack = OpStack::default().set_op(Op::Exposure(crate::op::Exposure { ev: 0.3 }));
        let mut ep = EditPipeline::new(ctx, &src, stack, IDENTITY);
        let _ = ep.evaluate();
        assert_eq!(
            ep.transmission_rebuild_count(),
            0,
            "no dehaze op in the stack: transmission must not run at all"
        );

        // Unrelated upstream edit (contrast); still no dehaze op anywhere.
        let stack = OpStack::default()
            .set_op(Op::Exposure(crate::op::Exposure { ev: 0.3 }))
            .set_op(Op::Contrast(crate::op::Contrast { amount: 0.4 }));
        ep.set_stack(stack);
        let _ = ep.evaluate();
        assert_eq!(
            ep.transmission_rebuild_count(),
            0,
            "an upstream (non-dehaze) edit must NOT trigger the guided filter \
             when dehaze is off"
        );

        // Now enable dehaze: the transmission map must compute exactly once.
        let stack = OpStack::default()
            .set_op(Op::Exposure(crate::op::Exposure { ev: 0.3 }))
            .set_op(Op::Contrast(crate::op::Contrast { amount: 0.4 }))
            .set_op(Op::Dehaze(crate::op::Dehaze {
                amount: 0.5,
                radius: 8,
            }));
        ep.set_stack(stack);
        let _ = ep.evaluate();
        assert_eq!(
            ep.transmission_rebuild_count(),
            1,
            "turning dehaze on must compute the transmission map"
        );
    }

    #[test]
    fn transmission_texture_present_only_when_dehaze_active() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = LinearRgbaF32::new(32, 24, vec![0.5; 32 * 24 * 4]).unwrap();
        // No dehaze → no transmission texture.
        let mut ep = EditPipeline::new(ctx.clone(), &src, OpStack::default(), IDENTITY);
        let _ = ep.evaluate();
        assert!(ep.transmission_texture().is_none());
        // Dehaze active → a transmission texture exists.
        let stack = OpStack::default().set_op(crate::op::Op::Dehaze(crate::op::Dehaze {
            amount: 0.6,
            radius: 8,
        }));
        ep.set_stack(stack);
        let _ = ep.evaluate();
        assert!(ep.transmission_texture().is_some());
    }

    /// Phase 4 Task 3: a MASK-ONLY dehaze layer (global `Dehaze` op absent, so
    /// `stack.dehaze()` is `None`) must still get the shared whole-image
    /// transmission computed — the wiring concern the task brief called out
    /// explicitly: `TransmissionParams` used to gate purely on the global op,
    /// so a mask-only dehaze amount would have silently never triggered the
    /// transmission node, leaving the per-mask recovery step permanently
    /// identity (no transmission bound). Constructed via `set_op` (not
    /// `EditPipeline::new`) so the doc's default global radius still governs
    /// (see `EditDoc::dehaze_active_anywhere` / `TransmissionParams::from_stack`).
    #[test]
    fn mask_only_dehaze_still_computes_transmission() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = LinearRgbaF32::new(32, 24, vec![0.5; 32 * 24 * 4]).unwrap();

        let la = LocalAdjustments {
            layers: vec![MaskLayer {
                name: "m".into(),
                visible: true,
                mask: MaskDefinition::default(),
                adjustments: AdjustmentSet {
                    dehaze: crate::op::Dehaze {
                        amount: 0.4,
                        radius: 8,
                    },
                    ..Default::default()
                },
            }],
        };
        let stack = OpStack::default().set_op(Op::LocalAdjustments(la));
        assert!(
            stack.dehaze().is_none(),
            "sanity: no GLOBAL dehaze op is present"
        );
        assert!(
            stack.dehaze_active_anywhere(),
            "sanity: the mask layer's dehaze amount activates the doc-wide gate"
        );

        let mut ep = EditPipeline::new(ctx, &src, stack, IDENTITY);
        let _ = ep.evaluate();
        assert_eq!(
            ep.transmission_rebuild_count(),
            1,
            "a mask-only dehaze layer must compute the transmission map"
        );
        assert!(
            ep.transmission_texture().is_some(),
            "a mask-only dehaze layer must yield a bound transmission texture"
        );
    }

    /// ST-Task 2 review fix (round 1), still exercised post-Phase-4-Task-2: a
    /// pixel-level regression proving the out-of-band transmission→recovery
    /// hand-off (`set_shared_transmission` + the explicit
    /// `mark_dirty(local_adjust_id)` in `set_stack`'s transmission-change
    /// branch) actually propagates into the RECOVERED OUTPUT of a LIVE
    /// pipeline, not just that
    /// `transmission_rebuild_count()` incremented or `transmission_texture()`
    /// is present. Every existing dehaze test either discards `evaluate()`'s
    /// output (`let _ = ep.evaluate();`) or does a single fresh-evaluate golden
    /// — neither exercises "change the transmission on an already-evaluated,
    /// LIVE pipeline via `set_stack`, then read back different pixels", which
    /// is exactly the class of stale-hand-off bug this rework risks (recovery
    /// silently keeps sampling the OLD shared texture after a radius change).
    ///
    /// Fixture mirrors the golden `dehaze_no_halo_on_dark_edge`/
    /// `dehaze_positive_increases_contrast_on_hazy_image` fixtures: a thin sky
    /// band (seeds a realistic atmospheric light `A`) over a dark/bright edge
    /// field — enough spatial structure that the guided-filter radius change
    /// (4 -> 16) measurably moves the refined transmission map, unlike a flat
    /// fixture where every radius produces the same trivial (constant)
    /// transmission.
    #[test]
    fn radius_change_propagates_to_recovered_output() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);

        let (w, h) = (128usize, 32usize);
        let sky_rows = 4usize;
        let edge = 64usize;
        let (sky, field, dark) = (1.0f32, 0.4f32, 0.05f32);
        let mut px = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let v = if y < sky_rows {
                    sky
                } else if x < edge {
                    dark
                } else {
                    field
                };
                px.extend_from_slice(&[v, v, v, 1.0]);
            }
        }
        let src = LinearRgbaF32::new(w as u32, h as u32, px).expect("hazy edge fixture");

        let small_radius = OpStack::default().set_op(Op::Dehaze(crate::op::Dehaze {
            amount: 1.0,
            radius: 4,
        }));
        let mut ep = EditPipeline::new(ctx.clone(), &src, small_radius, IDENTITY);
        let a = ep.render_to_image();

        // Live `set_stack` on the SAME pipeline instance — this is the path that
        // relies on `set_shared_transmission`/the explicit
        // `mark_dirty(local_adjust_id)` to propagate the new transmission into
        // the fused recovery step's output, rather than constructing a fresh
        // pipeline.
        let large_radius = OpStack::default().set_op(Op::Dehaze(crate::op::Dehaze {
            amount: 1.0,
            radius: 16,
        }));
        ep.set_stack(large_radius);
        let b = ep.render_to_image();

        assert_eq!(a.len(), b.len());
        let mut max_diff = 0i32;
        for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
            for c in 0..3 {
                let d = (pa[c] as i32 - pb[c] as i32).abs();
                max_diff = max_diff.max(d);
            }
        }
        eprintln!("radius_change_propagates_to_recovered_output: max abs u8 diff = {max_diff}");
        assert!(
            max_diff > 3,
            "a live radius change (4 -> 16) via set_stack on the SAME EditPipeline \
             must propagate through set_shared_transmission into different recovered \
             pixels; max abs diff (u8) = {max_diff}, expected > 3 — a stale hand-off \
             would leave this at 0"
        );
    }

    /// Phase 3 (fused layer engine) dirty-routing regression, Light-engine
    /// side: a light-segment-only `set_stack` (Exposure) re-runs the
    /// Light-stage engine node AND (correctly — its input texture changed)
    /// the downstream Color-stage engine node, but must NOT force the Color
    /// engine to re-composite its masks — an unrelated upstream light-segment
    /// change is exactly the "mask-adjustment-only" case the compositing
    /// cache (keyed on mask defs [+ color segment, see `local_node.rs`'s
    /// `CachedMasks`]) already guards; this proves the graph-level downstream
    /// re-run doesn't defeat that cache.
    #[test]
    fn light_segment_only_change_reruns_color_engine_without_recompositing_masks() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = LinearRgbaF32::new(8, 8, vec![0.5; 8 * 8 * 4]).unwrap();
        let base = masked_stack(0.2).set_op(Op::Exposure(crate::op::Exposure { ev: 0.2 }));
        let mut ep = EditPipeline::new(ctx, &src, base, IDENTITY);
        let _ = ep.evaluate();
        assert_eq!(
            ep.local_rebuild_count(),
            1,
            "first evaluate composites the mask once"
        );
        let (light_before, color_before) =
            (ep.light_engine_eval_count(), ep.color_engine_eval_count());

        // Change ONLY the global (light-segment) exposure; the masked layer's
        // own adjustments and mask definition are untouched.
        ep.set_stack(masked_stack(0.2).set_op(Op::Exposure(crate::op::Exposure { ev: 0.9 })));
        let _ = ep.evaluate();
        assert_eq!(
            ep.light_engine_eval_count(),
            light_before + 1,
            "a light-segment change must re-run the Light engine"
        );
        assert_eq!(
            ep.color_engine_eval_count(),
            color_before + 1,
            "the downstream Color engine also re-runs (its input texture changed)"
        );
        assert_eq!(
            ep.local_rebuild_count(),
            1,
            "an unrelated upstream light-segment change must NOT recomposite masks"
        );
    }

    /// Phase 3 dirty-routing regression: a color-segment-only `set_stack`
    /// (ToneCurve) must re-run the Color-stage engine node but NOT the
    /// Light-stage engine node — the "grade-only drag must not re-run the
    /// Light engine or dehaze transmission" guarantee the plan requires,
    /// checked from the Color-engine side (the transmission-side half is
    /// already covered by `no_dehaze_op_skips_transmission_passes` /
    /// `amount_change_does_not_recompute_transmission`, both unaffected by
    /// this task since the Light engine still feeds the transmission node the
    /// same way the old `contrast_id` did).
    #[test]
    fn color_segment_only_change_does_not_dirty_light_engine() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = LinearRgbaF32::new(8, 8, vec![0.5; 8 * 8 * 4]).unwrap();
        let base = OpStack::default().set_op(Op::ToneCurve(crate::op::ToneCurve {
            points: vec![(0.0, 0.0), (0.5, 0.4), (1.0, 1.0)],
            ..Default::default()
        }));
        let mut ep = EditPipeline::new(ctx, &src, base, IDENTITY);
        let _ = ep.evaluate();
        let (light_before, color_before) =
            (ep.light_engine_eval_count(), ep.color_engine_eval_count());

        ep.set_stack(
            OpStack::default().set_op(Op::ToneCurve(crate::op::ToneCurve {
                points: vec![(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)],
                ..Default::default()
            })),
        );
        let _ = ep.evaluate();
        assert_eq!(
            ep.color_engine_eval_count(),
            color_before + 1,
            "a color-segment-only change must re-run the Color engine"
        );
        assert_eq!(
            ep.light_engine_eval_count(),
            light_before,
            "a color-segment-only change must NOT re-run the Light engine"
        );
    }
}
