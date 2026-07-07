# Tiled Range-Mask Composite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make content-dependent mask components (Color range, Luminance range) render correctly in the tiled/idle full-res view, by compositing each visible layer's mask **per tile** against that tile's own edited content instead of once at full-output resolution against a single tile's pixels.

**Architecture:** The mask compositor currently evaluates every component into a full-output-resolution buffer and each tile samples a sub-region via a `mask_origin`/`mask_lod` offset. Spatial components (gradient/radial/brush) are pure functions of the normalized output coordinate, so that works. But `LumaRange`/`ColorRange` shaders read image content with `textureLoad(src, xy)` — in the tiled pipeline `src` is a single ~256px tile, not the full image, so the composited mask is garbage. Fix: composite each layer's mask at the **tile's own (haloed) resolution**, sampling the tile input directly (range components become exact), and give the spatial shape passes a uv scale+offset so their normalized parameters still map into full-image space. The apply pass then samples the mask 1:1 with the tile, so the `mask_origin`/`mask_lod` machinery is removed.

**Tech Stack:** Rust, wgpu compute shaders (WGSL), `ferrolite-mask` (mask compositing), `ferrolite-pipeline` (edit DAG / tiled producer). GPU goldens run only where `GpuContext::headless()` returns `Some` (dev machine has a GPU; headless CI auto-skips).

## Global Constraints

- **`ferrolite-gpu` and `ferrolite-vt` MUST NOT be modified** (source-agnostic crates, architecture contract §4/§5). All changes live in `ferrolite-mask` and `ferrolite-pipeline`.
- **Never block the UI/update thread; build GPU pipelines ONCE and reuse** (CLAUDE.md). All shape/apply pipelines are already build-once; keep them so — only uniform buffers are rewritten per run. Per-tile compositing must stay bounded to the tile size (do not introduce any full-image O(all-pixels) mask buffer).
- **Whole-image (preview/overlay) behavior must stay byte-identical** (within float tolerance). The identity transform `TileTransform::whole_image(w, h)` MUST reduce every code path to exactly today's behavior; existing `ferrolite-mask` and `ferrolite-pipeline` goldens must keep passing unchanged in intent.
- **Immutability / small focused files** (CLAUDE.md coding style): new `TileTransform` lives in its own small module.
- **Green gate before finishing:** `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace` all clean, THEN hand the author a visual test and hold.

## File Structure

- `ferrolite-mask/src/tile_transform.rs` — **new**. `TileTransform { origin: [i32;2], level_dims: [u32;2] }` + `whole_image(w,h)` + `uv_scale_offset(w,h)`. One responsibility: map a composite-buffer coordinate to full-image-normalized uv and supply brush origin/level-dims.
- `ferrolite-mask/src/lib.rs` — export `TileTransform`.
- `ferrolite-mask/src/shapes/linear.rs`, `ferrolite-mask/src/shaders/linear_gradient.wgsl` — add `uv_scale`/`uv_offset` to the uniform + shader.
- `ferrolite-mask/src/shapes/radial.rs`, `ferrolite-mask/src/shaders/radial_gradient.wgsl` — same.
- `ferrolite-mask/src/compositor.rs` — thread `tile: TileTransform` through `eval`, `composite`, `composite_cached`.
- `ferrolite-pipeline/src/mask_overlay.rs` — pass `TileTransform::whole_image(w,h)` at the one `composite_cached` call.
- `ferrolite-pipeline/src/local_node.rs` — replace `set_full_dims`/`set_mask_origin`/`set_mask_lod` with `set_tile_transform(Option<TileTransform>)`; composite at input dims with the transform; cache only the whole-image path.
- `ferrolite-pipeline/src/uniforms.rs`, `ferrolite-pipeline/src/shaders/local_adjust.wgsl` — remove `mask_origin`/`mask_lod`/`_pad` tail; sample mask 1:1.
- `ferrolite-pipeline/src/tile_edit.rs` — store output dims; per tile call `set_tile_transform`; update doc comments.
- `ferrolite-pipeline/tests/local_golden.rs` — port the spatial lod-1 golden to the new API; add range-mask (luma + color) tiled-vs-whole-image goldens.

---

### Task 1: `TileTransform` + uv-mapped spatial shape passes + per-tile compositor API

**Files:**
- Create: `ferrolite-mask/src/tile_transform.rs`
- Modify: `ferrolite-mask/src/lib.rs` (add `mod tile_transform;` + re-export)
- Modify: `ferrolite-mask/src/shapes/linear.rs`, `ferrolite-mask/src/shaders/linear_gradient.wgsl`
- Modify: `ferrolite-mask/src/shapes/radial.rs`, `ferrolite-mask/src/shaders/radial_gradient.wgsl`
- Modify: `ferrolite-mask/src/compositor.rs` (`eval`, `composite`, `composite_cached`, and the in-file `#[cfg(test)] mod tests` call sites)
- Test: unit tests inside `ferrolite-mask/src/compositor.rs` and `ferrolite-mask/src/tile_transform.rs`

**Interfaces:**
- Produces:
  - `pub struct TileTransform { pub origin: [i32; 2], pub level_dims: [u32; 2] }`
  - `impl TileTransform { pub fn whole_image(w: u32, h: u32) -> Self; pub fn uv_scale_offset(&self, w: u32, h: u32) -> ([f32; 2], [f32; 2]); }`
  - `MaskCompositor::composite(&self, def, input, w, h, rasters, tile: TileTransform) -> MaskBuffer`
  - `MaskCompositor::composite_cached(&self, def, input, input_id, w, h, rasters, cache, tile: TileTransform) -> MaskBuffer` (new last param `tile`)
  - `LinearGradientPass::run(&self, start, end, uv_scale: [f32;2], uv_offset: [f32;2], width, height) -> MaskBuffer`
  - `RadialGradientPass::run(&self, center, radius, rotation, feather, invert, uv_scale: [f32;2], uv_offset: [f32;2], width, height) -> MaskBuffer`
- Consumes: existing `LumaRangePass`/`ColorRangePass`/`BrushRasterizer` unchanged.

- [ ] **Step 1: Write the failing test for `TileTransform::uv_scale_offset`**

Add to a new `ferrolite-mask/src/tile_transform.rs` bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_image_is_identity_uv() {
        let t = TileTransform::whole_image(100, 60);
        let (scale, offset) = t.uv_scale_offset(100, 60);
        assert_eq!(scale, [1.0, 1.0]);
        assert_eq!(offset, [0.0, 0.0]);
    }

    #[test]
    fn tile_maps_composite_uv_to_full_image_uv() {
        // A 40x40 composite buffer placed at level-pixel origin (100, 20) inside
        // a 400x400 level. uv_full = (origin + composite_px) / level_dims.
        let t = TileTransform {
            origin: [100, 20],
            level_dims: [400, 400],
        };
        let (scale, offset) = t.uv_scale_offset(40, 40);
        // scale = extent/level = 40/400 = 0.1 ; offset = origin/level = 0.25, 0.05
        assert!((scale[0] - 0.1).abs() < 1e-6);
        assert!((scale[1] - 0.1).abs() < 1e-6);
        assert!((offset[0] - 0.25).abs() < 1e-6);
        assert!((offset[1] - 0.05).abs() < 1e-6);
        // A composite-local uv of 0.5 (pixel center of the 40px buffer) maps to
        // full uv = 0.5*0.1 + 0.25 = 0.30 == (100 + 20)/400.
        let uv_full_x = 0.5 * scale[0] + offset[0];
        assert!((uv_full_x - 0.30).abs() < 1e-6);
    }
}
```

- [ ] **Step 2: Create `TileTransform` and run the test to verify it fails then passes**

Create `ferrolite-mask/src/tile_transform.rs`:

```rust
//! Tile-space placement for compositing a mask into a sub-region of the full
//! image. Spatial shape passes are pure functions of a full-image-normalized
//! uv; when a mask is composited at a tile's own (haloed) resolution, this maps
//! each composite-buffer pixel back to the full-image uv the shape expects, and
//! supplies the brush rasterizer's tile origin + level dims. `whole_image` is
//! the identity used by the preview / UI-overlay paths (composite spans the
//! whole level 1:1), which reduces every consumer to its pre-tiling behavior.

/// Placement of a composite buffer within its full image level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileTransform {
    /// Haloed tile origin in the tile's LOD level pixel space (may be negative
    /// at the top/left edges).
    pub origin: [i32; 2],
    /// Full dimensions of the tile's LOD level (pixels).
    pub level_dims: [u32; 2],
}

impl TileTransform {
    /// Identity: the composite buffer IS the whole level, 1:1.
    pub fn whole_image(w: u32, h: u32) -> Self {
        Self {
            origin: [0, 0],
            level_dims: [w, h],
        }
    }

    /// uv scale + offset mapping a composite-local uv in `[0,1]^2` (over the
    /// `w`x`h` composite buffer) to full-image-normalized uv:
    /// `uv_full = uv_local * scale + offset`. For `whole_image(w,h)` this is
    /// `scale = [1,1]`, `offset = [0,0]`.
    pub fn uv_scale_offset(&self, w: u32, h: u32) -> ([f32; 2], [f32; 2]) {
        let lw = self.level_dims[0].max(1) as f32;
        let lh = self.level_dims[1].max(1) as f32;
        (
            [w as f32 / lw, h as f32 / lh],
            [self.origin[0] as f32 / lw, self.origin[1] as f32 / lh],
        )
    }
}
```

Add to `ferrolite-mask/src/lib.rs` (near the other `mod`/`pub use` lines):

```rust
mod tile_transform;
pub use tile_transform::TileTransform;
```

Run: `cargo test -p ferrolite-mask tile_transform`
Expected: PASS (both tests). (Before creating the file, the crate would not compile — that is the "fails" state.)

- [ ] **Step 3: Add `uv_scale`/`uv_offset` to the linear-gradient uniform + shader**

Edit `ferrolite-mask/src/shapes/linear.rs` — replace the uniform + `run`:

```rust
/// Uniform for `linear_gradient.wgsl`. 32 bytes (multiple of 16).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LinearGradientUniform {
    pub start: [f32; 2],
    pub end: [f32; 2],
    pub uv_scale: [f32; 2],
    pub uv_offset: [f32; 2],
}

impl LinearGradientUniform {
    pub fn from_params(start: Vec2, end: Vec2, uv_scale: [f32; 2], uv_offset: [f32; 2]) -> Self {
        Self {
            start: [start.x, start.y],
            end: [end.x, end.y],
            uv_scale,
            uv_offset,
        }
    }
}
```

And the `run` method:

```rust
    pub fn run(
        &self,
        start: Vec2,
        end: Vec2,
        uv_scale: [f32; 2],
        uv_offset: [f32; 2],
        width: u32,
        height: u32,
    ) -> MaskBuffer {
        self.inner.run(
            LinearGradientUniform::from_params(start, end, uv_scale, uv_offset),
            width,
            height,
        )
    }
```

Update the existing in-file unit test `uniform_maps_params_verbatim` to pass identity uv:

```rust
    #[test]
    fn uniform_maps_params_verbatim() {
        let u = LinearGradientUniform::from_params(
            Vec2::new(0.1, 0.2),
            Vec2::new(0.3, 0.4),
            [1.0, 1.0],
            [0.0, 0.0],
        );
        assert_eq!(u.start, [0.1, 0.2]);
        assert_eq!(u.end, [0.3, 0.4]);
        assert_eq!(u.uv_scale, [1.0, 1.0]);
        assert_eq!(u.uv_offset, [0.0, 0.0]);
    }
```

Edit `ferrolite-mask/src/shaders/linear_gradient.wgsl` — struct + uv line:

```wgsl
struct P { start: vec2<f32>, end: vec2<f32>, uv_scale: vec2<f32>, uv_offset: vec2<f32> };
@group(0) @binding(1) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(out_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let uv_local = (vec2<f32>(f32(gid.x), f32(gid.y)) + vec2<f32>(0.5, 0.5))
        / vec2<f32>(f32(dims.x), f32(dims.y));
    let uv = uv_local * p.uv_scale + p.uv_offset;
    let axis = p.end - p.start;
    let len2 = dot(axis, axis);
    var t = 0.0;
    if (len2 > 1e-12) {
        t = clamp(dot(uv - p.start, axis) / len2, 0.0, 1.0);
    }
    textureStore(out_tex, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(t, 0.0, 0.0, 1.0));
}
```

- [ ] **Step 4: Add `uv_scale`/`uv_offset` to the radial-gradient uniform + shader**

Edit `ferrolite-mask/src/shapes/radial.rs` uniform:

```rust
/// Uniform for `radial_gradient.wgsl` — 48 bytes (multiple of 16).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RadialGradientUniform {
    pub center: [f32; 2],
    pub radius: [f32; 2],
    pub rotation: f32,
    pub feather: f32,
    pub invert: f32,
    pub _pad: f32,
    pub uv_scale: [f32; 2],
    pub uv_offset: [f32; 2],
}

impl RadialGradientUniform {
    #[allow(clippy::too_many_arguments)]
    pub fn from_params(
        center: Vec2,
        radius: Vec2,
        rotation: f32,
        feather: f32,
        invert: bool,
        uv_scale: [f32; 2],
        uv_offset: [f32; 2],
    ) -> Self {
        Self {
            center: [center.x, center.y],
            radius: [radius.x, radius.y],
            rotation,
            feather,
            invert: if invert { 1.0 } else { 0.0 },
            _pad: 0.0,
            uv_scale,
            uv_offset,
        }
    }
}
```

And `run`:

```rust
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        center: Vec2,
        radius: Vec2,
        rotation: f32,
        feather: f32,
        invert: bool,
        uv_scale: [f32; 2],
        uv_offset: [f32; 2],
        width: u32,
        height: u32,
    ) -> MaskBuffer {
        self.inner.run(
            RadialGradientUniform::from_params(
                center, radius, rotation, feather, invert, uv_scale, uv_offset,
            ),
            width,
            height,
        )
    }
```

Update the in-file test `invert_flag_maps_to_float` calls to pass `[1.0,1.0], [0.0,0.0]` for the two new args (insert after `invert` bool, before `width`... note `from_params` has no width/height):

```rust
        let a = RadialGradientUniform::from_params(
            Vec2::new(0.5, 0.5), Vec2::new(0.3, 0.2), 0.0, 0.1, false, [1.0, 1.0], [0.0, 0.0],
        );
        assert_eq!(a.invert, 0.0);
        let b = RadialGradientUniform::from_params(
            Vec2::new(0.5, 0.5), Vec2::new(0.3, 0.2), 0.0, 0.1, true, [1.0, 1.0], [0.0, 0.0],
        );
        assert_eq!(b.invert, 1.0);
```

Edit `ferrolite-mask/src/shaders/radial_gradient.wgsl` struct + uv line:

```wgsl
struct P {
    center: vec2<f32>,
    radius: vec2<f32>,
    rotation: f32,
    feather: f32,
    invert: f32,
    pad: f32,
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
};
@group(0) @binding(1) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(out_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let uv_local = (vec2<f32>(f32(gid.x), f32(gid.y)) + vec2<f32>(0.5, 0.5))
        / vec2<f32>(f32(dims.x), f32(dims.y));
    let uv = uv_local * p.uv_scale + p.uv_offset;
    let d0 = uv - p.center;
    let c = cos(p.rotation);
    let s = sin(p.rotation);
    let local = vec2<f32>(c * d0.x + s * d0.y, -s * d0.x + c * d0.y);
    let rx = max(p.radius.x, 1e-6);
    let ry = max(p.radius.y, 1e-6);
    let dist = length(vec2<f32>(local.x / rx, local.y / ry));
    let f = max(p.feather, 1e-6);
    var m = 1.0 - smoothstep(1.0, 1.0 + f, dist);
    if (p.invert > 0.5) { m = 1.0 - m; }
    textureStore(out_tex, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(m, 0.0, 0.0, 1.0));
}
```

- [ ] **Step 5: Thread `tile: TileTransform` through `MaskCompositor::eval`/`composite`/`composite_cached`**

In `ferrolite-mask/src/compositor.rs`, change `eval` to take `tile: TileTransform` and use it for spatial + brush:

```rust
    fn eval(
        &self,
        comp: &MaskComponent,
        input: &wgpu::TextureView,
        w: u32,
        h: u32,
        rasters: &RasterStore,
        tile: TileTransform,
    ) -> MaskBuffer {
        let (uv_scale, uv_offset) = tile.uv_scale_offset(w, h);
        match comp {
            MaskComponent::LinearGradient { start, end } => self.linear.run(
                Vec2::new(start.x, start.y),
                Vec2::new(end.x, end.y),
                uv_scale,
                uv_offset,
                w,
                h,
            ),
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
                uv_scale,
                uv_offset,
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
                let mut i = 0usize;
                while i < strokes.len() {
                    let erase = strokes[i].erase;
                    let mut dabs = Vec::new();
                    while i < strokes.len() && strokes[i].erase == erase {
                        dabs.extend(stroke_dabs(&strokes[i], SPACING_FRAC));
                        i += 1;
                    }
                    acc = self.brush.stamp_onto(
                        &acc,
                        &dabs,
                        erase,
                        (tile.origin[0], tile.origin[1]),
                        (tile.level_dims[0], tile.level_dims[1]),
                    );
                }
                acc
            }
            MaskComponent::Imported { handle, .. } => match rasters.get(*handle) {
                Some(buf) if (buf.width, buf.height) == (w, h) => buf.clone(),
                _ => MaskBuffer::alloc_zeroed(&self.ctx, w, h),
            },
        }
    }
```

Add the import at the top of the file: `use crate::TileTransform;`

Update `composite` signature + the one `eval` call:

```rust
    pub fn composite(
        &self,
        def: &MaskDefinition,
        input: &wgpu::TextureView,
        w: u32,
        h: u32,
        rasters: &RasterStore,
        tile: TileTransform,
    ) -> MaskBuffer {
        if def.components.is_empty() {
            return self.empty_coverage(def.invert, w, h);
        }
        let inputs: Vec<(MaskBuffer, CompositeMode)> = def
            .components
            .iter()
            .map(|(c, m)| (self.eval(c, input, w, h, rasters, tile), *m))
            .collect();
        self.composite.composite(&inputs, def.invert)
    }
```

Update `composite_cached` — add `tile: TileTransform` as the LAST parameter and pass it to the `eval` call inside (keep the `#[allow(clippy::too_many_arguments)]`):

```rust
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
        tile: TileTransform,
    ) -> MaskBuffer {
        // ... unchanged body except the eval call:
        //     let cov = self.eval(comp, input, w, h, rasters, tile);
    }
```

- [ ] **Step 6: Fix the in-file `compositor.rs` tests to pass `TileTransform::whole_image(w,h)`**

Every `comp.composite(&def, &iv, W, H, &store)` call in `#[cfg(test)] mod tests` gains a final arg `TileTransform::whole_image(W, H)` (matching that call's W/H), and every `comp.composite_cached(&def, &iv, id, W, H, &store, &mut cache)` gains a final arg `TileTransform::whole_image(W, H)`. Add `use crate::TileTransform;` to the test module (or rely on the `super::*` glob if `TileTransform` is re-exported at crate root — it is; `use super::*;` already imports crate items in scope, but add an explicit `use crate::TileTransform;` inside `mod tests` to be safe). Example for the 4x4 case:

```rust
        let full = comp.composite(
            &MaskDefinition { components: vec![], invert: false },
            &iv, 4, 4, &RasterStore::default(), TileTransform::whole_image(4, 4),
        );
```

- [ ] **Step 7: Write a failing per-tile composite unit test (spatial + range)**

Add to `ferrolite-mask/src/compositor.rs` `mod tests`. This is the core correctness proof: compositing at a tile placement equals the matching sub-window of the whole-image composite. Uses a helper to upload an Rgba16F content texture — reuse `constant_buffer` for range, and a small CPU-built gradient input for luma.

```rust
    /// Upload an w*h Rgba16Float content texture (row-major [r,g,b,a] f32).
    fn upload_rgba16f(ctx: &Arc<GpuContext>, w: u32, h: u32, px: &[f32]) -> MaskBuffer {
        // MaskBuffer is R32Float; for CONTENT we need an Rgba16Float texture the
        // range shaders textureLoad. Build it directly here (test-only).
        let tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test-content"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let halfs: Vec<u8> = px
            .iter()
            .flat_map(|v| half::f16::from_f32(*v).to_le_bytes())
            .collect();
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &halfs,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(w * 8), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        // Wrap in a MaskBuffer-like holder just for the texture view; we only need a view.
        MaskBuffer { texture: std::sync::Arc::new(tex), width: w, height: h }
    }

    #[test]
    fn luma_range_tile_matches_whole_image_subwindow() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let comp = MaskCompositor::new(ctx.clone());

        // 16x16 content: horizontal luma ramp so a luma-range band selects a
        // vertical stripe. r=g=b=x/16.
        let (w, h) = (16u32, 16u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for _y in 0..h {
            for x in 0..w {
                let v = x as f32 / w as f32;
                px.extend_from_slice(&[v, v, v, 1.0]);
            }
        }
        let content = upload_rgba16f(&ctx, w, h, &px);
        let cv = content.texture.create_view(&wgpu::TextureViewDescriptor::default());

        let def = MaskDefinition {
            components: vec![(
                MaskComponent::LumaRange { lo: 0.4, hi: 0.7, softness: 0.05 },
                CompositeMode::Add,
            )],
            invert: false,
        };

        // Whole-image mask.
        let whole = comp.composite(&def, &cv, w, h, &RasterStore::default(),
            TileTransform::whole_image(w, h));
        let whole_px = read_mask_r32f(&ctx, &whole);

        // A 8x8 tile covering the RIGHT half [8..16) x [0..8). In the tiled
        // pipeline the node input for this tile would be exactly those pixels,
        // so build that sub-window content and composite at tile placement.
        let (tw, th) = (8u32, 8u32);
        let (ox, oy) = (8i32, 0i32);
        let mut tpx = Vec::with_capacity((tw * th * 4) as usize);
        for y in 0..th {
            for x in 0..tw {
                let gx = ox as u32 + x;
                let i = ((oy as u32 + y) * w + gx) as usize * 4;
                tpx.extend_from_slice(&px[i..i + 4]);
            }
        }
        let tcontent = upload_rgba16f(&ctx, tw, th, &tpx);
        let tcv = tcontent.texture.create_view(&wgpu::TextureViewDescriptor::default());
        // level_dims = whole image; origin = tile origin in level space.
        let tile_t = TileTransform { origin: [ox, oy], level_dims: [w, h] };
        let tmask = comp.composite(&def, &tcv, tw, th, &RasterStore::default(), tile_t);
        let tmask_px = read_mask_r32f(&ctx, &tmask);

        // Each tile pixel must equal the whole-image mask at (ox+x, oy+y).
        let mut max_d = 0.0f32;
        for y in 0..th {
            for x in 0..tw {
                let ti = (y * tw + x) as usize;
                let wi = ((oy as u32 + y) * w + (ox as u32 + x)) as usize;
                max_d = max_d.max((tmask_px[ti] - whole_px[wi]).abs());
            }
        }
        assert!(max_d < 1e-3, "luma-range tile vs whole-image drift {max_d}");
    }

    #[test]
    fn linear_gradient_tile_matches_whole_image_subwindow() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let comp = MaskCompositor::new(ctx.clone());
        let (w, h) = (16u32, 16u32);
        // Content is irrelevant for a gradient; a 1x1 dummy view still needs
        // correct dims for range, but gradients ignore input — use a wxh zero.
        let content = MaskBuffer::alloc_zeroed(&ctx, w, h);
        let cv = content.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let def = MaskDefinition {
            components: vec![(
                MaskComponent::LinearGradient {
                    start: crate::vec::Vec2::new(0.0, 0.5),
                    end: crate::vec::Vec2::new(1.0, 0.5),
                },
                CompositeMode::Add,
            )],
            invert: false,
        };
        let whole = comp.composite(&def, &cv, w, h, &RasterStore::default(),
            TileTransform::whole_image(w, h));
        let whole_px = read_mask_r32f(&ctx, &whole);

        let (tw, th) = (8u32, 8u32);
        let (ox, oy) = (8i32, 0i32);
        // For a gradient the tile content is unused; a tw x th zero view is fine.
        let tcontent = MaskBuffer::alloc_zeroed(&ctx, tw, th);
        let tcv = tcontent.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let tile_t = TileTransform { origin: [ox, oy], level_dims: [w, h] };
        let tmask = comp.composite(&def, &tcv, tw, th, &RasterStore::default(), tile_t);
        let tmask_px = read_mask_r32f(&ctx, &tmask);
        let mut max_d = 0.0f32;
        for y in 0..th {
            for x in 0..tw {
                let ti = (y * tw + x) as usize;
                let wi = ((oy as u32 + y) * w + (ox as u32 + x)) as usize;
                max_d = max_d.max((tmask_px[ti] - whole_px[wi]).abs());
            }
        }
        assert!(max_d < 2e-3, "linear-gradient tile vs whole-image drift {max_d}");
    }
```

Run: `cargo test -p ferrolite-mask luma_range_tile_matches_whole_image_subwindow linear_gradient_tile_matches_whole_image_subwindow`
Expected on a GPU machine: **before** Steps 3–5 these would fail to compile / mismatch; **after** Steps 3–5 they PASS. (Steps 3–5 are already applied by the time this test is added, so it should pass — this test's value is locking the per-tile==sub-window contract in place.)

- [ ] **Step 8: Build + run the whole `ferrolite-mask` test suite**

Run: `cargo test -p ferrolite-mask`
Expected: PASS (all existing goldens unchanged in intent; identity transform preserves behavior).

Run: `cargo clippy -p ferrolite-mask --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add ferrolite-mask/src/tile_transform.rs ferrolite-mask/src/lib.rs \
  ferrolite-mask/src/shapes/linear.rs ferrolite-mask/src/shaders/linear_gradient.wgsl \
  ferrolite-mask/src/shapes/radial.rs ferrolite-mask/src/shaders/radial_gradient.wgsl \
  ferrolite-mask/src/compositor.rs
git commit -m "feat(mask): per-tile mask compositing via TileTransform (uv-mapped shapes)"
```

---

### Task 2: `LocalAdjustmentsNode` composites per-tile; drop `mask_origin`/`mask_lod`

**Files:**
- Modify: `ferrolite-pipeline/src/local_node.rs`
- Modify: `ferrolite-pipeline/src/uniforms.rs` (`LocalAdjustUniform` + `local_adjust_uniform`, and any test referencing the removed tail)
- Modify: `ferrolite-pipeline/src/shaders/local_adjust.wgsl`
- Modify: `ferrolite-pipeline/src/mask_overlay.rs` (the one `composite_cached` call)

**Interfaces:**
- Consumes: `ferrolite_mask::TileTransform`, `MaskCompositor::composite(def, input, w, h, rasters, tile)`, `MaskCompositor::composite_cached(def, input, id, w, h, rasters, cache, tile)`.
- Produces: `LocalAdjustmentsNode::set_tile_transform(&self, tile: Option<TileTransform>)` (replaces `set_full_dims`/`set_mask_origin`/`set_mask_lod`). `None` = whole-image (cached, identity); `Some(t)` = tiled (composite fresh at input dims with placement `t`).

- [ ] **Step 1: Remove the `mask_origin`/`mask_lod`/`_pad` tail from `LocalAdjustUniform`**

Edit `ferrolite-pipeline/src/uniforms.rs`. Change the struct doc + drop the three tail fields (the struct then ends at `contrast_pivot`, size 64 bytes, still `% 16 == 0`):

```rust
/// GPU uniform for `local_adjust.wgsl`. `#[repr(C)]`, 16-byte aligned. Field order +
/// padding MIRROR the WGSL `struct P` exactly. The mask is composited at the SAME
/// resolution as this pass's input (whole image for preview, one tile for the tiled
/// tier), so the apply pass samples it 1:1 — no per-tile origin/LOD offset.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LocalAdjustUniform {
    pub exposure_gain: f32,
    pub contrast_gain: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32,
    pub hue_deg: f32,
    pub wb_mul: [f32; 3],
    pub color_amount: f32,
    pub color_rgb: [f32; 3],
    pub contrast_pivot: f32,
}
```

And in `local_adjust_uniform`, delete the `mask_origin: [0, 0]`, `mask_lod: 0`, `_pad: 0` lines (the constructor ends at `contrast_pivot: CONTRAST_PIVOT,`).

The existing test `local_adjust_uniform_is_identity_when_default` keeps its `size_of::<LocalAdjustUniform>() % 16 == 0` assertion — still true (64 bytes). No test references `mask_origin`/`mask_lod`, so nothing else changes here.

- [ ] **Step 2: Sample the mask 1:1 in `local_adjust.wgsl`**

Edit `ferrolite-pipeline/src/shaders/local_adjust.wgsl`. Update the header comment, the `struct P` tail, and the mask lookup:

```wgsl
// Local Light+Color point op, blended by a mask. Mirrors uniforms::light_color_apply
// exactly. `dst[xy] = mix(src[xy], adjusted(src[xy]), mask[xy])`, so a mask value of 0
// leaves the pixel untouched and 1 applies the full adjustment. The mask is composited
// at the SAME resolution as `src` (whole image for preview, one tile for the tiled
// tier), so it is sampled 1:1 with no origin/LOD offset.
```

Change `struct P`'s tail from `mask_origin: vec2<i32>, mask_lod: i32, pad: i32,` to nothing — it ends at `color_rgb: vec3<f32>, contrast_pivot: f32,`:

```wgsl
struct P {
    exposure_gain: f32, contrast_gain: f32, highlights: f32, shadows: f32,
    whites: f32, blacks: f32, saturation: f32, hue_deg: f32,
    wb_mul: vec3<f32>, color_amount: f32,
    color_rgb: vec3<f32>, contrast_pivot: f32,
};
```

And the `main` mask lookup (replace the `mdims`/`scale`/`mcoord` block):

```wgsl
@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    let m = textureLoad(mask, xy, 0).r;
    let out = mix(c.rgb, adjust(c.rgb), clamp(m, 0.0, 1.0));
    textureStore(dst, xy, vec4<f32>(out, c.a));
}
```

- [ ] **Step 3: Rework `LocalAdjustmentsNode` state + `evaluate` to composite per-tile**

Edit `ferrolite-pipeline/src/local_node.rs`.

Imports: add `use ferrolite_mask::TileTransform;` to the existing `ferrolite_mask` use (it currently imports `{MaskBuffer, MaskCompositor, RasterStore}`).

Replace the three tile-tier fields (`full_dims`, `mask_origin`, `mask_lod`) with one:

```rust
    // tile-tier placement: None = whole-image (cached, identity); Some = tiled
    // (composite fresh at input dims with this placement so range components
    // sample the tile's own content and spatial components map to full-image uv).
    tile: RefCell<Option<TileTransform>>,
```

In `new`, replace the three initializers with `tile: RefCell::new(None),` (drop `full_dims`, `mask_origin`, `mask_lod`).

Replace `set_full_dims`/`set_mask_origin`/`set_mask_lod` with:

```rust
    /// Set the tile-tier placement. `None` = whole-image (identity, cached);
    /// `Some(t)` = tiled: the mask is composited fresh each evaluate at the
    /// input (tile) resolution with placement `t`, so content-dependent
    /// components (Color/Luminance range) sample this tile's own edited pixels
    /// and spatial components map to full-image uv. Clears the cache on change.
    pub(crate) fn set_tile_transform(&self, tile: Option<TileTransform>) {
        let mut cur = self.tile.borrow_mut();
        if *cur != tile {
            *cur = tile;
            self.cache.borrow_mut().take();
        }
    }
```

Update `CachedMasks` to key on the composite dims (was `full_dims`), keeping the field name meaning "the dims the cache was built at":

```rust
struct CachedMasks {
    mask_defs: Vec<ferrolite_mask::MaskDefinition>,
    dims: (u32, u32),
    masks: Vec<MaskBuffer>,
}
```

Rewrite `evaluate`:

```rust
impl Node<PipelineImage> for LocalAdjustmentsNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let input = inputs[0];
        let layers = self.layers.borrow();
        if layers.is_identity() {
            return input.clone();
        }
        // Composite the mask at the INPUT resolution: whole image for preview,
        // one (haloed) tile for the tiled tier. Range components read this exact
        // content; the tile placement maps spatial components to full-image uv.
        let (mw, mh) = (input.width, input.height);
        let tile = self.tile.borrow();
        let placement = tile.unwrap_or_else(|| TileTransform::whole_image(mw, mh));
        let input_view = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let cur_defs: Vec<ferrolite_mask::MaskDefinition> =
            layers.visible_layers().map(|l| l.mask.clone()).collect();

        // The whole-image path caches masks (keyed on defs+dims) so an
        // adjustment-only change reuses them. The tiled path composites fresh
        // every evaluate: each produced tile has different content/placement,
        // and content-dependent components would otherwise go stale across
        // upstream edits. Tile masks are tile-sized, so this is cheap + bounded.
        let use_cache = tile.is_none();
        let composite_all = || -> Vec<MaskBuffer> {
            self.rebuilds.set(self.rebuilds.get() + 1);
            layers
                .visible_layers()
                .map(|l| {
                    self.compositor.composite(
                        &l.mask,
                        &input_view,
                        mw,
                        mh,
                        &RasterStore::default(),
                        placement,
                    )
                })
                .collect()
        };

        let masks: Vec<MaskBuffer> = if use_cache {
            let hit = {
                let c = self.cache.borrow();
                matches!(&*c, Some(cm) if cm.mask_defs == cur_defs && cm.dims == (mw, mh))
            };
            if !hit {
                let masks = composite_all();
                *self.cache.borrow_mut() = Some(CachedMasks {
                    mask_defs: cur_defs.clone(),
                    dims: (mw, mh),
                    masks,
                });
            }
            self.cache.borrow().as_ref().unwrap().masks.clone()
        } else {
            composite_all()
        };

        let mut current = input.clone();
        for (layer, mask) in layers.visible_layers().zip(masks.iter()) {
            let u = local_adjust_uniform(&layer.adjustments);
            current = self.apply(&current, mask, u);
        }
        current
    }
}
```

Note: `MaskBuffer` is `Clone` (used elsewhere via `.clone()`), so `masks.clone()` for the cache-hit branch is fine (clones the `Arc<Texture>` handles). Remove the now-unused `local_adjust_uniform` mutation of `u.mask_origin`/`u.mask_lod`.

- [ ] **Step 4: Update `mask_overlay.rs` to pass the identity transform**

Edit `ferrolite-pipeline/src/mask_overlay.rs` `overlay_texture`, the `composite_cached` call — add `ferrolite_mask::TileTransform::whole_image(w, h)` as the final arg:

```rust
        let coverage = self.compositor.composite_cached(
            def,
            &iv,
            input_id,
            w,
            h,
            &RasterStore::default(),
            &mut self.cache,
            ferrolite_mask::TileTransform::whole_image(w, h),
        );
```

- [ ] **Step 5: Update the `local_node.rs` unit tests to the new API**

In `ferrolite-pipeline/src/local_node.rs` `mod tests`, the test `adjustment_only_change_does_not_recomposite_masks` uses the default node (no tile transform → `tile == None` → cache active), so its `rebuild_count` assertions (1, then 1 after adjustment-only, then 2 after mask change) stay valid **unchanged**. Confirm no test calls `set_full_dims`/`set_mask_origin`/`set_mask_lod` (they don't). No test edits needed beyond compilation — but build to confirm.

- [ ] **Step 6: Build the crate (it will not fully link until Task 3 fixes `tile_edit.rs`)**

`tile_edit.rs` still calls the removed setters, so the crate won't compile yet. Run just this crate's unit tests for `local_node`/`uniforms` is not possible until Task 3. Therefore: run `cargo build -p ferrolite-mask` (Task 1 crate) to confirm Task 1 still builds, and defer `ferrolite-pipeline` compilation to Task 3. Do **not** commit a non-compiling crate alone — **fold the commit into Task 3** (this task and Task 3 share one commit). Mark Task 2 complete only after Task 3's build is green.

> Reviewer note: Tasks 2 and 3 are one compilation unit (the API change and its only remaining caller). They are split for review clarity but committed together at the end of Task 3.

---

### Task 3: `TileEditPipeline` sets the per-tile transform

**Files:**
- Modify: `ferrolite-pipeline/src/tile_edit.rs`

**Interfaces:**
- Consumes: `LocalAdjustmentsNode::set_tile_transform(Option<TileTransform>)`, `ferrolite_image::{level_size, haloed_tile_origin}`, `ferrolite_mask::TileTransform`, `crate::edited_output_dims`.

- [ ] **Step 1: Store output dims; drop the `set_full_dims` call**

Edit `ferrolite-pipeline/src/tile_edit.rs`.

Add fields to `TileEditPipeline` (near `halo`):

```rust
    out_w: u32,
    out_h: u32,
```

In `new`, the `(out_w, out_h)` is already computed:

```rust
        let (out_w, out_h) = crate::edited_output_dims(&stack, src_w, src_h);
```

Delete the line `local_node.set_full_dims((out_w, out_h));`. Add `out_w, out_h,` to the struct literal that builds `Self { .. }`.

Add imports: extend the `ferrolite_image` use to include `level_size` and `haloed_tile_origin`:

```rust
use ferrolite_image::{haloed_tile_origin, level_size, TileCoord, TILE_SIZE};
```

Add `use ferrolite_mask::TileTransform;`.

- [ ] **Step 2: Set the per-tile transform in `produce_tile`**

Replace the `mask_origin`/`mask_lod` block in `produce_tile` (the `let gx = ...; let gy = ...; self.local_node.set_mask_origin([gx, gy]); self.local_node.set_mask_lod(coord.lod);` lines) with:

```rust
        // Composite the mask at THIS tile's resolution (the haloed color-chain
        // buffer), so content-dependent components (Color/Luminance range) read
        // the tile's own edited pixels. `origin` is the haloed tile origin in
        // the tile's LOD level (output) pixel space; `level_dims` is that level's
        // full size — together they map spatial components to full-image uv. The
        // apply pass then samples the mask 1:1 (no origin/LOD offset).
        let (lw, lh) = level_size(self.out_w, self.out_h, coord.lod);
        let (ox, oy) = haloed_tile_origin(coord, self.halo);
        self.local_node.set_tile_transform(Some(TileTransform {
            origin: [ox as i32, oy as i32],
            level_dims: [lw, lh],
        }));
```

Keep the two `mark_dirty` calls and the `evaluate`/`extract_interior` tail unchanged.

- [ ] **Step 3: Update the module + `set_stack` doc comments**

In `tile_edit.rs`, update the file-level doc block that describes "**LocalAdjustments — output-space mask, pragmatic limitation**" and the `set_stack` LIMITATION paragraph mentioning `set_full_dims`. Replace the stale "composited ONCE per document at the full output resolution … each `produce_tile` only updates the per-tile `mask_origin`" wording with a description of per-tile compositing. Concretely, replace the file-level paragraph (lines describing the output-space mask) with:

```rust
//! **LocalAdjustments — per-tile mask, output space:** because geometry runs at
//! the head, the entire color chain (including `LocalAdjustments`) operates in
//! **output space**. Each `produce_tile` composites the layer masks at that
//! tile's own (haloed) resolution against the tile's edited content, placed via
//! `set_tile_transform` (haloed origin + LOD level dims). Content-dependent
//! components (Color/Luminance range) therefore sample the correct full-res
//! pixels; spatial components (gradient/radial/brush) are mapped to full-image
//! uv by the placement. For identity/translation geometry this matches the
//! whole-image preview render within float tolerance. Under crop/rotate the mask
//! anchors to the cropped/rotated **output** frame — the same accepted
//! difference already noted for Sharpen. Per-tile masks are tile-sized (bounded,
//! no full-frame mask buffer).
```

And in `set_stack`, replace the final sentence about `set_full_dims` with:

```rust
    /// change requires the same full rebuild, not just a `set_stack` call. The
    /// per-tile mask placement is set per `produce_tile` via `set_tile_transform`.
```

- [ ] **Step 4: Build + run the pipeline unit tests**

Run: `cargo build -p ferrolite-pipeline`
Expected: compiles (Task 2 + Task 3 API now consistent).

Run: `cargo test -p ferrolite-pipeline --lib`
Expected: PASS (unit tests, incl. `local_node` cache tests and `uniforms` size test).

- [ ] **Step 5: Commit Tasks 2 + 3 together**

```bash
git add ferrolite-pipeline/src/local_node.rs ferrolite-pipeline/src/uniforms.rs \
  ferrolite-pipeline/src/shaders/local_adjust.wgsl ferrolite-pipeline/src/mask_overlay.rs \
  ferrolite-pipeline/src/tile_edit.rs
git commit -m "feat(pipeline): composite local-adjust masks per tile; drop mask_origin/lod"
```

---

### Task 4: Integration goldens — tiled range masks match the whole-image preview

**Files:**
- Modify: `ferrolite-pipeline/tests/local_golden.rs`

**Interfaces:**
- Consumes: `common::{gradient, read_image_linear, read_tile_linear}`, `EditPipeline`, `TileEditPipeline`, `GpuPyramidSource`, `TileCoord`, `MaskComponent::{LumaRange, ColorRange}`.

- [ ] **Step 1: Port the spatial lod-1 golden to the new API (if it references removed setters)**

Open `ferrolite-pipeline/tests/local_golden.rs`. The test `tile_lod1_masked_adjustment_samples_correct_mask_half` (and any comment at ~line 209 mentioning `set_mask_origin`) references the old machinery only via `TileEditPipeline::produce_tile` (which now sets the transform internally) — so it needs **no API change**, only a comment refresh. Update the doc comment above `tile_masked_adjustment_matches_preview_region_identity_geometry` (line ~207–212) to drop "composites the mask once at full output resolution … via `set_mask_origin`" and say "composites the mask per tile (see `TileEditPipeline::produce_tile` → `set_tile_transform`)". Do NOT change assertions. These tests still exercise a spatial (LinearGradient) mask and must keep passing.

- [ ] **Step 2: Write the failing range-mask golden (luma)**

Add to `ferrolite-pipeline/tests/local_golden.rs`. Build a source whose luma varies across a full tile boundary so the range band selects a region spanning >1 tile, then assert tile (1,0) matches the whole-image reference. `common::gradient(w,h)` produces an x/y ramp (per the existing tests' usage), which gives varying luma — reuse it.

```rust
/// Regression for the tiled Color/Luminance range-mask bug: content-dependent
/// mask components must be composited from each tile's OWN edited content, not
/// from a single tile smeared across a full-output mask. A luma-range masked
/// exposure boost on a source larger than one tile must render the SAME in a
/// non-(0,0) tile as in the whole-image preview.
#[test]
fn tile_luma_range_masked_adjustment_matches_preview_region() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // Two tiles wide so tile (1,0) is a genuine non-origin sub-region.
    let sw = TILE_SIZE * 2 + 40;
    let sh = TILE_SIZE + 24;
    let src = common::gradient(sw, sh);
    let la = LocalAdjustments {
        layers: vec![MaskLayer {
            name: "luma".into(),
            visible: true,
            mask: MaskDefinition {
                components: vec![(
                    MaskComponent::LumaRange { lo: 0.3, hi: 0.8, softness: 0.1 },
                    CompositeMode::Add,
                )],
                invert: false,
            },
            adjustments: AdjustmentSet {
                exposure: 0.8,
                ..Default::default()
            },
        }],
    };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));

    let mut preview = EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    let whole = common::read_image_linear(&ctx, &preview.evaluate());

    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    let mut tiles = TileEditPipeline::new(ctx.clone(), pyramid, stack, IDENTITY, None, None);
    // Tile (1,0): covers output columns [TILE_SIZE, 2*TILE_SIZE).
    let tex = tiles.produce_tile(TileCoord { lod: 0, x: 1, y: 0 });
    let tile = common::read_tile_linear(&ctx, &tex);

    let base_x = TILE_SIZE; // tile (1,0) origin in output space
    let mut max_d = 0.0f32;
    for ty in 0..TILE_SIZE.min(sh) {
        for tx in 0..TILE_SIZE {
            let gx = base_x + tx;
            if gx >= sw { continue; }
            for ch in 0..3 {
                let ti = ((ty * TILE_SIZE + tx) * 4 + ch) as usize;
                let wi = ((ty * sw + gx) * 4 + ch) as usize;
                max_d = max_d.max((tile[ti] - whole[wi]).abs());
            }
        }
    }
    assert!(max_d < 0.02, "luma-range tile (1,0) vs preview drift {max_d}");
}
```

- [ ] **Step 3: Run the luma golden to verify it FAILS on the pre-fix code path, PASSES now**

Run: `cargo test -p ferrolite-pipeline --test local_golden tile_luma_range_masked_adjustment_matches_preview_region`
Expected: PASS on the fixed code. (To confirm it is a real regression guard, temporarily `git stash` the Task-1/2/3 changes → it FAILS with a large drift; restore → PASSES. This is a verification aid, not a required commit step.)

- [ ] **Step 4: Add the color-range golden**

Add a second test that samples a color present in tile (1,0). `common::gradient` sets each pixel to a known function of (x,y); sample the color at the tile-(1,0) center from the CPU `src` and use it as the `ColorRange` sample so the mask is non-trivial there.

```rust
/// Same as the luma golden but for a Color-range component (the other
/// content-dependent mask). Uses a sample color taken from within tile (1,0).
#[test]
fn tile_color_range_masked_adjustment_matches_preview_region() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let sw = TILE_SIZE * 2 + 40;
    let sh = TILE_SIZE + 24;
    let src = common::gradient(sw, sh);
    // Sample the source color near the center of tile (1,0).
    let cx = TILE_SIZE + TILE_SIZE / 2;
    let cy = sh / 2;
    let ci = ((cy * sw + cx) * 4) as usize;
    let sample = ferrolite_mask::Rgb::new(src.data()[ci], src.data()[ci + 1], src.data()[ci + 2]);
    let la = LocalAdjustments {
        layers: vec![MaskLayer {
            name: "color".into(),
            visible: true,
            mask: MaskDefinition {
                components: vec![(
                    MaskComponent::ColorRange {
                        samples: vec![sample],
                        tolerance: 0.15,
                        softness: 0.1,
                    },
                    CompositeMode::Add,
                )],
                invert: false,
            },
            adjustments: AdjustmentSet { exposure: 0.8, ..Default::default() },
        }],
    };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));

    let mut preview = EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    let whole = common::read_image_linear(&ctx, &preview.evaluate());
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    let mut tiles = TileEditPipeline::new(ctx.clone(), pyramid, stack, IDENTITY, None, None);
    let tex = tiles.produce_tile(TileCoord { lod: 0, x: 1, y: 0 });
    let tile = common::read_tile_linear(&ctx, &tex);

    let base_x = TILE_SIZE;
    let mut max_d = 0.0f32;
    for ty in 0..TILE_SIZE.min(sh) {
        for tx in 0..TILE_SIZE {
            let gx = base_x + tx;
            if gx >= sw { continue; }
            for ch in 0..3 {
                let ti = ((ty * TILE_SIZE + tx) * 4 + ch) as usize;
                let wi = ((ty * sw + gx) * 4 + ch) as usize;
                max_d = max_d.max((tile[ti] - whole[wi]).abs());
            }
        }
    }
    assert!(max_d < 0.02, "color-range tile (1,0) vs preview drift {max_d}");
}
```

> Note on the `src.data()` accessor and `ferrolite_mask::Rgb`: confirm the exact accessor name on `LinearRgbaF32` (the CPU source type from `common::gradient`) — it may be `.data()`, `.pixels()`, or a field. Use whatever the existing `common` helpers use to read pixels. Confirm `ferrolite_mask::Rgb` is the sample color type used by `MaskComponent::ColorRange { samples: Vec<Rgb> }` (it is — `ColorRange.samples` is `Vec<ferrolite_mask::Rgb>` per `compositor.rs::eval`). Adjust the import/path to match (`use ferrolite_mask::Rgb;` at the top, or fully-qualify).

- [ ] **Step 5: Run the full golden suite**

Run: `cargo test -p ferrolite-pipeline --test local_golden`
Expected: PASS (spatial goldens unchanged; both new range goldens pass).

- [ ] **Step 6: Commit**

```bash
git add ferrolite-pipeline/tests/local_golden.rs
git commit -m "test(pipeline): golden — tiled luma/color-range masks match preview"
```

---

### Task 5: Workspace green gate

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then: `cargo fmt --all --check`
Expected: clean (no diff).

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean. (Watch for: unused imports from the removed setters, `too_many_arguments` on `composite_cached`/`radial::run` — already `#[allow]`ed; add `#[allow(clippy::too_many_arguments)]` to `MaskCompositor::composite_cached` if clippy flags the extra param.)

- [ ] **Step 3: Full test suite**

Run: `cargo test --workspace`
Expected: PASS (GPU goldens run on the dev machine; headless CI skips them).

- [ ] **Step 4: Commit any fmt-only changes**

```bash
git add -A
git commit -m "chore: fmt" # only if fmt produced changes; otherwise skip
```

- [ ] **Step 5: STOP — hand the author the visual test plan and hold**

Do NOT merge/push. Present the numbered visual test plan (below) and wait for the author's hands-on results.

**Visual test plan (author, hands-on):**
1. Open a RAW that decodes to RGGB (full-res RCD path), enter Develop, add a local-adjustment mask **layer** with a strong, visible adjustment (e.g. exposure +1.0).
2. **Luminance range:** add a **Luminance range** component to the mask; set the band to select, say, the midtones. Verify the adjustment lands on the correct tones. Now **stop interacting** (don't pan/zoom) and wait for the idle full-res composite. **Expected:** the masked adjustment stays exactly where it was while interacting — no reversion to the base image, no wrong-region selection. **Failure signature:** at idle the effect disappears, shifts, or selects a different region than during interaction / than the before-after split shows.
3. **Color range:** replace with a **Color range** component; pick a color sample from the image. Same idle check. **Expected:** idle matches interacting. **Failure signature:** idle differs from the interacting/split view.
4. **Pan/zoom stress:** with a range mask active, pan and zoom around; at each rest the full-res tiles must agree with the moving preview. Check the image **edges** and **tile boundaries** — no seams in the masked effect between adjacent tiles.
5. **Regression — spatial masks still work:** confirm brush and linear/radial gradient masks still render correctly at idle (they were fixed previously; must not regress).
6. **Before/after split:** toggle the split — both halves must be self-consistent with the idle full-res view.
7. **Per-control reset:** the range component's controls each reset individually (unchanged behavior; quick sanity check).

(The right-edge ~20px stretched seam is a **separate** issue tracked next — crop-to-active-area at decode — and is expected to still be present after this branch.)

---

## Self-Review

**1. Spec coverage (the reported bug):** Root cause = range shaders sample per-tile `src` while the mask is composited at full-output dims. Fixed by Task 1 (per-tile compositor + uv-mapped spatial shapes), Task 2 (node composites at input dims, samples mask 1:1), Task 3 (tile pipeline sets placement). Task 4 proves it via luma+color goldens. Task 5 gates + hands off. ✓

**2. Placeholder scan:** No TBD/TODO. Every code step shows full code. The one soft spot — the `src.data()`/`Rgb` accessor names in Task 4 Step 4 — is flagged with an explicit "confirm the accessor" note and the fallback (match `common` helpers). ✓

**3. Type consistency:**
- `TileTransform { origin: [i32;2], level_dims: [u32;2] }`, `whole_image(w,h)`, `uv_scale_offset(w,h) -> ([f32;2],[f32;2])` — used identically in Tasks 1–3. ✓
- `composite(.., tile: TileTransform)` and `composite_cached(.., tile: TileTransform)` (tile is the LAST param) — call sites in local_node (`composite`), mask_overlay (`composite_cached`), and compositor tests all pass `TileTransform` last. ✓
- `set_tile_transform(Option<TileTransform>)` — defined in Task 2, called in Task 3. `None` ⇒ whole-image/cached; `Some` ⇒ tiled/fresh. ✓
- `LinearGradientPass::run` / `RadialGradientPass::run` gain `uv_scale,uv_offset` before `width,height`; `eval` computes them from `tile.uv_scale_offset(w,h)`. ✓
- Removed `mask_origin`/`mask_lod` consistently across uniforms.rs, local_adjust.wgsl, local_node.rs, tile_edit.rs. ✓
- `CachedMasks.full_dims` → `CachedMasks.dims`; only referenced inside `local_node.rs`. ✓
