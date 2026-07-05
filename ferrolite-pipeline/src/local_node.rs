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
    // tile-tier controls
    full_dims: RefCell<Option<(u32, u32)>>, // None -> use input dims
    mask_origin: RefCell<[i32; 2]>,
    cache: RefCell<Option<CachedMasks>>,
}

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

    // used by TileEditPipeline in a later task (Task 9)
    #[allow(dead_code)]
    pub(crate) fn set_mask_origin(&self, origin: [i32; 2]) {
        *self.mask_origin.borrow_mut() = origin;
    }

    // used by TileEditPipeline in a later task (Task 9)
    #[allow(dead_code)]
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
    ) -> PipelineImage {
        let dst = self.ensure_out(input, input.width, input.height);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::{AdjustmentSet, MaskLayer};
    use crate::nodes::upload_source;
    use ferrolite_image::LinearRgbaF32;
    use ferrolite_mask::MaskDefinition;

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
                    rgb = crate::uniforms::light_color_apply(rgb, &l.adjustments);
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
        let node = LocalAdjustmentsNode::new(ctx.clone(), Rc::new(RefCell::new(la)));

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
        let node = LocalAdjustmentsNode::new(ctx.clone(), Rc::new(RefCell::new(la)));

        let out1 = node.evaluate(&[&src]);
        let px1 = read_pixels(&ctx, &out1);
        let out2 = node.evaluate(&[&src]);
        let px2 = read_pixels(&ctx, &out2);
        assert_eq!(px1, px2, "repeated evaluate of the same inputs is stable");
    }
}
