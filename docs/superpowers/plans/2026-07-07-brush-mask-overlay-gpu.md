# Brush-mask overlay GPU-tint Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the Develop brush-mask overlay's per-frame GPU→CPU readback (the measured lag) by tinting the composited mask red on the GPU and handing egui the wgpu texture directly.

**Architecture:** `MaskOverlayCompositor` gains a build-once fullscreen-triangle **tint render pass** that reads the R32F coverage and renders premultiplied red into a small `Rgba8Unorm` texture (with an `Rgba8UnormSrgb` view for egui). The app registers that texture once as an egui *native* texture and updates it in place on each mask change — no `read_mask_r32f`, no CPU RGBA rebuild, no per-frame `ColorImage` upload. `mask_overlay::show` draws it with the same `ui.painter().image(..)` call as before.

**Tech Stack:** Rust, wgpu (via `ferrolite-gpu`), egui/egui-wgpu 0.29, `ferrolite-mask`, `ferrolite-pipeline`, `ferrolite-app`.

## Global Constraints

- **Never block the UI/update thread** (CLAUDE.md §1). The composite + tint are async GPU submits with **no** `poll(Wait)`. No readback on the paint path.
- **Build GPU pipelines once, reuse** (CLAUDE.md §2). The tint render pipeline is built in `MaskOverlayCompositor::new` and never rebuilt per frame/edit.
- **Visual behavior is preserved exactly:** the red overlay looks and aligns identically to today; the `overlay_on && !adjusting` gate, selection gates, live prospective-component preview, and tool affordances are unchanged.
- **Gate before finishing:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green, **then HOLD for the author's hands-on visual test** (CLAUDE.md "Finishing a branch").
- **Tint strength = 0.5** (matches today's 50% red tint). Overlay input stays bounded to `OVERLAY_MAX_EDGE = 512`.
- Branch: `fix/brush-mask-perf` (already created, off `main`). The temporary `FERROLITE_BRUSH_PROFILE` instrumentation from earlier commits is removed in Task 3.

---

## Task 1: GPU tint render pass in `ferrolite-pipeline`

Add the build-once tint pipeline and `overlay_texture()` to `MaskOverlayCompositor`, plus the pure tint mapping and a golden GPU test. `coverage()` is **kept** this task (the app still calls it until Task 2) so the workspace stays green.

**Files:**
- Create: `ferrolite-pipeline/src/shaders/mask_overlay_tint.wgsl`
- Modify: `ferrolite-pipeline/src/mask_overlay.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (re-export `OverlayTexture`, `overlay_tint`)
- Test: inline `#[cfg(test)]` in `ferrolite-pipeline/src/mask_overlay.rs`

**Interfaces:**
- Consumes: `ferrolite_mask::MaskCompositor` (`composite(def, &view, w, h, &RasterStore) -> MaskBuffer`), `ferrolite_mask::MaskBuffer` (`.texture: Arc<wgpu::Texture>`, `.width`, `.height`), `ferrolite_gpu::GpuContext` (`.device`, `.queue`, `.shader_module(label, wgsl) -> Arc<ShaderModule>`), `crate::image::PipelineImage` (`.texture: Arc<wgpu::Texture>`, `.width`, `.height`).
- Produces (relied on by Task 2):
  - `pub struct OverlayTexture { pub texture: std::sync::Arc<wgpu::Texture>, pub width: u32, pub height: u32 }` with `pub fn srgb_view(&self) -> wgpu::TextureView` (an `Rgba8UnormSrgb` view for egui).
  - `MaskOverlayCompositor::overlay_texture(&self, def: &MaskDefinition, input: &PipelineImage, strength: f32) -> OverlayTexture`.
  - `pub fn overlay_tint(coverage: f32, strength: f32) -> [f32; 4]` (premultiplied linear red; mirrors the WGSL).

- [ ] **Step 1: Write the failing pure-mapping test**

Add to the `tests` module in `ferrolite-pipeline/src/mask_overlay.rs`:

```rust
#[test]
fn overlay_tint_is_premultiplied_red_and_clamped() {
    assert_eq!(overlay_tint(0.0, 0.5), [0.0, 0.0, 0.0, 0.0], "zero coverage -> transparent");
    assert_eq!(overlay_tint(1.0, 0.5), [0.5, 0.0, 0.0, 0.5], "full coverage -> premul red at strength");
    // premultiplied: rgb.r always equals alpha
    let t = overlay_tint(0.4, 0.5);
    assert_eq!(t[0], t[3], "red channel is premultiplied by alpha");
    assert_eq!([t[1], t[2]], [0.0, 0.0], "green/blue are zero");
    // clamps coverage and strength into [0,1]
    assert_eq!(overlay_tint(-0.2, 0.5), [0.0, 0.0, 0.0, 0.0], "negative coverage clamps to 0");
    assert_eq!(overlay_tint(1.5, 2.0), [1.0, 0.0, 0.0, 1.0], "over-range clamps to 1");
}
```

- [ ] **Step 2: Run it, verify it fails to compile**

Run: `cargo test -p ferrolite-pipeline overlay_tint_is_premultiplied -- --nocapture`
Expected: FAIL — `cannot find function overlay_tint`.

- [ ] **Step 3: Add the pure `overlay_tint` function**

Add to `ferrolite-pipeline/src/mask_overlay.rs` (module level, above `MaskOverlayCompositor`):

```rust
/// Premultiplied **linear** red overlay tint for a coverage value, mirroring the
/// `mask_overlay_tint.wgsl` fragment shader exactly. Returns `[r, g, b, a]` with
/// `a = clamp(coverage) * clamp(strength)` and `r = a` (premultiplied red),
/// `g = b = 0`. The GPU pass stores these into a linear `Rgba8Unorm` target
/// (byte = value*255); an sRGB view is then handed to egui, so the on-screen
/// texel matches the former CPU overlay (`Color32::from_rgba_unmultiplied(255,0,0,a)`
/// premultiplies to the same `(a,0,0,a)`).
pub fn overlay_tint(coverage: f32, strength: f32) -> [f32; 4] {
    let a = coverage.clamp(0.0, 1.0) * strength.clamp(0.0, 1.0);
    [a, 0.0, 0.0, a]
}
```

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p ferrolite-pipeline overlay_tint_is_premultiplied -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Write the tint WGSL shader**

Create `ferrolite-pipeline/src/shaders/mask_overlay_tint.wgsl`:

```wgsl
// Fullscreen-triangle overlay tint: sample the R32F coverage by framebuffer
// position and output PREMULTIPLIED red. Rendered into a linear Rgba8Unorm
// target whose dims equal the coverage dims (so textureLoad by pixel position is
// 1:1). An Rgba8UnormSrgb view of that target is handed to egui. Mirrors the
// pure `overlay_tint` in mask_overlay.rs.

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Oversized triangle covering the whole viewport.
    let x = f32(i32(vi) - 1) * 4.0;          // vi=0 -> -4, 1 -> 0? see below
    let y = f32(i32(vi & 1u) * 4 - 1);
    // Robust fullscreen triangle:
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    let pos = uv * 2.0 - vec2<f32>(1.0, 1.0);
    return vec4<f32>(pos, 0.0, 1.0);
}

@group(0) @binding(0) var coverage: texture_2d<f32>;

struct TintParams { strength: f32, _pad0: f32, _pad1: f32, _pad2: f32 };
@group(0) @binding(1) var<uniform> params: TintParams;

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = vec2<i32>(i32(frag.x), i32(frag.y));
    let c = textureLoad(coverage, px, 0).r;
    let a = clamp(c, 0.0, 1.0) * params.strength;
    return vec4<f32>(a, 0.0, 0.0, a); // premultiplied red
}
```

> Note: delete the unused `x`/`y` lines — keep only the `uv`/`pos` fullscreen-triangle. (They are shown for clarity; the shader only needs the `uv`→`pos` version.)

- [ ] **Step 6: Add `OverlayTexture`, the tint pipeline, and `overlay_texture()`**

Rewrite `ferrolite-pipeline/src/mask_overlay.rs`'s `MaskOverlayCompositor` to hold the tint pipeline and add the new method. Keep the existing `coverage(..)` method unchanged (still used by the app until Task 2). Replace the struct + `new` + add the method:

```rust
use std::sync::Arc;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{read_mask_r32f, MaskCompositor, MaskDefinition, RasterStore};

use crate::image::PipelineImage;

/// The linear render/storage format of the overlay target. An `Rgba8UnormSrgb`
/// view (added via `view_formats`) is what egui samples, so bytes written here
/// linearly (value*255) are interpreted by egui as sRGB — matching the former
/// managed overlay texture texel-for-texel.
const OVERLAY_LINEAR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const OVERLAY_SRGB_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// A GPU overlay texture: premultiplied red tint of a composited mask. Format is
/// `Rgba8Unorm` with an `Rgba8UnormSrgb` view format so it can be handed to egui.
pub struct OverlayTexture {
    pub texture: Arc<wgpu::Texture>,
    pub width: u32,
    pub height: u32,
}

impl OverlayTexture {
    /// An `Rgba8UnormSrgb` view — pass this to `register_native_texture`.
    pub fn srgb_view(&self) -> wgpu::TextureView {
        self.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(OVERLAY_SRGB_FORMAT),
            ..Default::default()
        })
    }
}

pub struct MaskOverlayCompositor {
    compositor: MaskCompositor,
    ctx: Arc<GpuContext>,
    tint_pipeline: wgpu::RenderPipeline,
    tint_bgl: wgpu::BindGroupLayout,
}

impl MaskOverlayCompositor {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        let tint_bgl = ctx
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mask-overlay-tint-bgl"),
                entries: &[
                    // 0: coverage (R32Float, non-filterable, textureLoad — no sampler)
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    // 1: TintParams uniform
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
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
        let module = ctx.shader_module(
            "mask-overlay-tint",
            include_str!("shaders/mask_overlay_tint.wgsl"),
        );
        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mask-overlay-tint-pl"),
                bind_group_layouts: &[&tint_bgl],
                push_constant_ranges: &[],
            });
        let tint_pipeline = ctx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("mask-overlay-tint-pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: "vs_main",
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: "fs_main",
                    targets: &[Some(OVERLAY_LINEAR_FORMAT.into())],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });
        Self {
            compositor: MaskCompositor::new(ctx.clone()),
            ctx,
            tint_pipeline,
            tint_bgl,
        }
    }

    /// Composite `def` against `input` (on the GPU) and tint it premultiplied red
    /// into a fresh `Rgba8Unorm` texture (dims = `input` dims). NO readback.
    pub fn overlay_texture(
        &self,
        def: &MaskDefinition,
        input: &PipelineImage,
        strength: f32,
    ) -> OverlayTexture {
        use wgpu::util::DeviceExt;
        let (w, h) = (input.width, input.height);
        let iv = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let coverage = self
            .compositor
            .composite(def, &iv, w, h, &RasterStore::default());

        let target = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mask-overlay-target"),
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OVERLAY_LINEAR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC, // COPY_SRC for the golden test readback
            view_formats: &[OVERLAY_SRGB_FORMAT],
        });
        let target = Arc::new(target);
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default()); // linear
        let cov_view = coverage
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let params: [f32; 4] = [strength.clamp(0.0, 1.0), 0.0, 0.0, 0.0];
        let ubuf = self
            .ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mask-overlay-tint-params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask-overlay-tint-bind"),
            layout: &self.tint_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&cov_view) },
                wgpu::BindGroupEntry { binding: 1, resource: ubuf.as_entire_binding() },
            ],
        });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mask-overlay-tint-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.tint_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.ctx.queue.submit([enc.finish()]);

        OverlayTexture { texture: target, width: w.max(1), height: h.max(1) }
    }
}
```

> Keep the existing `coverage(..)` method **and** its `FERROLITE_BRUSH_PROFILE` split block for now — Task 2 removes the app's only caller, and Task 3 deletes `coverage` + the probe. Keep `read_mask_r32f` in the `use` (still used by `coverage`).

- [ ] **Step 7: Re-export the new API from `lib.rs`**

In `ferrolite-pipeline/src/lib.rs`, extend the existing `mask_overlay` re-export line to include the new items. Find the line re-exporting `MaskOverlayCompositor` and add `OverlayTexture, overlay_tint`:

```rust
pub use mask_overlay::{overlay_tint, MaskOverlayCompositor, OverlayTexture};
```

- [ ] **Step 8: Write the golden GPU test (auto-skips headless)**

Add to the `tests` module in `ferrolite-pipeline/src/mask_overlay.rs`:

```rust
#[test]
fn overlay_texture_tints_premultiplied_red_ramp() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let oc = MaskOverlayCompositor::new(ctx.clone());
    // 8x1 mid-grey input; a left→right linear-gradient coverage.
    let src = ferrolite_image::LinearRgbaF32::new(8, 1, vec![0.5; 8 * 4]).unwrap();
    let img = crate::nodes::upload_source(&ctx, &src);
    let def = MaskDefinition {
        components: vec![(
            ferrolite_mask::MaskComponent::LinearGradient {
                start: ferrolite_mask::Vec2::new(0.0, 0.5),
                end: ferrolite_mask::Vec2::new(1.0, 0.5),
            },
            ferrolite_mask::CompositeMode::Add,
        )],
        invert: false,
    };
    let tex = oc.overlay_texture(&def, &img, 0.5);
    assert_eq!((tex.width, tex.height), (8, 1));
    // Read the LINEAR view bytes: byte == round(coverage*0.5*255), premultiplied.
    let bytes = ctx.read_rgba8(&tex.texture, 8, 1);
    // Every texel: green/blue zero, red == alpha (premultiplied).
    for x in 0..8usize {
        let px = &bytes[x * 4..x * 4 + 4];
        assert_eq!(px[1], 0, "green zero at {x}");
        assert_eq!(px[2], 0, "blue zero at {x}");
        assert_eq!(px[0], px[3], "red == alpha (premultiplied) at {x}");
    }
    // Alpha ramps left→right (coverage increases).
    assert!(bytes[3] < bytes[7 * 4 + 3], "alpha ramps L->R: {} !< {}", bytes[3], bytes[7 * 4 + 3]);
    // Full-ish coverage at the right edge is ~50% strength => ~128.
    assert!(bytes[7 * 4 + 3] > 96 && bytes[7 * 4 + 3] <= 130, "right edge ~50% alpha, got {}", bytes[7 * 4 + 3]);
}
```

- [ ] **Step 9: Run the crate's tests**

Run: `cargo test -p ferrolite-pipeline mask_overlay -- --nocapture`
Expected: PASS (the golden prints a skip line and returns if headless; passes on the dev GPU).

- [ ] **Step 10: Format, lint, commit**

Run: `cargo fmt && cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings`
Expected: clean.

```bash
git add ferrolite-pipeline/src/mask_overlay.rs ferrolite-pipeline/src/lib.rs ferrolite-pipeline/src/shaders/mask_overlay_tint.wgsl
git commit -m "feat(pipeline): GPU tint pass for the mask overlay (no readback)"
```

---

## Task 2: Wire the app to the native GPU overlay texture

Switch `rebuild_mask_overlay_if_needed` to `overlay_texture()` + a reused egui native texture, and change `mask_overlay::show` (and its `MaskTool::canvas` wrapper) to draw that `TextureId`. This removes the overlay-site probe block. The workspace builds and the overlay behaves identically.

**Files:**
- Modify: `ferrolite-app/src/state.rs:233` (add two `AppState` fields + init)
- Modify: `ferrolite-app/src/app.rs` (`rebuild_mask_overlay_if_needed`, ~1499–1620; the dispatch/extract site ~3926)
- Modify: `ferrolite-app/src/develop/mask_overlay.rs` (`show` signature + fill draw)
- Modify: `ferrolite-app/src/develop/tools/mask.rs` (`canvas` wrapper passes the `TextureId`)

**Interfaces:**
- Consumes: `ferrolite_pipeline::{MaskOverlayCompositor, OverlayTexture}`, `MaskOverlayCompositor::overlay_texture(def, input, strength) -> OverlayTexture`, `OverlayTexture::srgb_view()`, `egui_wgpu::Renderer::{register_native_texture, update_egui_texture_from_wgpu_texture}` (via `rs.renderer.write()`), `crate::develop::mask_overlay_color::{OVERLAY_MAX_EDGE, OVERLAY_STRENGTH}`.
- Produces: `AppState.mask_overlay_native: Option<egui::TextureId>` (app-global, single reused overlay texture); `mask_overlay::show(.., overlay_tex: Option<egui::TextureId>, ..)`.

- [ ] **Step 1: Add the `OVERLAY_STRENGTH` constant**

In `ferrolite-app/src/develop/mask_overlay_color.rs`, add next to `OVERLAY_MAX_EDGE`:

```rust
/// Red-overlay tint strength (alpha multiplier). Matches the former 50% tint.
pub const OVERLAY_STRENGTH: f32 = 0.5;
```

- [ ] **Step 2: Add the two `AppState` fields**

In `ferrolite-app/src/state.rs`, inside `pub struct AppState { .. }` (near the other UI/session fields, e.g. just before `pub viewer:` at line 112), add:

```rust
    /// App-global egui native texture id for the Develop mask overlay (GPU-tinted;
    /// no readback). Registered once, updated in place for whichever viewer is
    /// active — a single reused texture, so no per-image free is needed.
    pub mask_overlay_native: Option<egui::TextureId>,
    /// Keeps the current overlay `OverlayTexture` alive while egui's bind group
    /// references it. Replaced on each overlay rebuild.
    pub mask_overlay_gpu: Option<ferrolite_pipeline::OverlayTexture>,
```

And in `AppState::new`'s `Ok(Self { .. })` (state.rs:233), add:

```rust
            mask_overlay_native: None,
            mask_overlay_gpu: None,
```

- [ ] **Step 3: Rewrite the tail of `rebuild_mask_overlay_if_needed`**

In `ferrolite-app/src/app.rs`, replace the overlay-build block (from the `// PERF (non-blocking, follow-up):` comment and the `let (w, h2, cov) = oc.coverage(..)` probe block through `v.mask.overlay_key = Some(key);`) with the GPU-native path. The composite is computed inside the `v` scope; the `v` borrow is released before touching `self.state.mask_overlay_native` + the renderer (disjoint borrow of `frame`/`rs`).

Replace:

```rust
        let (Some(oc), Some(input)) = (v.mask_overlay.as_ref(), v.mask_overlay_input.as_ref())
        else {
            return;
        };
        // PERF (non-blocking, follow-up): ...  [entire comment]
        // TEMP brush-perf probe (measure-before-fix): ...  [entire probe block]
        let (w, h2, cov) = oc.coverage(&gpu_ctx, &def, input);
        ...
        v.mask.overlay_key = Some(key);
```

with:

```rust
        let overlay = {
            let (Some(oc), Some(input)) =
                (v.mask_overlay.as_ref(), v.mask_overlay_input.as_ref())
            else {
                return;
            };
            let overlay = oc.overlay_texture(
                &def,
                input,
                crate::develop::mask_overlay_color::OVERLAY_STRENGTH,
            );
            v.mask.overlay_key = Some(key);
            overlay
        };
        // `v` borrow ends here; the renderer + app-global texture id are disjoint.
        let view = overlay.srgb_view();
        {
            let mut renderer = rs.renderer.write();
            match self.state.mask_overlay_native {
                Some(id) => renderer.update_egui_texture_from_wgpu_texture(
                    &gpu_ctx.device,
                    &view,
                    wgpu::FilterMode::Linear,
                    id,
                ),
                None => {
                    let id = renderer.register_native_texture(
                        &gpu_ctx.device,
                        &view,
                        wgpu::FilterMode::Linear,
                    );
                    self.state.mask_overlay_native = Some(id);
                }
            }
        }
        self.state.mask_overlay_gpu = Some(overlay);
```

> The early-return branch at the top (no selected mask) still sets `v.mask_overlay_tex = None`; change that line to `v.mask.overlay_key = None;` only (drop the `mask_overlay_tex` write — that field is removed in the next step). Leaving the app-global native texture registered but undrawn is correct (a single reused texture).

- [ ] **Step 4: Remove the `mask_overlay_tex` field and its writes**

In `ferrolite-app/src/viewer/mod.rs`: delete the `pub mask_overlay_tex: Option<egui::TextureHandle>,` field (line ~213) and its `mask_overlay_tex: None,` initializer (line ~334). In `ferrolite-app/src/app.rs`, delete the two `v.mask_overlay_tex = None;` lines at ~240 and ~1131 (the `mask_overlay_input = None;` lines stay). The no-selection branch now clears only `v.mask.overlay_key = None;` (done in Step 3).

- [ ] **Step 5: Change `mask_overlay::show` to draw the native `TextureId`**

In `ferrolite-app/src/develop/mask_overlay.rs`, change the `show` signature parameter:

```rust
    overlay_tex: Option<egui::TextureId>,
```

and the fill draw at the top of `show`:

```rust
    if mask.overlay_on && !mask.adjusting {
        if let Some(tex_id) = overlay_tex {
            ui.painter().image(
                tex_id,
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }
```

- [ ] **Step 6: Update the `MaskTool::canvas` wrapper**

In `ferrolite-app/src/develop/tools/mask.rs`, change the extraction to read the app-global id and pass it (a `Copy` `TextureId`, no `.as_ref()`):

```rust
        let (stack, dims, tex, preview_source) = {
            let v = state.viewer.as_ref()?;
            (
                v.op_stack.clone(),
                v.image_dims.unwrap_or((1, 1)),
                state.mask_overlay_native,
                v.preview_source.clone(),
            )
        };
        let v = state.viewer.as_mut()?;
        crate::develop::mask_overlay::show(
            ui,
            image_rect,
            &stack,
            &mut v.mask,
            tex,
            dims,
            preview_source.as_ref(),
        )
```

> If `app.rs` has a second, older dispatch site for `mask_overlay::show` (search `mask_overlay::show(`), update it the same way: pass `self.state.mask_overlay_native` (a `Copy` `TextureId`) instead of `v.mask_overlay_tex.clone()`/`.as_ref()`.

- [ ] **Step 7: Build the app**

Run: `cargo build -p ferrolite-app --bin ferrolite-app`
Expected: compiles. Fix any borrow/type errors per the messages (the `overlay` scope must end before `self.state.*` is touched).

- [ ] **Step 8: Run app + pipeline tests**

Run: `cargo test -p ferrolite-app -p ferrolite-pipeline`
Expected: PASS.

- [ ] **Step 9: Format, lint, commit**

Run: `cargo fmt && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

```bash
git add ferrolite-app/src/state.rs ferrolite-app/src/app.rs ferrolite-app/src/viewer/mod.rs ferrolite-app/src/develop/mask_overlay.rs ferrolite-app/src/develop/tools/mask.rs ferrolite-app/src/develop/mask_overlay_color.rs
git commit -m "feat(develop): draw mask overlay from a GPU native texture (no readback)"
```

---

## Task 3: Remove `coverage`/instrumentation, verify, hand off

Delete the now-dead readback overlay path and the temporary profiling probe, run the full workspace gate, and hand over the measure-after + visual test plan.

**Files:**
- Modify: `ferrolite-pipeline/src/mask_overlay.rs` (delete `coverage` + its probe + the `brush_profile_enabled` helper)
- Modify: `ferrolite-app/src/app.rs` (delete the preview `ep.evaluate()` probe block)
- Modify: `ferrolite-app/src/diag.rs` (delete `brush_profile_enabled`)
- Modify: `ferrolite-app/src/develop/mask_overlay_color.rs` (delete `overlay_rgba` + its tests, now unused; keep `OVERLAY_MAX_EDGE` + `OVERLAY_STRENGTH`)

- [ ] **Step 1: Delete the dead readback overlay method + probe in the pipeline**

In `ferrolite-pipeline/src/mask_overlay.rs`: delete the entire `coverage(..)` method and the module-level `brush_profile_enabled()` fn added earlier. Update the `use` line to drop `read_mask_r32f` if it is no longer referenced in this file (the `tests` module's `imported`/`empty` tests live in `ferrolite-mask`, not here — but the old `linear_gradient_coverage_ramps_left_to_right` test **does** call `coverage`; delete that test, since Task 1's `overlay_texture_tints_premultiplied_red_ramp` now covers coverage-correctness through the overlay). Confirm `read_mask_r32f` is unused here and remove it from the `use`.

- [ ] **Step 2: Delete the preview-evaluate probe in the app**

In `ferrolite-app/src/app.rs` (`set_preview_and_full`), delete the `// TEMP brush-perf probe` block around `ep.evaluate()`, restoring:

```rust
            // keep the evaluate out of the lock scope to stay close to the
            // apply_full_decoded discipline.)
            let img = ep.evaluate();
            let mut renderer = rs.renderer.write();
```

- [ ] **Step 3: Delete the `brush_profile_enabled` gate in diag**

In `ferrolite-app/src/diag.rs`, delete the `brush_profile_enabled()` fn added earlier (and its doc comment).

- [ ] **Step 4: Delete the now-unused `overlay_rgba`**

In `ferrolite-app/src/develop/mask_overlay_color.rs`, delete `overlay_rgba` and its `#[cfg(test)] mod tests`. Keep `OVERLAY_MAX_EDGE` and `OVERLAY_STRENGTH`. (Confirm no other caller: `grep -rn overlay_rgba ferrolite-app/src` returns nothing.)

- [ ] **Step 5: Full workspace gate**

Run:
```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
Expected: all green. Fix any unused-import / dead-code fallout from the deletions.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "chore(develop): remove readback overlay path + brush-perf instrumentation"
```

- [ ] **Step 7: Measure-after + hand off the visual test (HOLD)**

Do NOT finish the branch. Hand the author this test plan and hold for hands-on results:

1. **Measure-after (optional sanity, needs a temporary reinstrument or trust the design):** the `FERROLITE_BRUSH_PROFILE` probe is now removed; the proof is the hands-on feel. If a number is wanted, note it can be re-added briefly.
2. **Smooth painting:** Open an image → Develop → Mask → Create New Mask → Brush. Paint several long, continuous strokes at both a **small** and a **large** brush radius. Expect: smooth, stall-free painting, no per-frame hitch; the red overlay tracks the growing stroke live. Failure signature: the multi-second/stuttery lag returns.
3. **Overlay correctness (unchanged behavior):**
   - Red tint appears over painted areas at ~50% and aligns with the brush.
   - Toggling the overlay off/on hides/shows the tint.
   - Dragging a mask **Light/Color slider** hides the tint while dragging (the `adjusting` suppression), then it returns.
   - The **Components window** Add section (Luma/Color) still shows the live prospective-component red preview as its sliders move.
   - Linear/Radial gradient + Color-range eyedropper overlays still draw and align.
   - Under **crop/rotate/zoom**, the overlay still aligns with image content.
4. **Regression:** switch between several images with masks — the overlay updates to the active image's mask; no stale tint from the previous image.

---

## Self-Review

**Spec coverage:**
- §3.1 tint render pass + `overlay_texture` → Task 1. ✓
- §3.1 `Rgba8UnormSrgb` constraint via `view_formats` (linear render, sRGB view) → Task 1 Step 6. ✓
- §3.2 register-once/update-in-place, no per-frame register → Task 2 Step 3. ✓
- §3.2 lifecycle without leaks → app-global single reused texture (AppState), no free plumbing → Task 2 Steps 2–3. ✓
- §3.3 `show` native-`TextureId` draw + signature change → Task 2 Steps 5–6. ✓
- §4 preserved behavior (adjusting gate, prospective preview, alignment) → unchanged draw + verified in Task 3 Step 7. ✓
- §5 non-goals (no pipeline/StrokeCursor/serde changes) → respected. ✓
- §7 tests: pure tint mapping + golden GPU diff → Task 1 Steps 1/8; measure-after + visual → Task 3 Step 7. ✓
- Instrumentation removed → Task 3. ✓

**Placeholder scan:** none — every code step has full code. (The WGSL Step 5 has an explicit "delete the unused x/y lines" instruction; the final shader is the `uv`/`pos` fullscreen triangle.)

**Type consistency:** `OverlayTexture { texture: Arc<wgpu::Texture>, width, height }` + `srgb_view()` defined in Task 1 and consumed identically in Task 2. `overlay_texture(def, input, strength)` signature matches between tasks. `mask_overlay_native: Option<egui::TextureId>` on `AppState` written in Task 2 Step 3 and read in Step 6. `overlay_tint(coverage, strength)` name consistent. `show(.., overlay_tex: Option<egui::TextureId>, ..)` matches its call sites.

**Note on premultiplied/sRGB (the one subtlety):** the target is `Rgba8Unorm` (linear store: byte = value·255) with an `Rgba8UnormSrgb` **view** handed to egui — so the sampled result equals the former managed overlay texture, which stored the same premultiplied `(a,0,0,a)` bytes and was likewise sampled as sRGB. The golden test (Task 1 Step 8) reads the linear view and asserts `red == alpha`, `g==b==0`, and the L→R alpha ramp, pinning this down.
