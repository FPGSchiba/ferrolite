//! `TileEditPipeline` — the per-tile, full-res GPU edit producer. For each
//! requested tile it runs geometry-at-the-head (resampling the GPU-resident
//! source for the haloed output tile) then the color chain (exposure→WB→contrast
//! →tone-curve→HSL→LocalAdjustments→sharpen) over the haloed buffer, and returns
//! the interior `TILE_SIZE`² as an `Rgba16Float` `COPY_SRC` texture for the VT to
//! copy into a pool slot. No CPU readback (spec §5.2).
//!
//! Geometry is applied at the head (spec §8.4). For identity geometry the head is
//! a 1:1 haloed copy, so the result is identical to the whole-image Plan-2 chain
//! and to a whole-image render — this is what the tile-seam golden asserts. For
//! non-identity geometry, Sharpen operates in output space rather than source
//! space, an accepted pragmatic difference (architecture map §2).
//!
//! **LocalAdjustments — output-space mask, pragmatic limitation:** because
//! geometry runs at the head, the entire color chain (including
//! `LocalAdjustments`) operates in **output space**, not source space. The
//! node's mask is composited ONCE per document at the full **output**
//! resolution (`set_full_dims`, fixed at construction from
//! `edited_output_dims`) and cached; each `produce_tile` call only updates the
//! per-tile `mask_origin` (a cheap uniform write) so the shader samples the
//! correct sub-region — the mask itself is never rebuilt per tile. For
//! identity/translation geometry this is exact and matches the whole-image
//! preview render bit-for-bit (within float tolerance). Under crop/rotate the
//! mask anchors to the cropped/rotated **output** frame rather than the
//! source frame — the same accepted difference already noted above for
//! Sharpen. Materializing the mask at full output resolution is a pragmatic
//! P1 memory/compute cost (one extra full-frame buffer per visible layer,
//! rebuilt only when the layers change via `set_stack` → `invalidate`); a
//! later optimization could stream/tile mask evaluation instead.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Graph, NodeId};
use ferrolite_image::{TileCoord, TILE_SIZE};

use crate::gpu_pyramid::GpuPyramidSource;
use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::lens_gpu::{VignetteTexture, WarpGridTexture};
use crate::local::LocalAdjustments;
use crate::local_node::LocalAdjustmentsNode;
use crate::nodes::{
    CurveNode, GeometryHeadNode, PointOpNode, TileFrame, TileRequest, VignetteNode,
};
use crate::op::{Aspect, CropRect, Geometry, LensCorrection, OpStack};
use crate::uniforms::{
    color_matrix_uniform, contrast_uniform, curve_lut, exposure_uniform, hsl_uniform, lens_halo_px,
    sharpen_halo, sharpen_uniform, ColorMatrixUniform, ContrastUniform, ExposureUniform,
    HslUniform, LensUniform, SharpenUniform, VignetteUniform, WbUniform,
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
    // Param cells (set from the stack; Plan 4 mutates via set_stack).
    exposure: Rc<Cell<ExposureUniform>>,
    wb: Rc<Cell<WbUniform>>,
    contrast: Rc<Cell<ContrastUniform>>,
    tone_curve: Rc<Cell<[f32; 256]>>,
    hsl: Rc<Cell<HslUniform>>,
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
        // origin + full output dims each evaluate; the vignette node READS it so its
        // radius is measured in full-image space (seamless, not per-tile). The graph
        // runs head → vignette in the same evaluate, so the frame is always current.
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
            Some(frame),
        ));
        let vignette_id = graph.add_node(Box::new(vignette_node.clone()), vec![color_matrix_id]);

        let exposure = Rc::new(Cell::new(exposure_uniform(stack.exposure())));
        let exposure_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/exposure.wgsl"),
                "exposure",
                exposure.clone(),
            )),
            vec![vignette_id],
        );
        let wb = Rc::new(Cell::new(crate::uniforms::wb_uniform(
            stack.white_balance(),
        )));
        let wb_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/white_balance.wgsl"),
                "white-balance",
                wb.clone(),
            )),
            vec![exposure_id],
        );
        let contrast = Rc::new(Cell::new(contrast_uniform(stack.contrast())));
        let contrast_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/contrast.wgsl"),
                "contrast",
                contrast.clone(),
            )),
            vec![wb_id],
        );
        let tone_curve = Rc::new(Cell::new(curve_lut(
            &stack.tone_curve().map(|t| t.points).unwrap_or_default(),
            stack
                .tone_curve()
                .map(|t| t.mode)
                .unwrap_or(crate::op::CurveMode::Linear),
        )));
        let tone_curve_id = graph.add_node(
            Box::new(CurveNode::new(ctx.clone(), tone_curve.clone())),
            vec![contrast_id],
        );
        let hsl = Rc::new(Cell::new(hsl_uniform(stack.hsl())));
        let hsl_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/hsl.wgsl"),
                "hsl",
                hsl.clone(),
            )),
            vec![tone_curve_id],
        );
        let local_layers = Rc::new(RefCell::new(stack.local_adjustments().unwrap_or_default()));
        let local_node = Rc::new(LocalAdjustmentsNode::new(ctx.clone(), local_layers.clone()));
        let (out_w, out_h) = crate::edited_output_dims(&stack, src_w, src_h);
        local_node.set_full_dims((out_w, out_h));
        let local_adjust_id = graph.add_node(Box::new(local_node.clone()), vec![hsl_id]);

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
            exposure,
            wb,
            contrast,
            tone_curve,
            hsl,
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
    /// the lens/vignette setters (no rebuild). The `LocalAdjustments` full-output
    /// mask resolution (`set_full_dims`) is likewise derived from the stack's
    /// geometry at construction time and fixed thereafter — a geometry/output-dims
    /// change requires the same full rebuild, not just a `set_stack` call.
    pub fn set_stack(&mut self, stack: OpStack) {
        self.exposure.set(exposure_uniform(stack.exposure()));
        self.wb
            .set(crate::uniforms::wb_uniform(stack.white_balance()));
        self.contrast.set(contrast_uniform(stack.contrast()));
        self.tone_curve.set(curve_lut(
            &stack.tone_curve().map(|t| t.points).unwrap_or_default(),
            stack
                .tone_curve()
                .map(|t| t.mode)
                .unwrap_or(crate::op::CurveMode::Linear),
        ));
        self.hsl.set(hsl_uniform(stack.hsl()));
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

    /// Render the edited interior `TILE_SIZE`² for `coord` as an `Rgba16Float`
    /// `COPY_SRC` texture. Re-runs the whole per-tile chain (the geometry head is
    /// dirtied each call because the tile coord changed).
    pub fn produce_tile(&mut self, coord: TileCoord) -> wgpu::Texture {
        self.request.set(TileRequest {
            coord,
            halo: self.halo,
        });
        // The color chain (including LocalAdjustments) runs over the haloed
        // tile buffer of extent `haloed_tile_extent(halo)`, and
        // `extract_interior` later copies the interior at offset `halo`. The
        // full-output mask was composited once at construction (`set_full_dims`);
        // shift the per-tile origin by `-halo` so `textureLoad(mask, mask_origin
        // + xy)` in the apply shader lands on the correct full-output pixels for
        // every haloed-buffer coordinate `xy`, including the halo border itself.
        // This can be negative at the top/left output edges (and can exceed the
        // mask dims at the right/bottom edges). `textureLoad` does NOT clamp
        // out-of-bounds coordinates under wgpu robustness (it returns 0), so the
        // apply shader explicitly clamps the sampled coordinate to the mask's
        // bounds, edge-replicating the mask across the halo. This matches the
        // color halo, which `GeometryHeadNode` fills via ClampToEdge sampling of
        // the source — so tiled Sharpen (which reads the halo) agrees with the
        // whole-image render at image edges.
        let gx = coord.x as i32 * TILE_SIZE as i32 - self.halo as i32;
        let gy = coord.y as i32 * TILE_SIZE as i32 - self.halo as i32;
        self.local_node.set_mask_origin([gx, gy]);
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
