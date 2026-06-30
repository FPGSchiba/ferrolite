# ferrolite — Display-pipeline reuse & pre-warm (design)

> **Status:** Design — approved by user (approach: reuse + pre-warm); pending writing-plans.
> **Date:** 2026-06-30
> **Branch:** `feat/viewer-and-vt-ladder` (UX/perf fix before finishing the branch).

---

## 1. Problem

Opening an image freezes the UI thread for ~841ms (debug) building a wgpu render pipeline in
`VirtualTexture::single_texture`. Evidence (instrumented run): `apply_preview_ready` →
`single_texture (pipeline) built in 841ms`. The render pipeline (shader module + bind-group layout
+ pipeline) is **image-independent** — it depends only on the WGSL shader and the `target_format` —
yet each of the four VT display constructors (`single_texture`, `tiled_resident`, `streaming`,
`sparse`) rebuilds it from scratch on **every** open. The per-image parts (texture, bind group,
uniform buffer, sampler) are cheap.

## 2. Goal

Build each display pipeline **once and reuse** it across all opens, and **pre-warm** them at startup,
so opening an image never pays a pipeline-compile cost on the UI thread. Rendered output is
unchanged (golden tests stay green).

## 3. Design

### 3.1 `ferrolite_vt::DisplayPipelines` (new)
A cache, built once for a given `wgpu::TextureFormat`, holding the reusable GPU objects for all four
display variants:
- one shared `wgpu::ShaderModule` (from `display.wgsl`),
- per variant (`Single`, `Tiled`, `Streaming`, `Sparse`): the `wgpu::BindGroupLayout` + the
  `wgpu::RenderPipeline`,
- a shared `wgpu::Sampler` (filtering),

API:
```
pub enum DisplayVariant { Single, Tiled, Streaming, Sparse }
pub struct DisplayPipelines { /* shader, sampler, per-variant (bgl, pipeline), target_format */ }
impl DisplayPipelines {
    /// Build (compile) all four pipelines for `target_format`. Call once (pre-warm).
    pub fn new(ctx: &GpuContext, target_format: wgpu::TextureFormat) -> Self;
    pub fn layout(&self, v: DisplayVariant) -> &wgpu::BindGroupLayout;
    pub fn pipeline(&self, v: DisplayVariant) -> &wgpu::RenderPipeline;
    pub fn sampler(&self) -> &wgpu::Sampler;
    pub fn target_format(&self) -> wgpu::TextureFormat;
}
```
The exact bind-group-layout entries per variant are moved verbatim from the current constructors
(single: tex+sampler+uniform; tiled/streaming/sparse: their existing extra bindings — array texture,
slots/meta, page-table, feedback). No layout/shader/pipeline-state changes — only their construction
moves into `DisplayPipelines::new`.

### 3.2 VT constructors consume the cache
The four constructors change from `(ctx, …, target_format)` to `(ctx, …, pipelines: &DisplayPipelines)`.
They build only the **per-image** resources (texture(s), uniform buffer, bind group referencing
`pipelines.layout(variant)` + `pipelines.sampler()`), and store an `Arc`/clone of
`pipelines.pipeline(variant)` for `draw_*`. They assert `pipelines.target_format()` matches what they
render to (debug_assert). `render_*_to_image` golden helpers gain a `&DisplayPipelines` arg (built
once per test).

`wgpu::RenderPipeline`/`BindGroupLayout`/`Sampler` are `Clone` (cheap Arc-backed handles) in wgpu 22,
so the VT can hold clones without lifetime entanglement.

### 3.3 App: persist one cache + pre-warm at startup
- `FerroliteApp::new` (has `cc.wgpu_render_state`): build a `GpuContext` + `DisplayPipelines::new(ctx,
  target_format)` once and store both in eframe `callback_resources` (a small `ViewerPipelines`
  holder, persistent like `CanvasResources`). This compiles all four pipelines at startup, off the
  interaction path.
- `apply_preview_ready` / `apply_full_decoded`: fetch the persistent `DisplayPipelines` (and a
  `GpuContext`) from `callback_resources` instead of building pipelines, and pass `&DisplayPipelines`
  to the VT constructors. Opening then only creates the per-image texture + bind group (sub-ms).

### 3.4 What does NOT change
The WGSL, the bind-group layouts, the pipeline state, the rendered output, the VT streaming/feedback
logic, and all public render/draw entry points' behavior. Only *where/when* pipelines are created.

## 4. Scope & files
- `ferrolite-vt/src/pipelines.rs` (new) — `DisplayVariant`, `DisplayPipelines` (+ the per-variant
  layout/pipeline construction moved from `view.rs`).
- `ferrolite-vt/src/view.rs` — four constructors take `&DisplayPipelines`; build per-image resources
  only; store the cloned pipeline handle for drawing. `render_*_to_image` helpers take it too.
- `ferrolite-vt/src/lib.rs` — export `DisplayPipelines`, `DisplayVariant`.
- `ferrolite-vt/tests/golden.rs` — build a `DisplayPipelines` once per test and pass it.
- `ferrolite-app/src/app.rs` — build + store `GpuContext` + `DisplayPipelines` at startup
  (pre-warm); `apply_preview_ready`/`apply_full_decoded` use the cached pipelines.
- `ferrolite-app/src/viewer/callback.rs` — `ViewerGpu` may hold the shared `DisplayPipelines`
  reference/clone if `draw_*` needs it (it stores the pipeline handle in the VT already).

## 5. Testing
- The existing golden tests (rung1–4) are the correctness gate — output must be **byte-identical**
  (within the existing tolerance) after the refactor, proving reuse didn't change rendering.
- No new pure-logic units (the change is GPU resource lifetime). Gate: `cargo build` + `cargo clippy
  --workspace --all-targets -- -D warnings` + `cargo test --workspace` green (incl. goldens on the
  dev GPU), plus the user's manual smoke: opening an image no longer hitches; a re-instrumented run
  (optional) shows the per-open pipeline build gone.

## 6. Repo rule (separate deliverable)
Create `CLAUDE.md` at the repo root with a "Responsiveness & threading" rule (two clauses):
1. **No blocking/heavy CPU or I/O on the UI/update thread** — RAW/image decode, file & DB I/O,
   ingest walks, thumbnail generation, and any multi-millisecond CPU work must be submitted to
   `ferrolite-jobs` (priority + cancellation token) and delivered back via the app event channel,
   then `ctx.request_repaint()`. UI-thread list/grid/strip rendering must be virtualized (visible
   items only) so it never does O(all-items) decode work per frame.
2. **GPU work stays on the render thread but must be bounded** — build pipelines/shaders **once and
   reuse** (never per interaction/open), pre-warm expensive pipelines at startup, and stream/upload
   incrementally (the VT) rather than in one synchronous build. Anything that could exceed a frame
   budget on open/navigation must be profiled.

## 7. Out of scope (YAGNI)
Multi-format pipeline caching (a single `target_format` is used app-wide), a generic pipeline cache
on `GpuContext`, async/off-thread GPU resource creation, and a `wgpu::PipelineCache` (disk shader
cache). Just build-once + reuse + pre-warm for the one display format in use.

## 8. Decisions recorded (2026-06-30)
| Question | Decision |
|---|---|
| Fix approach | **Reuse + pre-warm** (build pipelines once, pre-warm at startup) — not offload-to-job. |
| Cache location | `ferrolite-vt::DisplayPipelines` (pipeline-building knowledge stays in the VT crate, not the generic `ferrolite-gpu`). |
| Repo rule | **Adopt** the two-clause Responsiveness & threading rule in a new root `CLAUDE.md`. |
