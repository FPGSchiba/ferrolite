//! `MaskCompositor` — composite a `MaskDefinition` into one `MaskBuffer` by
//! evaluating each component (analytic shapes, range shapes sampling `input`,
//! brush dab-stamping) and folding by `CompositeMode` (+ final invert). Owns the
//! shape/brush/composite passes, built ONCE. The single source of truth for mask
//! compositing semantics: used by `ferrolite_pipeline::LocalAdjustmentsNode`
//! (the edit DAG) and `MaskOverlayCompositor` (the UI overlay).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;

use crate::buffer::{MaskBuffer, MASK_FORMAT};
use crate::model::{CompositeMode, MaskComponent, MaskDefinition};
use crate::shapes::{ColorRangePass, LinearGradientPass, LumaRangePass, RadialGradientPass};
use crate::stroke::{stroke_dabs, SPACING_FRAC};
use crate::vec::{Rgb, Vec2};
use crate::RasterStore;
use crate::{BrushRasterizer, CompositePass};

pub struct MaskCompositor {
    ctx: Arc<GpuContext>,
    linear: LinearGradientPass,
    radial: RadialGradientPass,
    luma: LumaRangePass,
    color: ColorRangePass,
    brush: BrushRasterizer,
    composite: CompositePass,
}

impl MaskCompositor {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        Self {
            linear: LinearGradientPass::new(ctx.clone()),
            radial: RadialGradientPass::new(ctx.clone()),
            luma: LumaRangePass::new(ctx.clone()),
            color: ColorRangePass::new(ctx.clone()),
            brush: BrushRasterizer::new(ctx.clone()),
            composite: CompositePass::new(ctx.clone()),
            ctx,
        }
    }

    fn ones(&self, w: u32, h: u32) -> MaskBuffer {
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

    fn eval(
        &self,
        comp: &MaskComponent,
        input: &wgpu::TextureView,
        w: u32,
        h: u32,
        rasters: &RasterStore,
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
                self.luma.run(*lo, *hi, *softness, input, w, h)
            }
            MaskComponent::ColorRange {
                samples,
                tolerance,
                softness,
            } => {
                let s: Vec<Rgb> = samples.iter().map(|c| Rgb::new(c.r, c.g, c.b)).collect();
                self.color.run(&s, *tolerance, *softness, input, w, h)
            }
            MaskComponent::Brush { strokes } => {
                let mut acc = MaskBuffer::alloc_zeroed(&self.ctx, w, h);
                for st in strokes {
                    let dabs = stroke_dabs(st, SPACING_FRAC);
                    acc = self.brush.stamp_onto(&acc, &dabs, st.erase, (0, 0), (w, h));
                }
                acc
            }
            // The AI/imported seam. Resolve the handle from the runtime RasterStore;
            // a resolved raster of matching dims folds like any other component. With
            // no producer (P1) the store is empty → inert (zeroed). A dim-mismatch is
            // also treated as absent (A2 supplies rasters at the compositing resolution).
            MaskComponent::Imported { handle, .. } => match rasters.get(*handle) {
                Some(buf) if (buf.width, buf.height) == (w, h) => buf.clone(),
                _ => MaskBuffer::alloc_zeroed(&self.ctx, w, h),
            },
        }
    }

    /// Composite `def` into one mask at `(w,h)`. Empty → ones (or zeroed if
    /// inverted); otherwise fold each component by its mode, then invert.
    pub fn composite(
        &self,
        def: &MaskDefinition,
        input: &wgpu::TextureView,
        w: u32,
        h: u32,
        rasters: &RasterStore,
    ) -> MaskBuffer {
        if def.components.is_empty() {
            return if def.invert {
                MaskBuffer::alloc_zeroed(&self.ctx, w, h)
            } else {
                self.ones(w, h)
            };
        }
        let inputs: Vec<(MaskBuffer, CompositeMode)> = def
            .components
            .iter()
            .map(|(c, m)| (self.eval(c, input, w, h, rasters), *m))
            .collect();
        self.composite.composite(&inputs, def.invert)
    }
}

/// Read a `MaskBuffer` (R32Float) back to a row-unpadded `Vec<f32>` of length w*h.
pub fn read_mask_r32f(ctx: &GpuContext, buf: &MaskBuffer) -> Vec<f32> {
    let (w, h) = (buf.width, buf.height);
    let bpp = 4u32;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bpr = (w * bpp).div_ceil(align) * align;
    let rb = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mask-readback"),
        size: (bpr * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &buf.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &rb,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
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
    let slice = rb.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    ctx.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let mut out = vec![0.0f32; (w * h) as usize];
    for row in 0..h {
        let start = (row * bpr) as usize;
        for x in 0..w {
            let o = start + x as usize * 4;
            out[(row * w + x) as usize] =
                f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
        }
    }
    drop(data);
    rb.unmap();
    let _ = MASK_FORMAT; // documents the format assumption
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CompositeMode, MaskComponent, RasterHandle};
    use crate::RasterStore;

    fn constant_buffer(ctx: &Arc<GpuContext>, w: u32, h: u32, value: f32) -> MaskBuffer {
        let buf = MaskBuffer::alloc(ctx, w, h);
        let data = vec![value; (buf.width * buf.height) as usize];
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &buf.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&data),
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

    fn imported(handle: u64) -> MaskComponent {
        use crate::model::{MaskProvenance, RasterHandle};
        MaskComponent::Imported {
            handle: RasterHandle(handle),
            provenance: MaskProvenance {
                model_id: "sam2.1".into(),
                model_version: "1".into(),
                prompt: "click:0.5,0.5".into(),
            },
        }
    }

    #[test]
    fn empty_definition_is_ones_or_zero_by_invert() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let comp = MaskCompositor::new(ctx.clone());
        // A 4x4 input (unused for empty defs).
        let input = MaskBuffer::alloc_zeroed(&ctx, 4, 4);
        let iv = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let full = comp.composite(
            &MaskDefinition {
                components: vec![],
                invert: false,
            },
            &iv,
            4,
            4,
            &RasterStore::default(),
        );
        assert!(
            read_mask_r32f(&ctx, &full)
                .iter()
                .all(|&v| (v - 1.0).abs() < 1e-4),
            "empty => ones"
        );
        let none = comp.composite(
            &MaskDefinition {
                components: vec![],
                invert: true,
            },
            &iv,
            4,
            4,
            &RasterStore::default(),
        );
        assert!(
            read_mask_r32f(&ctx, &none).iter().all(|&v| v.abs() < 1e-4),
            "empty+invert => zero"
        );
    }

    #[test]
    fn imported_composites_like_any_other_component() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let comp = MaskCompositor::new(ctx.clone());
        let input = MaskBuffer::alloc_zeroed(&ctx, 4, 4);
        let iv = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // A store holding a constant-0.6 raster for handle 7 (stands in for an AI mask).
        let store =
            RasterStore::default().with_raster(RasterHandle(7), constant_buffer(&ctx, 4, 4, 0.6));

        // 1) Single Imported → the raster values themselves.
        let single = comp.composite(
            &MaskDefinition {
                components: vec![(imported(7), CompositeMode::Add)],
                invert: false,
            },
            &iv,
            4,
            4,
            &store,
        );
        assert!(
            read_mask_r32f(&ctx, &single)
                .iter()
                .all(|&v| (v - 0.6).abs() < 1e-4),
            "single imported == raster"
        );

        // 2) Imported inverted → 1 - 0.6 = 0.4 (composites like any other, invert applies).
        let inv = comp.composite(
            &MaskDefinition {
                components: vec![(imported(7), CompositeMode::Add)],
                invert: true,
            },
            &iv,
            4,
            4,
            &store,
        );
        assert!(
            read_mask_r32f(&ctx, &inv)
                .iter()
                .all(|&v| (v - 0.4).abs() < 1e-4),
            "inverted imported == 0.4"
        );

        // 3) Full luma seed (lo=0,hi=1 → 1.0) SUBTRACT imported → 1*(1-0.6) = 0.4 ("refine for free").
        let seed = MaskComponent::LumaRange {
            lo: 0.0,
            hi: 1.0,
            softness: 0.0,
        };
        let sub = comp.composite(
            &MaskDefinition {
                components: vec![
                    (seed.clone(), CompositeMode::Add),
                    (imported(7), CompositeMode::Subtract),
                ],
                invert: false,
            },
            &iv,
            4,
            4,
            &store,
        );
        assert!(
            read_mask_r32f(&ctx, &sub)
                .iter()
                .all(|&v| (v - 0.4).abs() < 1e-4),
            "brush/range SUBTRACT imported folds like any component"
        );

        // 4) Imported INTERSECT a 0.3 constant raster (handle 9) → min(0.6, 0.3) = 0.3.
        let store2 = store.with_raster(RasterHandle(9), constant_buffer(&ctx, 4, 4, 0.3));
        let isect = comp.composite(
            &MaskDefinition {
                components: vec![
                    (imported(7), CompositeMode::Add),
                    (imported(9), CompositeMode::Intersect),
                ],
                invert: false,
            },
            &iv,
            4,
            4,
            &store2,
        );
        assert!(
            read_mask_r32f(&ctx, &isect)
                .iter()
                .all(|&v| (v - 0.3).abs() < 1e-4),
            "imported INTERSECT imported == min"
        );
    }

    #[test]
    fn imported_with_no_producer_is_inert() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let comp = MaskCompositor::new(ctx.clone());
        let input = MaskBuffer::alloc_zeroed(&ctx, 4, 4);
        let iv = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // Empty store (the P1 default: no producer) → Imported contributes zero.
        let out = comp.composite(
            &MaskDefinition {
                components: vec![(imported(7), CompositeMode::Add)],
                invert: false,
            },
            &iv,
            4,
            4,
            &RasterStore::default(),
        );
        assert!(
            read_mask_r32f(&ctx, &out).iter().all(|&v| v.abs() < 1e-4),
            "no producer => imported inert"
        );
    }
}
