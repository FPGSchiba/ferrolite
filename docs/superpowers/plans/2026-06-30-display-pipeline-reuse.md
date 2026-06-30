# Display-Pipeline Reuse & Pre-warm Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop rebuilding the image-independent wgpu display pipelines on every image open; build them once, reuse across opens, pre-warm at startup — eliminating the ~841ms UI-thread freeze on open.

**Architecture:** Extract the (image-independent) shader/bind-group-layout/pipeline/sampler construction for all four VT display variants into a new `ferrolite_vt::DisplayPipelines` cache built once per `target_format`. The four `VirtualTexture` constructors take `&DisplayPipelines` and build only per-image resources (texture + bind group + uniform). The app builds + stores one `DisplayPipelines` at startup (pre-warm) in eframe `callback_resources` and passes it on every open.

**Tech Stack:** Rust 2021, wgpu 22.1, egui/eframe 0.29, `ferrolite-gpu`/`ferrolite-vt`/`ferrolite-app`.

## Global Constraints

- Stay on branch `feat/viewer-and-vt-ladder`; no new branch. `cargo fmt --all` before each commit. Keep `cargo clippy --workspace --all-targets -- -D warnings` exit 0 and `cargo test --workspace` green. Conventional commits, no attribution footer.
- **Rendered output must not change** — the golden tests (rung1–4) are the correctness gate and must stay green within their existing tolerances. The refactor only moves *where/when* pipelines are created; WGSL, bind-group layouts, pipeline state, and draw logic are unchanged.
- Engine-tier purity: `DisplayPipelines` lives in `ferrolite-vt` (it knows the VT shaders/layouts); `ferrolite-gpu` stays generic. Permissive deps only.
- `wgpu::RenderPipeline`/`BindGroupLayout`/`Sampler` are `Clone` (Arc-backed) in wgpu 22 — the VT may store clones.

## File Structure
- `ferrolite-vt/src/pipelines.rs` (create) — `DisplayVariant`, `DisplayPipelines`.
- `ferrolite-vt/src/view.rs` (modify) — 4 constructors + `render_*_to_image` helpers take `&DisplayPipelines`; build per-image resources only; store cloned pipeline handle for `draw_*`.
- `ferrolite-vt/src/lib.rs` (modify) — export `DisplayPipelines`, `DisplayVariant`.
- `ferrolite-vt/tests/golden.rs` (modify) — build a `DisplayPipelines` once per test; pass to constructors.
- `ferrolite-app/src/app.rs` (modify) — build+store `GpuContext`+`DisplayPipelines` at startup; use them on open.
- `ferrolite-app/src/viewer/callback.rs` (modify if needed) — `ViewerGpu` holds what `draw_*` needs.
- `CLAUDE.md` (create) — responsiveness/threading repo rule.

---

## Task 1: `DisplayPipelines` cache + refactor VT constructors to use it

**Files:** Create `ferrolite-vt/src/pipelines.rs`; modify `ferrolite-vt/src/view.rs`, `ferrolite-vt/src/lib.rs`, `ferrolite-vt/tests/golden.rs`.

**Interfaces:**
- Produces:
  `pub enum DisplayVariant { Single, Tiled, Streaming, Sparse }`
  `pub struct DisplayPipelines { … }` with `new(ctx: &GpuContext, target_format: wgpu::TextureFormat) -> Self`, `layout(&self, DisplayVariant) -> &wgpu::BindGroupLayout`, `pipeline(&self, DisplayVariant) -> &wgpu::RenderPipeline`, `sampler(&self) -> &wgpu::Sampler`, `target_format(&self) -> wgpu::TextureFormat`.
- Changed: `VirtualTexture::{single_texture, tiled_resident, streaming, sparse}` and `{render_to_image, render_tiled_to_image, render_streaming/render_sparse offscreen helpers}` replace their `target_format: wgpu::TextureFormat` parameter with `pipelines: &DisplayPipelines`.

This is one cohesive task: the cache, the four constructors, and the golden harness must change together to keep compiling and keep the goldens green. Work incrementally inside the task, variant by variant, running the golden suite as you go.

- [ ] **Step 1: Create `DisplayPipelines` by moving the existing build code**

Create `ferrolite-vt/src/pipelines.rs`. For each of the four variants, MOVE (don't rewrite) the exact `create_bind_group_layout`, `create_shader_module`, `create_pipeline_layout`, `create_render_pipeline`, and `create_sampler` calls that currently live inline in the four `view.rs` constructors (single ≈ view.rs:186-247; tiled, streaming, sparse at their respective sites). The shader module (`include_str!("shaders/display.wgsl")`) is built once and shared by all four pipelines; each variant uses its own bind-group-layout + entry points (`fs_main`, `fs_tiled`, `fs_sparse`, etc. — copy the exact `entry_point`s the current constructors use). Keep one shared filtering `Sampler`.

```rust
//! Cached, image-independent wgpu display pipelines. Built once per target
//! format (pre-warmed at startup) and reused for every image open, so opening
//! an image never pays a pipeline-compile cost on the UI thread.

use ferrolite_gpu::GpuContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayVariant {
    Single,
    Tiled,
    Streaming,
    Sparse,
}

pub struct DisplayPipelines {
    target_format: wgpu::TextureFormat,
    sampler: wgpu::Sampler,
    single: (wgpu::BindGroupLayout, wgpu::RenderPipeline),
    tiled: (wgpu::BindGroupLayout, wgpu::RenderPipeline),
    streaming: (wgpu::BindGroupLayout, wgpu::RenderPipeline),
    sparse: (wgpu::BindGroupLayout, wgpu::RenderPipeline),
}

impl DisplayPipelines {
    pub fn new(ctx: &GpuContext, target_format: wgpu::TextureFormat) -> Self {
        let device = &ctx.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vt-display"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/display.wgsl").into()),
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vt-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // For each variant: build its bind_group_layout (entries copied verbatim
        // from the current constructor) + pipeline_layout + render_pipeline using
        // `&shader` and `target_format`. Helper to cut repetition:
        let mk = |bgl: &wgpu::BindGroupLayout, vs: &str, fs: &str| -> wgpu::RenderPipeline {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vt-pl"),
                bind_group_layouts: &[bgl],
                push_constant_ranges: &[],
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vt-pipeline"),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: vs,
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: fs,
                    targets: &[Some(target_format.into())],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };
        // single_bgl/tiled_bgl/streaming_bgl/sparse_bgl: copy the exact
        // BindGroupLayoutDescriptor entries from the four current constructors.
        // (single: tex@0, sampler@1, uniform@2; tiled: + array-tex@3, slots@4,
        //  meta@5; streaming: same family as the current streaming ctor; sparse:
        //  + page_table@6, feedback@7.) Use the matching vs/fs entry points.
        // ... build single_bgl, tiled_bgl, streaming_bgl, sparse_bgl ...
        // let single = (single_bgl, mk(&single_bgl, "vs_main", "fs_main"));
        // ... etc, then:
        Self { target_format, sampler, single, tiled, streaming, sparse }
    }

    pub fn target_format(&self) -> wgpu::TextureFormat {
        self.target_format
    }
    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }
    pub fn layout(&self, v: DisplayVariant) -> &wgpu::BindGroupLayout {
        match v {
            DisplayVariant::Single => &self.single.0,
            DisplayVariant::Tiled => &self.tiled.0,
            DisplayVariant::Streaming => &self.streaming.0,
            DisplayVariant::Sparse => &self.sparse.0,
        }
    }
    pub fn pipeline(&self, v: DisplayVariant) -> &wgpu::RenderPipeline {
        match v {
            DisplayVariant::Single => &self.single.1,
            DisplayVariant::Tiled => &self.tiled.1,
            DisplayVariant::Streaming => &self.streaming.1,
            DisplayVariant::Sparse => &self.sparse.1,
        }
    }
}
```
Add `mod pipelines; pub use pipelines::{DisplayPipelines, DisplayVariant};` to `ferrolite-vt/src/lib.rs`.

- [ ] **Step 2: Refactor the four constructors to take `&DisplayPipelines`**

For each constructor (`single_texture`, `tiled_resident`, `streaming`, `sparse`): change the trailing `target_format: wgpu::TextureFormat` param to `pipelines: &DisplayPipelines`. Delete the inline shader/bgl/pipeline-layout/pipeline/sampler creation. Build the per-image texture(s) + uniform buffer exactly as before. Build the bind group against `pipelines.layout(DisplayVariant::X)` and `pipelines.sampler()`. Store `pipelines.pipeline(DisplayVariant::X).clone()` (RenderPipeline is Clone) in the variant's resources struct for `draw_*`. Add `debug_assert_eq!(pipelines.target_format(), <the format this VT renders to>)` where a format was previously taken — for the offscreen `render_*_to_image` helpers, the format is `Rgba8Unorm`; for the app path it is the surface format, so the app must build `DisplayPipelines` with the surface `target_format` (see Task 2).

Example (single_texture):
```rust
pub fn single_texture(
    ctx: &GpuContext,
    image: &LinearRgbaF32,
    pipelines: &DisplayPipelines,
) -> Self {
    let device = &ctx.device;
    let texels: Vec<f16> = image.pixels.iter().map(|&v| f16::from_f32(v)).collect();
    let texture = device.create_texture_with_data(/* …unchanged… */);
    // No bgl/shader/pipeline/sampler creation here anymore.
    Self {
        single: Some(SingleResources {
            texture,
            // store clones of the cached pipeline + (a reference to) the layout/sampler
            // as needed by prepare_single/draw_single:
            pipeline: pipelines.pipeline(DisplayVariant::Single).clone(),
            bind_group_layout: pipelines.layout(DisplayVariant::Single).clone(),
            sampler: pipelines.sampler().clone(),
            uniform_buf: /* created as today */,
            bind_group: /* built lazily in prepare_single, as today */,
            // …
        }),
        // other variants None
        image_dims: (image.width, image.height),
    }
}
```
(`prepare_single`/`draw_single` and the tiled/streaming/sparse `prepare_*`/`draw_*` keep working — they now read the pipeline/layout/sampler from the stored clones instead of self-built ones. Keep their bodies otherwise unchanged.)

Update the offscreen helpers `render_to_image`/`render_tiled_to_image` (and any `render_streaming`/`render_sparse` test helpers) to take `&DisplayPipelines` and pass it through to the constructor they call, instead of a `target_format`.

- [ ] **Step 3: Update the golden tests to build `DisplayPipelines` once**

In `ferrolite-vt/tests/golden.rs`, each test that builds a VT now first builds the cache:
```rust
let ctx = match GpuContext::headless() { Some(c) => c, None => { eprintln!("no GPU adapter; skipping"); return; } };
let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
// pass &pipelines wherever the test previously passed wgpu::TextureFormat::Rgba8Unorm
```
Keep every assertion and tolerance identical. The goldens (rung1_fit PNG, rung2/3 relative diffs, rung4 residency) MUST pass unchanged — that proves the refactor didn't alter rendering.

- [ ] **Step 4: Build, lint, and run the golden suite (the gate)**

```bash
cargo build -p ferrolite-vt
cargo clippy -p ferrolite-vt --all-targets -- -D warnings
cargo test -p ferrolite-vt
```
Expected: build OK; clippy exit 0; ALL `ferrolite-vt` tests pass, including the 4 GPU goldens (rung1–4) on the dev GPU — byte-identical/within tolerance to before. If a golden drifts, the refactor changed rendering — STOP and find what differs (likely a wrong entry point or bgl entry mismatch per variant); do not adjust tolerances.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add ferrolite-vt/src/pipelines.rs ferrolite-vt/src/view.rs ferrolite-vt/src/lib.rs ferrolite-vt/tests/golden.rs
git commit -m "refactor(vt): cache display pipelines in DisplayPipelines; constructors reuse them"
```

---

## Task 2: App pre-warms + reuses `DisplayPipelines` on open

**Files:** Modify `ferrolite-app/src/app.rs` (and `ferrolite-app/src/viewer/callback.rs` if `ViewerGpu` needs the cache).

**Interfaces:**
- Consumes: `ferrolite_vt::DisplayPipelines`, `ferrolite_gpu::GpuContext`.
- A persistent holder stored in `callback_resources` at startup, e.g. `struct ViewerPipelines { pipelines: DisplayPipelines }` (plus a persistent `GpuContext` if convenient).

- [ ] **Step 1: Build + store the cache at startup (pre-warm)**

In `FerroliteApp::new`, inside `if let Some(rs) = cc.wgpu_render_state.as_ref() { … }`, after inserting `CanvasResources`, build and insert the pipeline cache using the surface format:
```rust
let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
let pipelines = ferrolite_vt::DisplayPipelines::new(&gpu, rs.target_format);
rs.renderer
    .write()
    .callback_resources
    .insert(crate::viewer::ViewerPipelines { pipelines });
```
Define `ViewerPipelines { pub pipelines: ferrolite_vt::DisplayPipelines }` (e.g. in `viewer/callback.rs` or `viewer/mod.rs`). This compiles all four pipelines once at startup, off the open path.

- [ ] **Step 2: Use the cached pipelines in `apply_preview_ready`**

Replace the per-open pipeline build. `apply_preview_ready` currently does `single_texture(&gpu, &linear, rs.target_format)`. Change to fetch the cached `DisplayPipelines` from `callback_resources` and pass it:
```rust
let mut renderer = rs.renderer.write();
let pipelines = &renderer
    .callback_resources
    .get::<crate::viewer::ViewerPipelines>()
    .expect("ViewerPipelines pre-warmed at startup")
    .pipelines;
let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
let linear = viewer::load::preview_to_linear(image);
let dims = (linear.width, linear.height);
let vt = ferrolite_vt::VirtualTexture::single_texture(&gpu, &linear, pipelines);
// …then insert ViewerGpu as before (drop the renderer borrow first if needed for borrow rules)…
```
Mind the borrow: you read `ViewerPipelines` and then `insert(ViewerGpu)` into the same `callback_resources` — structure the borrows so they don't overlap (e.g. build `vt` while borrowing `pipelines`, drop that borrow, then insert `ViewerGpu`). Keep the rest of `apply_preview_ready` (fit, `image_dims`, `loaded`, Standard→idle) unchanged.

- [ ] **Step 3: Use the cached pipelines in `apply_full_decoded`**

Same pattern: `VirtualTexture::sparse(&gpu, source, jobs, VIEWER_TILE_BUDGET, pipelines)` instead of passing `rs.target_format`. Preserve the existing "install full only after confirming holder image_id matches → set full_ready/begin_crossfade" ordering.

- [ ] **Step 4: Build, lint, test**

```bash
cargo build --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all green incl. goldens. The pipelines are now built once at startup; opens create only per-image resources.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/app.rs ferrolite-app/src/viewer/
git commit -m "perf(app): pre-warm + reuse display pipelines on image open (no per-open compile)"
```

- [ ] **Step 6: Manual smoke (user)**

Open a large RAW: it switches to Develop with a spinner and the image appears with **no multi-hundred-ms freeze** at the preview step; subsequent opens are instant. (Optional: re-add the temporary timing prints to confirm the `single_texture` build is gone from the open path.)

---

## Task 3: Repo rule — `CLAUDE.md` responsiveness & threading

**Files:** Create `CLAUDE.md` at the repo root.

- [ ] **Step 1: Write the rule**

Create `CLAUDE.md`:
```markdown
# ferrolite — repo conventions for Claude

## Responsiveness & threading (load-bearing)

1. **Never block the UI/update thread.** RAW/image decode, file & DB I/O, ingest
   directory walks, thumbnail generation, and any multi-millisecond CPU work MUST
   be submitted to `ferrolite-jobs` (with a priority + cancellation token) and
   delivered back over the app event channel, after which the job calls
   `ctx.request_repaint()`. UI-thread list/grid/filmstrip rendering MUST be
   virtualized (realize + decode only the items currently on screen) so it never
   does O(all-items) work per frame.

2. **GPU work stays on the render thread but must be bounded.** Build
   pipelines/shaders ONCE and reuse them (never rebuild per image/open/interaction);
   pre-warm expensive pipelines at startup; stream/upload incrementally (the sparse
   VT) rather than in one synchronous build. Profile anything that could exceed a
   frame budget on open or navigation.

These two rules exist because both were violated and caused multi-second UI
freezes on image open (eager per-frame thumbnail decode in the filmstrip; a
per-open render-pipeline rebuild). Keep them honored.
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: add CLAUDE.md responsiveness & threading repo rule"
```

---

## Self-Review

**Spec coverage:** §3.1 `DisplayPipelines` → Task 1 Step 1; §3.2 constructors consume cache → Task 1 Steps 2-3; §3.3 app pre-warm + reuse → Task 2; §4 files → all covered; §5 goldens-as-gate → Task 1 Step 4 + Task 2 Step 4; §6 repo rule → Task 3. ✓

**Placeholder scan:** Task 1 Step 1 intentionally says "copy the exact bind-group-layout entries / entry points from the current constructors" rather than reproducing all four verbatim (they already exist in `view.rs` and must be moved unchanged) — this is a MOVE instruction, not a vague placeholder; the golden suite (Step 4) verifies correctness exactly. No TBD/TODO.

**Type consistency:** `DisplayPipelines::{new, layout, pipeline, sampler, target_format}` and `DisplayVariant::{Single,Tiled,Streaming,Sparse}` used consistently; the four constructors' `target_format` param → `&DisplayPipelines` consistently across view.rs, goldens, and app; `ViewerPipelines` holder name consistent between Task 2 Steps 1-3.
