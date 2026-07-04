//! `TileEditPipeline` — the per-tile, full-res GPU edit producer. For each
//! requested tile it runs geometry-at-the-head (resampling the GPU-resident
//! source for the haloed output tile) then the color chain (exposure→WB→contrast
//! →tone-curve→HSL→sharpen) over the haloed buffer, and returns the interior
//! `TILE_SIZE`² as an `Rgba16Float` `COPY_SRC` texture for the VT to copy into a
//! pool slot. No CPU readback (spec §5.2).
//!
//! Geometry is applied at the head (spec §8.4). For identity geometry the head is
//! a 1:1 haloed copy, so the result is identical to the whole-image Plan-2 chain
//! and to a whole-image render — this is what the tile-seam golden asserts. For
//! non-identity geometry, Sharpen operates in output space rather than source
//! space, an accepted pragmatic difference (architecture map §2).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Graph, NodeId};
use ferrolite_image::{TileCoord, TILE_SIZE};

use crate::gpu_pyramid::GpuPyramidSource;
use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::lens_gpu::{VignetteTexture, WarpGridTexture};
use crate::nodes::{CurveNode, GeometryHeadNode, PointOpNode, TileRequest, VignetteNode};
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

        let mut graph = Graph::new();
        let head = Rc::new(GeometryHeadNode::new(
            ctx.clone(),
            source,
            geometry,
            request.clone(),
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
        let vignette_node = Rc::new(VignetteNode::new(ctx.clone(), vignette.clone()));
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
        let sharpen = Rc::new(Cell::new(sharpen_uniform(stack.sharpen())));
        let sharpen_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/sharpen.wgsl"),
                "sharpen",
                sharpen.clone(),
            )),
            vec![hsl_id],
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
            pad: [0.0; 3],
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
            sharpen,
        }
    }

    pub fn halo(&self) -> u32 {
        self.halo
    }

    /// Re-derive the color-op param cells (exposure, white balance, contrast,
    /// tone curve, HSL, sharpen amount) from `stack` and dirty the chain so the
    /// next `produce_tile` re-renders.
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
    /// the lens/vignette setters (no rebuild).
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
    pub fn set_vig_amount(&mut self, amount: f32) {
        let u = VignetteUniform {
            vig_amount: amount,
            pad: [0.0; 3],
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
        self.graph.mark_dirty(self.head_id);
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
