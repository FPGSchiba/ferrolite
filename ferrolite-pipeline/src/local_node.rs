//! `LocalAdjustmentsNode` — the whole masked-adjustment stage as one
//! `Node<PipelineImage>`. Per visible layer: (engine) composite the
//! `MaskDefinition` into a single `MaskBuffer`, then (photo) apply the Light+Color
//! point op blended by the mask. Inserted after `Hsl`, before `Sharpen`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Node};
use ferrolite_mask::{
    stroke_dabs, BrushRasterizer, ColorRangePass, CompositeMode, CompositePass, LinearGradientPass,
    LumaRangePass, MaskBuffer, MaskComponent, MaskDefinition, RadialGradientPass, Rgb, Vec2,
};
use wgpu::util::DeviceExt;

use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::local::LocalAdjustments;
use crate::uniforms::{local_adjust_uniform, LocalAdjustUniform};

struct CachedMasks {
    layers: LocalAdjustments,
    full_dims: (u32, u32),
    masks: Vec<MaskBuffer>, // one per visible layer, in visible order
}

pub(crate) struct LocalAdjustmentsNode {
    ctx: Arc<GpuContext>,
    layers: Rc<RefCell<LocalAdjustments>>,
    // build-once passes
    linear: LinearGradientPass,
    radial: RadialGradientPass,
    luma: LumaRangePass,
    color: ColorRangePass,
    brush: BrushRasterizer,
    composite: CompositePass,
    // apply pass
    apply_bgl: wgpu::BindGroupLayout,
    apply_pipeline: wgpu::ComputePipeline,
    apply_out: RefCell<Option<PipelineImage>>,
    // tile-tier controls
    full_dims: RefCell<Option<(u32, u32)>>, // None -> use input dims
    mask_origin: RefCell<[i32; 2]>,
    cache: RefCell<Option<CachedMasks>>,
}

// `LocalAdjustmentsNode` is wired into `EditPipeline`/`TileEditPipeline` in a
// later task (Task 7); until then nothing constructs it, so its API is
// intentionally allowed to look unused here.
#[allow(dead_code)]
impl LocalAdjustmentsNode {
    pub(crate) fn new(ctx: Arc<GpuContext>, layers: Rc<RefCell<LocalAdjustments>>) -> Self {
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
        Self {
            linear: LinearGradientPass::new(ctx.clone()),
            radial: RadialGradientPass::new(ctx.clone()),
            luma: LumaRangePass::new(ctx.clone()),
            color: ColorRangePass::new(ctx.clone()),
            brush: BrushRasterizer::new(ctx.clone()),
            composite: CompositePass::new(ctx.clone()),
            apply_bgl,
            apply_pipeline,
            apply_out: RefCell::new(None),
            full_dims: RefCell::new(None),
            mask_origin: RefCell::new([0, 0]),
            cache: RefCell::new(None),
            ctx,
            layers,
        }
    }

    pub(crate) fn set_mask_origin(&self, origin: [i32; 2]) {
        *self.mask_origin.borrow_mut() = origin;
    }

    pub(crate) fn set_full_dims(&self, dims: (u32, u32)) {
        let mut fd = self.full_dims.borrow_mut();
        if *fd != Some(dims) {
            *fd = Some(dims);
            self.cache.borrow_mut().take();
        }
    }

    /// Invalidate the cached composited masks (call when `layers` change).
    pub(crate) fn invalidate(&self) {
        self.cache.borrow_mut().take();
    }

    fn ones_mask(&self, w: u32, h: u32) -> MaskBuffer {
        let buf = MaskBuffer::alloc(&self.ctx, w, h);
        let ones = vec![1.0f32; (buf.width * buf.height) as usize];
        self.ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &buf.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&ones),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(buf.width * 4),
                rows_per_image: Some(buf.height),
            },
            wgpu::Extent3d {
                width: buf.width,
                height: buf.height,
                depth_or_array_layers: 1,
            },
        );
        buf
    }

    fn eval_component(
        &self,
        comp: &MaskComponent,
        color_view: &wgpu::TextureView,
        w: u32,
        h: u32,
    ) -> MaskBuffer {
        match comp {
            MaskComponent::LinearGradient { start, end } => {
                self.linear
                    .run(Vec2::new(start.x, start.y), Vec2::new(end.x, end.y), w, h)
            }
            MaskComponent::RadialGradient {
                center,
                radius,
                rotation,
                feather,
                invert,
            } => self.radial.run(
                Vec2::new(center.x, center.y),
                Vec2::new(radius.x, radius.y),
                *rotation,
                *feather,
                *invert,
                w,
                h,
            ),
            MaskComponent::LumaRange { lo, hi, softness } => {
                self.luma.run(*lo, *hi, *softness, color_view, w, h)
            }
            MaskComponent::ColorRange {
                samples,
                tolerance,
                softness,
            } => {
                let s: Vec<Rgb> = samples.iter().map(|c| Rgb::new(c.r, c.g, c.b)).collect();
                self.color.run(&s, *tolerance, *softness, color_view, w, h)
            }
            MaskComponent::Brush { strokes } => {
                let mut acc = MaskBuffer::alloc_zeroed(&self.ctx, w, h);
                for st in strokes {
                    let dabs = stroke_dabs(st, ferrolite_mask::SPACING_FRAC);
                    acc = self.brush.stamp_onto(&acc, &dabs, st.erase, (0, 0), (w, h));
                }
                acc
            }
            // Inert in P1 (no producer) — contributes nothing. Plan 5 wires it.
            MaskComponent::Imported { .. } => MaskBuffer::alloc_zeroed(&self.ctx, w, h),
        }
    }

    fn composite_mask(
        &self,
        def: &MaskDefinition,
        color_view: &wgpu::TextureView,
        w: u32,
        h: u32,
    ) -> MaskBuffer {
        if def.components.is_empty() {
            return if def.invert {
                MaskBuffer::alloc_zeroed(&self.ctx, w, h)
            } else {
                self.ones_mask(w, h)
            };
        }
        let inputs: Vec<(MaskBuffer, CompositeMode)> = def
            .components
            .iter()
            .map(|(c, m)| (self.eval_component(c, color_view, w, h), *m))
            .collect();
        self.composite.composite(&inputs, def.invert)
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.apply_out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("local-adjust-out"),
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

    fn apply(
        &self,
        input: &PipelineImage,
        mask: &MaskBuffer,
        u: LocalAdjustUniform,
    ) -> PipelineImage {
        let dst = self.ensure_out(input.width, input.height);
        let ubuf = self
            .ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("local-adjust-uniform"),
                contents: bytemuck::bytes_of(&u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let src_view = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mask_view = mask
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
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
}

impl Node<PipelineImage> for LocalAdjustmentsNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let input = inputs[0];
        let layers = self.layers.borrow();
        if layers.is_identity() {
            return input.clone();
        }
        // Mask compositing resolution: full output dims (tile tier) or input dims.
        let (mw, mh) = self
            .full_dims
            .borrow()
            .unwrap_or((input.width, input.height));
        let input_view = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // (Re)build the composited-mask cache if layers/full_dims changed.
        let rebuild = {
            let c = self.cache.borrow();
            match &*c {
                Some(cm) => cm.layers != *layers || cm.full_dims != (mw, mh),
                None => true,
            }
        };
        if rebuild {
            let masks: Vec<MaskBuffer> = layers
                .visible_layers()
                .map(|l| self.composite_mask(&l.mask, &input_view, mw, mh))
                .collect();
            *self.cache.borrow_mut() = Some(CachedMasks {
                layers: layers.clone(),
                full_dims: (mw, mh),
                masks,
            });
        }
        let cache = self.cache.borrow();
        let cm = cache.as_ref().unwrap();

        let origin = *self.mask_origin.borrow();
        let mut current = input.clone();
        for (layer, mask) in layers.visible_layers().zip(cm.masks.iter()) {
            let mut u = local_adjust_uniform(&layer.adjustments);
            u.mask_origin = origin;
            current = self.apply(&current, mask, u);
        }
        current
    }
}

impl Node<PipelineImage> for Rc<LocalAdjustmentsNode> {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        (**self).evaluate(inputs)
    }
}
