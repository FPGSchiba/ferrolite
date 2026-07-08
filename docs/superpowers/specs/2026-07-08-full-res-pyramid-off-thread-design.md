# Fast Image Open — Off-Thread Pyramids, Startup Pipeline Prewarm, Preview-Res Reveal — Design

> **Status:** Approved design (2026-07-08, revised after profiling). A focused performance fix.
> **Next step:** `superpowers:writing-plans` → implementation plan → subagent execution.

## 1. Problem (measured)

Opening an image freezes the UI: ~4.9 s on the first open, ~1.8 s on every subsequent open.
Timing instrumentation in `apply_full_decoded` (UI thread) measured (cold / warm ms):

| Phase | Cold | Warm | What it is |
|---|---|---|---|
| reveal `EditPipeline::new` + `evaluate` | 3056 | 674 | full-res rung-1 color-correct render |
| `GpuPyramidSource::new` | 1387 | 913 | CPU box-downsample pyramid + GPU upload (edit producer) |
| `PyramidTileSource::new` | 422 | 244 | CPU box-downsample pyramid + full-res clone (sparse VT source) |
| `TileEditPipeline::new` | 3 | 1 | construction only (cheap) |

Three independent causes, all on the UI thread inside `update()` (CLAUDE.md rule 1 violation):

1. **First-use driver pipeline compilation (~2.4 s, first open only).** `prewarm_shaders` compiles
   shader *modules* but never *evaluates* a pipeline, so the driver compiles each edit pipeline on
   its first dispatch — during the first real open's reveal `evaluate()`. `TileEditPipeline::new`
   at 1 ms warm proves construction is cheap; the cold→warm gaps are compilation.
2. **Two CPU pyramid builds (~1.2 s, every open).** `GpuPyramidSource::new` (913) +
   `PyramidTileSource::new` (244) both box-downsample the full-res image across mip levels.
3. **Full-res reveal render (~674 ms, every open).** `EditPipeline::evaluate()` does not block on a
   GPU readback; the cost is allocating ~11 full-res intermediate textures + bind groups + submits
   for the whole op chain at full resolution, for a transient rung-1 reveal.

## 2. Goal / non-goals

- **Goal:** image open stays interactive — first open and every open drop to well under ~200 ms of
  UI-thread work; full-res sharpness streams in a moment later.
- **Non-goals:** changing decode, shaders, the `ferrolite-gpu` executor, or the export path. No new
  dependencies. Not deduping the two pyramids' box-downsample (a possible later optimization; both
  move off-thread here, which already removes the freeze).

## 3. Hard constraints

- `GpuPyramidSource` (levels are `Arc<Texture>`) and the VT tile source (`Arc<dyn TileSource + Send
  + Sync>`, CPU levels) are both **`Send`** → build on a worker, deliver over the channel.
- `EditPipeline` and `TileEditPipeline` use `Rc` internally → **`!Send`** → must be built on the UI
  thread. (So the reveal render and the tile pipeline stay UI-thread; only the pyramids move.)
- `VirtualTexture::sparse` and pipeline builds need the eframe render state (`ViewerPipelines`,
  `GpuContext::from_render_state`) → UI thread.
- Driver pipeline compilation is triggered by first *dispatch* (evaluate/produce), not by
  `create_compute_pipeline` → prewarming must **evaluate** a dummy pipeline, not just build it.

## 4. Design — three coordinated changes (A + B + C)

### A. Prewarm pipeline objects at startup (kills cause #1)

Add `ferrolite_pipeline::prewarm_pipelines(ctx: Arc<GpuContext>)`: build a dummy `EditPipeline` on a
tiny (e.g. 64×64) `LinearRgbaF32` with `OpStack::default()` + identity matrix and call `evaluate()`
once (warms every point-op / HSL / color-grade / geometry / vignette / color-matrix pipeline); then
build a dummy `TileEditPipeline` (from a tiny `GpuPyramidSource`) and `produce_tile` one tile (warms
the geometry-head + tiled pipelines). Call it once at startup in `FerroliteApp::new`, right after
`prewarm_shaders`. Startup-only, on the UI thread (startup already builds `DisplayPipelines`); the
dummy pipelines are dropped immediately — only the driver's compiled-shader cache persists.

### B. Build both pyramids off the UI thread (kills cause #2)

- **New event:** `AppEvent::PyramidReady { image_id: i64, tile_source: Arc<dyn ferrolite_vt::TileSource + Send + Sync>, gpu_pyramid: Arc<ferrolite_pipeline::GpuPyramidSource> }`.
- **`apply_full_decoded` (UI thread):** reveal + install the preview holder **as today but with no
  full VT yet** (`full: None`); do NOT build `PyramidTileSource`, `GpuPyramidSource`,
  `VirtualTexture::sparse`, `TileEditPipeline`, or the producer here, and do NOT `set_producing`.
  Instead submit ONE Background job carrying an `Arc<GpuContext>` (from the render state) + the
  shared full-res image `Arc` (reuse the existing `raw_preview_source` clone — no extra copy).
- **Job (worker):** build `PyramidTileSource` (CPU) and `GpuPyramidSource` (CPU + GPU upload), send
  `AppEvent::PyramidReady { image_id, tile_source, gpu_pyramid }`, `request_repaint`. Cancel-aware.
- **`apply_pyramid_ready` (UI thread):** staleness guard (`image_id == current`); build the sparse
  full `VirtualTexture` from `tile_source` (needs `ViewerPipelines` + `GpuContext`), build
  `TileEditPipeline` from `gpu_pyramid` + the `EditTileProducer`, install `full` into the existing
  `ViewerGpu` holder, set `v.pyramid`/`v.edit_producer`, and `set_producing(true)` +
  `set_opstack_version`. This is the block moved out of `apply_full_decoded`, plus the sparse-VT
  construction (also moved here since it needs the off-thread `tile_source`).

Splitting the full-VT install to `PyramidReady` aligns with the existing preview-shown-until-full-ready
state machine (`full_ready`): during the ~1.2 s worker window the color-correct reveal (C) is shown;
the full VT swaps in when it lands. Stale results (rapid navigation) are dropped by the guard + cancel.

### C. Render the reveal at viewport resolution (kills cause #3)

The rung-1 reveal is a transient stopgap the sparse full VT replaces within a moment; at fit-zoom a
viewport-res reveal is pixel-identical to a full-res one (extra resolution is only visible zoomed, by
which point the VT has produced). In `apply_full_decoded`, before building the reveal `EditPipeline`,
box-downsample the full-res `image` **once** to fit the current viewport (fall back to a modest cap,
e.g. ≤ ~2 MP, when the viewport isn't known yet), and build/evaluate the reveal on that small source.
The op chain then allocates viewport-res intermediates instead of ~11 full-res textures, cutting the
reveal from ~674 ms to a small fraction. The full-res `image` `Arc` still goes to the pyramid job (B)
unchanged.

## 5. Projected result

First open ~4.9 s → **≲ ~150 ms** UI-thread; every subsequent open ~1.8 s → **≲ ~150 ms**. Full-res
sharpness streams in ~1.2 s later off-thread (was blocking; now non-blocking).

## 6. Testing

Threading/GPU/eframe glue — not meaningfully unit-testable (needs the live render state + a real
decode; `GpuPyramidSource`/pipelines need a GPU). Automated coverage: `prewarm_pipelines` gets a
GPU-gated integration test that it runs without panicking on a headless context; everything else is
verified by build + clippy + the existing suite staying green. The temporary `[open-profile]`
instrumentation is kept through implementation to re-measure, then removed. The **real gate is the
author's visual test**: open a large RAW and confirm (a) no freeze on first open or subsequent opens,
(b) the color-correct image shows immediately, (c) full-res resolves a beat later, (d) rapid
navigation shows the right image at full-res (no stale pyramid install), (e) editing after open still
updates the full-res view.

## 7. Scope summary

| Change | File(s) |
|---|---|
| A: `prewarm_pipelines` + startup call | `ferrolite-pipeline/src/lib.rs` (or `pipeline.rs`), `ferrolite-app/src/app.rs` (startup) |
| B: `PyramidReady` event + Debug derives | `ferrolite-app/src/events.rs`, `ferrolite-pipeline/src/{gpu_pyramid,image}.rs` |
| B: submit job; preview-only holder | `ferrolite-app/src/app.rs` (`apply_full_decoded`) |
| B: install full VT + producer on ready | `ferrolite-app/src/app.rs` (`apply_pyramid_ready` + loop arm) |
| C: viewport-res reveal downsample | `ferrolite-app/src/app.rs` (`apply_full_decoded`) |
| remove temporary profiling instrumentation | `ferrolite-app/src/app.rs` |

No changes to decode, shaders, `ferrolite-gpu` executor, or the export path. No new dependencies.
