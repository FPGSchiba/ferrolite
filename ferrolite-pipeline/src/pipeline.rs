//! `EditPipeline` + the `blit_to_rgba8` display/readback helper.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Graph, NodeId};
use ferrolite_image::LinearRgbaF32;
use wgpu::util::DeviceExt;

use crate::dehaze::estimate_atmospheric_light;
use crate::dehaze_node::{
    DehazeRecoveryNode, DehazeTransmissionNode, RecoveryParams, TransmissionParams,
};
use crate::image::PipelineImage;
use crate::lens_gpu::{VignetteTexture, WarpGridTexture};
use crate::local::LocalAdjustments;
use crate::local_node::LocalAdjustmentsNode;
use crate::nodes::{CurveNode, GeometryNode, PointOpNode, SourceNode, TileFrame, VignetteNode};
use crate::op::OpStack;
use crate::uniforms::{
    color_grade_uniform, color_matrix_uniform, contrast_uniform, exposure_uniform,
    geometry_uniform, hsl_uniform, sharpen_uniform, tone_curve_luts, wb_uniform, ColorGradeUniform,
    ColorMatrixUniform, ContrastUniform, ExposureUniform, GeometryUniform, HslUniform, LensUniform,
    SharpenUniform, VignetteUniform, WbUniform,
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
    vignette_id: NodeId,
    vignette: Rc<Cell<VignetteUniform>>,
    vignette_node: Rc<VignetteNode>,
    exposure_id: NodeId,
    exposure: Rc<Cell<ExposureUniform>>,
    wb_id: NodeId,
    wb: Rc<Cell<WbUniform>>,
    contrast_id: NodeId,
    contrast: Rc<Cell<ContrastUniform>>,
    dehaze_transmission_id: NodeId,
    transmission_params: Rc<Cell<TransmissionParams>>,
    // Handle to the transmission node, retained only for the
    // `transmission_rebuild_count` test hook (QS-Task 4's amount-drag-caches-
    // transmission proof) — the graph owns its own `Rc` clone for evaluation.
    // Mirrors `local_node`'s retention rationale.
    #[cfg_attr(not(test), allow(dead_code))]
    dehaze_transmission_node: Rc<DehazeTransmissionNode>,
    dehaze_recovery_id: NodeId,
    recovery_params: Rc<Cell<RecoveryParams>>,
    // Handle to the recovery node, retained so `evaluate` can hand it the
    // transmission node's fresh output every call (ST-Task 2: the recovery node
    // is no longer a graph-edge dependent of `dehaze_transmission_id` — see
    // `evaluate`'s doc — so this hand-off can't happen via the graph itself).
    dehaze_recovery_node: Rc<DehazeRecoveryNode>,
    /// Whole-image atmospheric light, estimated once from the CPU source at
    /// construction (design §5.3) and reused by every `set_stack` (it is an image
    /// property, independent of the edit stack).
    dehaze_atmos: [f32; 3],
    tone_curve_id: NodeId,
    tone_curve: Rc<Cell<[[f32; 256]; 3]>>,
    hsl_id: NodeId,
    hsl: Rc<Cell<HslUniform>>,
    color_grade_id: NodeId,
    color_grade: Rc<Cell<ColorGradeUniform>>,
    local_adjust_id: NodeId,
    local_layers: Rc<RefCell<LocalAdjustments>>,
    // Handle to the local-adjustments node. The graph owns its own `Rc` clone for
    // evaluation; this handle is retained for the `local_rebuild_count` test hook
    // (and parity with `TileEditPipeline`, which drives the node's tile controls).
    // Read only under `cfg(test)` now that `set_stack` no longer invalidates it.
    #[cfg_attr(not(test), allow(dead_code))]
    local_node: Rc<LocalAdjustmentsNode>,
    sharpen_id: NodeId,
    sharpen: Rc<Cell<SharpenUniform>>,
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

        // Vignetting sits scene-linear at the head, before exposure (spec §6.2).
        // Default `vig_amount = 0` → identity, so an uncorrected image is unchanged.
        let vignette = Rc::new(Cell::new(VignetteUniform::default()));
        // Preview is a single whole-image texture, so it passes `None` for the
        // tile frame → the vignette shader keeps its per-texture (whole-image)
        // radius path, byte-identical to before the tiled fix.
        let vignette_node = Rc::new(VignetteNode::new(ctx.clone(), vignette.clone(), None));
        let vignette_id = graph.add_node(Box::new(vignette_node.clone()), vec![color_matrix_id]);

        let exposure = Rc::new(Cell::new(exposure_uniform(stack.exposure())));
        let exposure_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/exposure.wgsl"),
            "exposure",
            exposure.clone(),
        );
        let exposure_id = graph.add_node(Box::new(exposure_node), vec![vignette_id]);

        let wb = Rc::new(Cell::new(wb_uniform(stack.white_balance())));
        let wb_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/white_balance.wgsl"),
            "white-balance",
            wb.clone(),
        );
        let wb_id = graph.add_node(Box::new(wb_node), vec![exposure_id]);

        let contrast = Rc::new(Cell::new(contrast_uniform(stack.contrast())));
        let contrast_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/contrast.wgsl"),
            "contrast",
            contrast.clone(),
        );
        let contrast_id = graph.add_node(Box::new(contrast_node), vec![wb_id]);

        // Halo-free dehaze (QS-Task 4): the refined transmission map (guided
        // filter, expensive multi-pass) and the amount/atmos recovery+blend
        // (cheap single pass) are separate graph nodes so an amount-only drag
        // dirties only the recovery node — the transmission node's dirty-cache
        // means it is NOT recomputed (see `transmission_rebuild_count`/
        // `amount_change_does_not_recompute_transmission`).
        let dehaze_atmos = estimate_atmospheric_light(source);
        let transmission_params = Rc::new(Cell::new(TransmissionParams::from_op(
            stack.dehaze(),
            dehaze_atmos,
        )));
        let dehaze_transmission_node = Rc::new(DehazeTransmissionNode::new(
            ctx.clone(),
            transmission_params.clone(),
        ));
        let dehaze_transmission_id = graph.add_node(
            Box::new(dehaze_transmission_node.clone()),
            vec![contrast_id],
        );

        let recovery_params = Rc::new(Cell::new(RecoveryParams::from_op(
            stack.dehaze(),
            dehaze_atmos,
        )));
        // ST-Task 2: the recovery node takes only `I` (contrast_id) as a graph
        // input now — the transmission is bound out-of-band via
        // `set_shared_transmission` (see `evaluate`), not a graph edge, so the
        // shared texture can later also serve the tiled tier. No tiling here, so
        // a dedicated frame — but NOT `TileFrame::default()` (`full_dims =
        // [0,0]`, which the shader's LOD-independent mapping would divide by
        // zero on): the whole-image tier has no LOD tiers, so its "full output
        // dims" is simply the source dims, origin `[0,0]`.
        let dehaze_recovery_node = Rc::new(DehazeRecoveryNode::new(
            ctx.clone(),
            recovery_params.clone(),
            Rc::new(Cell::new(TileFrame {
                origin: [0.0, 0.0],
                full_dims: [src_w as f32, src_h as f32],
            })),
        ));
        // Geometry (crop/rotate) runs downstream of dehaze in this graph (at
        // `geometry_id`, the very end), so recovery always sees the FULL source
        // dims here — identity mapping makes source UV == whole-image UV,
        // exactly matching the pre-ST-Task-2 `(xy+0.5)/dims(img)` sampling.
        let (identity_geo, _, _) = geometry_uniform(None, src_w, src_h);
        dehaze_recovery_node.set_geometry(identity_geo);
        let dehaze_recovery_id =
            graph.add_node(Box::new(dehaze_recovery_node.clone()), vec![contrast_id]);

        let tone_curve = Rc::new(Cell::new(tone_curve_luts(stack.tone_curve().as_ref())));
        let tone_curve_node = CurveNode::new(ctx.clone(), tone_curve.clone());
        let tone_curve_id = graph.add_node(Box::new(tone_curve_node), vec![dehaze_recovery_id]);

        let hsl = Rc::new(Cell::new(hsl_uniform(stack.hsl())));
        let hsl_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/hsl.wgsl"),
            "hsl",
            hsl.clone(),
        );
        let hsl_id = graph.add_node(Box::new(hsl_node), vec![tone_curve_id]);

        let color_grade = Rc::new(Cell::new(color_grade_uniform(stack.color_grade())));
        let color_grade_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/color_grade.wgsl"),
            "color-grade",
            color_grade.clone(),
        );
        let color_grade_id = graph.add_node(Box::new(color_grade_node), vec![hsl_id]);

        let local_layers = Rc::new(RefCell::new(stack.local_adjustments().unwrap_or_default()));
        let local_node = Rc::new(LocalAdjustmentsNode::new(ctx.clone(), local_layers.clone()));
        let local_adjust_id = graph.add_node(Box::new(local_node.clone()), vec![color_grade_id]);

        let sharpen = Rc::new(Cell::new(sharpen_uniform(stack.sharpen())));
        let sharpen_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/sharpen.wgsl"),
            "sharpen",
            sharpen.clone(),
        );
        let sharpen_id = graph.add_node(Box::new(sharpen_node), vec![local_adjust_id]);

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
            vignette_id,
            vignette,
            vignette_node,
            exposure_id,
            exposure,
            wb_id,
            wb,
            contrast_id,
            contrast,
            dehaze_transmission_id,
            transmission_params,
            dehaze_transmission_node,
            dehaze_recovery_id,
            recovery_params,
            dehaze_recovery_node,
            dehaze_atmos,
            tone_curve_id,
            tone_curve,
            hsl_id,
            hsl,
            color_grade_id,
            color_grade,
            local_adjust_id,
            local_layers,
            local_node,
            sharpen_id,
            sharpen,
            geometry_id,
            geometry,
            geometry_node,
            src_w,
            src_h,
            node_count: 14,
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
    pub fn set_stack(&mut self, stack: OpStack) {
        let e = exposure_uniform(stack.exposure());
        if e != self.exposure.get() {
            self.exposure.set(e);
            self.graph.mark_dirty(self.exposure_id);
        }
        let w = wb_uniform(stack.white_balance());
        if w != self.wb.get() {
            self.wb.set(w);
            self.graph.mark_dirty(self.wb_id);
        }
        let c = contrast_uniform(stack.contrast());
        if c != self.contrast.get() {
            self.contrast.set(c);
            self.graph.mark_dirty(self.contrast_id);
        }
        // Route `radius`/`atmos` to the transmission node (dirtying it only when
        // one of those actually changed) and `amount`/`atmos` to the recovery
        // node, independently — an amount-only change leaves `t` unchanged, so
        // the (expensive) transmission node is NOT dirtied; the graph still
        // re-runs recovery (its downstream consumer) because `r` changed.
        let t = TransmissionParams::from_op(stack.dehaze(), self.dehaze_atmos);
        if t != self.transmission_params.get() {
            self.transmission_params.set(t);
            self.graph.mark_dirty(self.dehaze_transmission_id);
            // ST-Task 2: the recovery node reads the transmission's OUTPUT via an
            // out-of-band shared-texture handle, not a graph edge, so
            // `mark_dirty`'s automatic dependent-propagation no longer reaches
            // it. A transmission change (radius/atmos/active) can change that
            // texture's CONTENT in place (same `Arc`, same dims) without
            // changing its identity, so dirty recovery explicitly here too.
            self.graph.mark_dirty(self.dehaze_recovery_id);
        }
        let r = RecoveryParams::from_op(stack.dehaze(), self.dehaze_atmos);
        if r != self.recovery_params.get() {
            self.recovery_params.set(r);
            self.graph.mark_dirty(self.dehaze_recovery_id);
        }
        let luts = tone_curve_luts(stack.tone_curve().as_ref());
        if luts != self.tone_curve.get() {
            self.tone_curve.set(luts);
            self.graph.mark_dirty(self.tone_curve_id);
        }
        let h = hsl_uniform(stack.hsl());
        if h != self.hsl.get() {
            self.hsl.set(h);
            self.graph.mark_dirty(self.hsl_id);
        }
        let cg = color_grade_uniform(stack.color_grade());
        if cg != self.color_grade.get() {
            self.color_grade.set(cg);
            self.graph.mark_dirty(self.color_grade_id);
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
    /// ST-Task 2: `DehazeRecoveryNode` is no longer a graph-edge dependent of
    /// `dehaze_transmission_id` (so the same shared-texture hand-off can also
    /// serve the tiled tier without a redundant per-tile transmission compute).
    /// That means `dehaze_transmission_id` is no longer an ancestor of
    /// `output_id`, so the graph's own lazy pull would never evaluate it. Force
    /// it via the graph (reusing its own dirty-cache — cheap when clean) and
    /// hand its current output to the recovery node BEFORE evaluating the rest
    /// of the chain, so recovery always samples the up-to-date transmission.
    pub fn evaluate(&mut self) -> PipelineImage {
        self.graph.evaluate(self.dehaze_transmission_id);
        self.dehaze_recovery_node
            .set_shared_transmission(self.dehaze_transmission_node.current_output_texture());
        self.graph.evaluate(self.output_id).clone()
    }

    /// Total node evaluations so far (for per-op invalidation tests).
    pub fn eval_count(&self) -> usize {
        self.graph.eval_count()
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
    /// only the cheap recovery/blend node; a `radius` change must recompute the
    /// transmission map. This is the "amount drag skips transmission" proof
    /// that motivated splitting the old single-pass dehaze `PointOpNode` into
    /// `DehazeTransmissionNode` + `DehazeRecoveryNode`.
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

    /// Regression for the dehaze two-node-split perf bug: with NO `Dehaze` op in
    /// the stack, `DehazeTransmissionNode` must never run its expensive
    /// multi-pass guided filter, even as unrelated upstream ops (exposure,
    /// contrast) keep changing on every `set_stack`. Before this fix,
    /// `TransmissionParams` had no way to know dehaze was off, so ANY upstream
    /// change (which reaches this node only via the graph's dirty-propagation,
    /// but `set_stack` also re-seeds `TransmissionParams` from `stack.dehaze()`
    /// every call) looked identical to "dehaze just got enabled" and re-ran the
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

    /// ST-Task 2 review fix (round 1): a pixel-level regression proving the
    /// out-of-band transmission→recovery hand-off (`set_shared_transmission` +
    /// the explicit `mark_dirty(dehaze_recovery_id)` in `set_stack`) actually
    /// propagates into the RECOVERED OUTPUT of a LIVE pipeline, not just that
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
        // `mark_dirty(dehaze_recovery_id)` to propagate the new transmission
        // into recovery's output, rather than constructing a fresh pipeline.
        let large_radius = OpStack::default().set_op(Op::Dehaze(crate::op::Dehaze {
            amount: 1.0,
            radius: 16,
        }));
        ep.set_stack(large_radius);
        let b = ep.render_to_image();

        assert_eq!(a.len(), b.len());
        let mut max_diff = 0i32;
        for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
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
}
