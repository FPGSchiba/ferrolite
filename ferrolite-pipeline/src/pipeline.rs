//! `EditPipeline` + the `blit_to_rgba8` display/readback helper.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{Graph, GpuContext, NodeId};
use ferrolite_image::LinearRgbaF32;

use crate::image::PipelineImage;
use crate::nodes::{PointOpNode, SourceNode};
use crate::op::OpStack;
use crate::uniforms::{contrast_uniform, exposure_uniform, wb_uniform, ContrastUniform, ExposureUniform, WbUniform};

/// The retained photo edit pipeline: a `Graph<PipelineImage>` of a source node
/// feeding the fixed canonical op chain. Editing updates a shared param cell and
/// marks that op's node dirty, so only it + downstream re-evaluate.
pub struct EditPipeline {
    ctx: Arc<GpuContext>,
    graph: Graph<PipelineImage>,
    output_id: NodeId,
    exposure_id: NodeId,
    exposure: Rc<Cell<ExposureUniform>>,
    wb_id: NodeId,
    wb: Rc<Cell<WbUniform>>,
    contrast_id: NodeId,
    contrast: Rc<Cell<ContrastUniform>>,
    stack: OpStack,
}

impl EditPipeline {
    pub fn new(ctx: Arc<GpuContext>, source: &LinearRgbaF32, stack: OpStack) -> Self {
        let mut graph = Graph::new();
        let source_id = graph.add_node(Box::new(SourceNode::new(&ctx, source)), vec![]);

        let exposure = Rc::new(Cell::new(exposure_uniform(stack.exposure())));
        let exposure_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/exposure.wgsl"),
            "exposure",
            exposure.clone(),
        );
        let exposure_id = graph.add_node(Box::new(exposure_node), vec![source_id]);

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

        Self {
            ctx,
            graph,
            output_id: contrast_id,
            exposure_id,
            exposure,
            wb_id,
            wb,
            contrast_id,
            contrast,
            stack,
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
        self.stack = stack;
    }

    /// Evaluate the pipeline output (re-running only dirty nodes).
    pub fn evaluate(&mut self) -> PipelineImage {
        self.graph.evaluate(self.output_id).clone()
    }

    /// Total node evaluations so far (for per-op invalidation tests).
    pub fn eval_count(&self) -> usize {
        self.graph.eval_count()
    }

    /// Evaluate and read back to an sRGB Rgba8 buffer (golden tests).
    pub fn render_to_image(&mut self) -> Vec<u8> {
        let out = self.evaluate();
        blit_to_rgba8(&self.ctx, &out)
    }
}

/// Render a display-linear `PipelineImage` to an sRGB `Rgba8Unorm` buffer at 1:1
/// (its own dims), returning `width*height*4` row-unpadded bytes. Used by golden
/// tests and (later) any CPU-side preview/export readback. Builds its pipeline
/// per call — acceptable for the test/readback path (not the per-frame path).
pub fn blit_to_rgba8(ctx: &GpuContext, img: &PipelineImage) -> Vec<u8> {
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

    let src_view = img.texture.create_view(&wgpu::TextureViewDescriptor::default());
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
