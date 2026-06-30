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
use crate::nodes::{CurveNode, GeometryHeadNode, PointOpNode, TileRequest};
use crate::op::{Aspect, CropRect, Geometry, OpStack};
use crate::uniforms::{
    contrast_uniform, curve_lut, exposure_uniform, hsl_uniform, sharpen_halo, sharpen_uniform,
    ContrastUniform, ExposureUniform, HslUniform, SharpenUniform, WbUniform,
};

pub struct TileEditPipeline {
    ctx: Arc<GpuContext>,
    graph: Graph<PipelineImage>,
    output_id: NodeId,
    request: Rc<Cell<TileRequest>>,
    head_id: NodeId,
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
    pub fn new(ctx: Arc<GpuContext>, source: Arc<GpuPyramidSource>, stack: OpStack) -> Self {
        let halo = sharpen_halo(stack.sharpen());
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
        let head = GeometryHeadNode::new(ctx.clone(), source, geometry, request.clone());
        let head_id = graph.add_node(Box::new(head), vec![]);

        let exposure = Rc::new(Cell::new(exposure_uniform(stack.exposure())));
        let exposure_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/exposure.wgsl"),
                "exposure",
                exposure.clone(),
            )),
            vec![head_id],
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

        Self {
            ctx,
            graph,
            output_id: sharpen_id,
            request,
            head_id,
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
    /// LIMITATION: the geometry transform (crop/rotate) and the sharpen **halo**
    /// are fixed at construction (baked into the `GeometryHeadNode` and the haloed
    /// extent). `set_stack` does NOT update them. If `stack.geometry()` changes or
    /// `sharpen_halo(stack.sharpen())` differs from the current `halo()`, this
    /// pipeline must be DISCARDED and rebuilt with `TileEditPipeline::new` — calling
    /// `set_stack` alone will silently keep the old geometry/halo. (A later plan that
    /// wires interactive edits is responsible for that rebuild decision.)
    pub fn set_stack(&mut self, stack: OpStack) {
        self.exposure.set(exposure_uniform(stack.exposure()));
        self.wb
            .set(crate::uniforms::wb_uniform(stack.white_balance()));
        self.contrast.set(contrast_uniform(stack.contrast()));
        self.tone_curve.set(curve_lut(
            &stack.tone_curve().map(|t| t.points).unwrap_or_default(),
        ));
        self.hsl.set(hsl_uniform(stack.hsl()));
        self.sharpen.set(sharpen_uniform(stack.sharpen()));
        self.graph.mark_dirty(self.head_id);
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
