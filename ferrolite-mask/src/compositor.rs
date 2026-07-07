//! `MaskCompositor` — composite a `MaskDefinition` into one `MaskBuffer` by
//! evaluating each component (analytic shapes, range shapes sampling `input`,
//! brush dab-stamping) and folding by `CompositeMode` (+ final invert). Owns the
//! shape/brush/composite passes, built ONCE. The single source of truth for mask
//! compositing semantics: used by `ferrolite_pipeline::LocalAdjustmentsNode`
//! (the edit DAG) and `MaskOverlayCompositor` (the UI overlay).

use std::hash::{Hash, Hasher};
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

    /// Empty-def coverage (extracted from `composite` for reuse by
    /// `composite_cached`): full (ones), or zeroed if inverted.
    fn empty_coverage(&self, invert: bool, w: u32, h: u32) -> MaskBuffer {
        if invert {
            MaskBuffer::alloc_zeroed(&self.ctx, w, h)
        } else {
            self.ones(w, h)
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
            return self.empty_coverage(def.invert, w, h);
        }
        let inputs: Vec<(MaskBuffer, CompositeMode)> = def
            .components
            .iter()
            .map(|(c, m)| (self.eval(c, input, w, h, rasters), *m))
            .collect();
        self.composite.composite(&inputs, def.invert)
    }

    /// Incremental composite: evaluate only components whose params changed since
    /// the last call (per `cache`), reuse the rest, then fold. Byte-identical to
    /// `composite` for the same `def`. `input_id` identifies the input image
    /// (range shapes sample it) — pass a value that changes when the input does.
    // `composite_cached` mirrors `composite`'s existing 7-param shape plus the one
    // new `input_id`/`cache` pair needed for incremental invalidation; splitting
    // these into a params struct would obscure the 1:1 correspondence with
    // `composite` that the correctness golden (and Task 3's callers) rely on.
    #[allow(clippy::too_many_arguments)]
    pub fn composite_cached(
        &self,
        def: &MaskDefinition,
        input: &wgpu::TextureView,
        input_id: u64,
        w: u32,
        h: u32,
        rasters: &RasterStore,
        cache: &mut ComponentCache,
    ) -> MaskBuffer {
        if def.components.is_empty() {
            cache.slots.clear();
            return self.empty_coverage(def.invert, w, h);
        }
        cache.reset_if_stale(input_id, (w, h));
        cache.slots.truncate(def.components.len());
        for (i, (comp, _mode)) in def.components.iter().enumerate() {
            let hash = component_hash(comp);
            match cache.slots.get(i) {
                Some((h0, _)) if *h0 == hash => { /* reuse */ }
                _ => {
                    let cov = self.eval(comp, input, w, h, rasters);
                    if i < cache.slots.len() {
                        cache.slots[i] = (hash, cov);
                    } else {
                        cache.slots.push((hash, cov));
                    }
                }
            }
        }
        let inputs: Vec<(MaskBuffer, CompositeMode)> = def
            .components
            .iter()
            .enumerate()
            .map(|(i, (_, m))| (cache.slots[i].1.clone(), *m))
            .collect();
        self.composite.composite(&inputs, def.invert)
    }
}

/// Cheap, allocation-free structural hash of a component's params (f32 by bits —
/// f32 isn't Hash). Used to detect which components changed between frames so the
/// cache re-evaluates only those. NOT serde (that was the O(n) UI-thread cost).
fn component_hash(c: &MaskComponent) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    fn f(h: &mut impl Hasher, x: f32) {
        x.to_bits().hash(h);
    }
    match c {
        MaskComponent::LinearGradient { start, end } => {
            0u8.hash(&mut h);
            f(&mut h, start.x);
            f(&mut h, start.y);
            f(&mut h, end.x);
            f(&mut h, end.y);
        }
        MaskComponent::RadialGradient {
            center,
            radius,
            rotation,
            feather,
            invert,
        } => {
            1u8.hash(&mut h);
            f(&mut h, center.x);
            f(&mut h, center.y);
            f(&mut h, radius.x);
            f(&mut h, radius.y);
            f(&mut h, *rotation);
            f(&mut h, *feather);
            invert.hash(&mut h);
        }
        MaskComponent::LumaRange { lo, hi, softness } => {
            2u8.hash(&mut h);
            f(&mut h, *lo);
            f(&mut h, *hi);
            f(&mut h, *softness);
        }
        MaskComponent::ColorRange {
            samples,
            tolerance,
            softness,
        } => {
            3u8.hash(&mut h);
            for s in samples {
                f(&mut h, s.r);
                f(&mut h, s.g);
                f(&mut h, s.b);
            }
            f(&mut h, *tolerance);
            f(&mut h, *softness);
        }
        MaskComponent::Brush { strokes } => {
            4u8.hash(&mut h);
            for st in strokes {
                st.erase.hash(&mut h);
                for n in &st.nodes {
                    f(&mut h, n.pos.x);
                    f(&mut h, n.pos.y);
                    f(&mut h, n.radius);
                    f(&mut h, n.hardness);
                    f(&mut h, n.flow);
                }
            }
        }
        MaskComponent::Imported { handle, .. } => {
            5u8.hash(&mut h);
            handle.0.hash(&mut h);
        }
    }
    h.finish()
}

/// Per-component coverage cache for incremental overlay compositing. Reused across
/// frames; only components whose `component_hash` changed are re-evaluated. Slot i
/// corresponds to component i. `input_id` guards range shapes (which sample the
/// input image): a new input clears the cache.
#[derive(Default)]
pub struct ComponentCache {
    input_id: u64,
    dims: (u32, u32),
    slots: Vec<(u64, MaskBuffer)>, // (component_hash, coverage)
}

impl ComponentCache {
    pub fn new() -> Self {
        Self::default()
    }
    /// The cached coverage of component `index`, if evaluated this generation.
    pub fn coverage(&self, index: usize) -> Option<&MaskBuffer> {
        self.slots.get(index).map(|(_, b)| b)
    }
    fn reset_if_stale(&mut self, input_id: u64, dims: (u32, u32)) {
        if self.input_id != input_id || self.dims != dims {
            self.slots.clear();
            self.input_id = input_id;
            self.dims = dims;
        }
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
    fn composite_cached_matches_full_composite_and_after_mutation() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let comp = MaskCompositor::new(ctx.clone());
        let input = MaskBuffer::alloc_zeroed(&ctx, 16, 16);
        let iv = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // A 3-component def: radial (Add), linear (Add), radial (Subtract).
        let mk = |cx: f32| MaskDefinition {
            components: vec![
                (
                    MaskComponent::RadialGradient {
                        center: crate::vec::Vec2::new(cx, 0.5),
                        radius: crate::vec::Vec2::new(0.3, 0.3),
                        rotation: 0.0,
                        feather: 0.3,
                        invert: false,
                    },
                    CompositeMode::Add,
                ),
                (
                    MaskComponent::LinearGradient {
                        start: crate::vec::Vec2::new(0.0, 0.5),
                        end: crate::vec::Vec2::new(1.0, 0.5),
                    },
                    CompositeMode::Add,
                ),
                (
                    MaskComponent::RadialGradient {
                        center: crate::vec::Vec2::new(0.7, 0.5),
                        radius: crate::vec::Vec2::new(0.2, 0.2),
                        rotation: 0.0,
                        feather: 0.3,
                        invert: false,
                    },
                    CompositeMode::Subtract,
                ),
            ],
            invert: false,
        };
        let def = mk(0.3);
        let mut cache = ComponentCache::new();
        let cached =
            comp.composite_cached(&def, &iv, 1, 16, 16, &RasterStore::default(), &mut cache);
        let full = comp.composite(&def, &iv, 16, 16, &RasterStore::default());
        assert_eq!(
            read_mask_r32f(&ctx, &cached),
            read_mask_r32f(&ctx, &full),
            "cached == full (initial)"
        );

        // Mutate ONLY the first component (move the radial center); cached must still
        // equal a fresh full composite of the mutated def (proves selective re-eval).
        let def2 = mk(0.6);
        let cached2 =
            comp.composite_cached(&def2, &iv, 1, 16, 16, &RasterStore::default(), &mut cache);
        let full2 = comp.composite(&def2, &iv, 16, 16, &RasterStore::default());
        assert_eq!(
            read_mask_r32f(&ctx, &cached2),
            read_mask_r32f(&ctx, &full2),
            "cached == full (after mutation)"
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
