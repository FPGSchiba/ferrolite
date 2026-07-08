# Fast Image Open (Off-Thread Pyramids + Startup Prewarm + Preview-Res Reveal) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the image-open UI freeze (~4.9 s first open, ~1.8 s after) measured to three UI-thread causes, dropping open to ≲ ~150 ms of UI-thread work.

**Architecture:** (A) prewarm the edit pipeline *objects* at startup so first-use driver shader compilation happens then, not on first open; (B) build both CPU/GPU pyramids on a `ferrolite-jobs` worker, delivered via a new `AppEvent::PyramidReady` and installed (with the `Rc`/`!Send` tile pipeline + sparse VT) on the UI thread; (C) render the transient rung-1 reveal at viewport resolution instead of full-res.

**Tech Stack:** Rust, `ferrolite-app` (eframe/egui + `ferrolite-jobs`), `ferrolite-pipeline` (`EditPipeline`/`TileEditPipeline`/`GpuPyramidSource`), `ferrolite-vt` (`PyramidTileSource`/`VirtualTexture`), `ferrolite-gpu` (`GpuContext`), `wgpu`.

## Global Constraints

From the design (`docs/superpowers/specs/2026-07-08-full-res-pyramid-off-thread-design.md`).

- **Never block the UI/update thread (CLAUDE.md rule 1).** The two pyramid builds MUST run on a worker. Startup prewarm is on the UI thread but startup-only (like `DisplayPipelines`).
- **`Send` vs `!Send`:** `GpuPyramidSource` and `Arc<dyn TileSource + Send + Sync>` are `Send` → built off-thread, delivered over the channel. `EditPipeline`/`TileEditPipeline` use `Rc` → `!Send` → built ONLY on the UI thread.
- **Driver compilation triggers on first *dispatch*,** not `create_compute_pipeline` → prewarm must `evaluate()` a dummy pipeline.
- **Color-correctness invariant:** the full VT must pass through the edit producer; it stays not-producing until the producer exists (in `apply_pyramid_ready`). During the off-thread gap the color-correct reveal is shown.
- **Staleness:** Background job + cancel token; `apply_pyramid_ready` guards `image_id == current` and drops stale results.
- **Scope:** `ferrolite-app` + a `prewarm_pipelines` fn and `Debug` derives in `ferrolite-pipeline`. No decode/shader/executor/export changes. No new dependencies.
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean; no `unwrap()` outside tests.

**Branch:** `feat/full-res-pyramid-off-thread` off `main` (already created; spec/plan committed; temporary `[open-profile]` instrumentation from commit `9ca300e` is present and kept until Task 4).

> **Testing note (applies to all tasks):** this is eframe/GPU/threading glue that needs the live render state + a real decode; `GpuPyramidSource`/pipelines need a GPU. The only honest automated test is a GPU-gated "does not panic" check for `prewarm_pipelines` (Task 1). Every other task's automated gate is build + `clippy --workspace --all-targets -D warnings` + the existing suite staying green; correctness is confirmed by the author's visual test (Task 4). A contrived test that asserts nothing is a defect — do not add one.

---

### Task 1: Prewarm edit pipeline objects at startup (fix A)

**Files:**
- Modify: `ferrolite-pipeline/src/lib.rs` (add `prewarm_pipelines`; re-export if needed)
- Modify: `ferrolite-app/src/app.rs` (call it at startup after `prewarm_shaders`, app.rs ~77)
- Test: `ferrolite-pipeline/tests/` (GPU-gated no-panic test) — reuse an existing golden test file's `GpuContext::headless()` pattern

**Interfaces:**
- Produces: `pub fn prewarm_pipelines(ctx: std::sync::Arc<ferrolite_gpu::GpuContext>)` in `ferrolite-pipeline`.
- Consumes: `EditPipeline`, `TileEditPipeline`, `GpuPyramidSource`, `OpStack`, `ferrolite_image::LinearRgbaF32`.

- [ ] **Step 1: Write a GPU-gated no-panic test**

Add `ferrolite-pipeline/tests/prewarm.rs`:
```rust
use ferrolite_gpu::GpuContext;

#[test]
fn prewarm_pipelines_runs_without_panicking() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Must not panic; warms driver pipeline compilation for the edit passes.
    ferrolite_pipeline::prewarm_pipelines(std::sync::Arc::new(ctx));
}
```
Run: `cargo test -p ferrolite-pipeline --test prewarm` — Expected: FAIL to compile (`prewarm_pipelines` undefined).

- [ ] **Step 2: Implement `prewarm_pipelines`**

Add to `ferrolite-pipeline/src/lib.rs` (near `prewarm_shaders`). It builds + evaluates a tiny dummy `EditPipeline`, then builds + produces one tile from a tiny `TileEditPipeline`, so the driver compiles every edit pipeline now:
```rust
/// Force first-use driver compilation of every edit pipeline at startup by
/// building + evaluating tiny dummy `EditPipeline`/`TileEditPipeline`s. Companion
/// to `prewarm_shaders` (which only compiles shader MODULES): the driver compiles
/// a pipeline on its first DISPATCH, so we must evaluate once here, not merely
/// construct. Startup-only; the dummies are dropped, only the driver's cache
/// persists. Call once, after `prewarm_shaders`, on the render thread.
pub fn prewarm_pipelines(ctx: std::sync::Arc<ferrolite_gpu::GpuContext>) {
    const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    // 64×64 opaque grey dummy — size-independent for compilation.
    let px = vec![0.5f32; 64 * 64 * 4];
    let img = ferrolite_image::LinearRgbaF32::new(64, 64, px).expect("dummy image");

    // Whole-image edit chain (reveal + preview path).
    let mut ep = EditPipeline::new(ctx.clone(), &img, OpStack::default(), IDENTITY);
    let _ = ep.evaluate();

    // Tiled edit chain (full-res producer path: geometry-head + tiled passes).
    let pyramid = std::sync::Arc::new(GpuPyramidSource::new(&ctx, &img));
    let mut tep = TileEditPipeline::new(ctx, pyramid, OpStack::default(), IDENTITY, None, None);
    let _ = tep.produce_tile(ferrolite_image::TileCoord { lod: 0, x: 0, y: 0 });
}
```
> Confirm the exact `TileEditPipeline::new` signature and `produce_tile` argument type against `tile_edit.rs` (the color-grading branch used `TileEditPipeline::new(ctx, pyramid, stack, cam, lens_warp, lens_vignette)` and `produce_tile(TileCoord)`); match them. If `TileCoord`'s path differs (`ferrolite_image::TileCoord`), use the correct path. Ensure `EditPipeline`, `TileEditPipeline`, `GpuPyramidSource`, `OpStack` are in scope (they are `pub` from the crate root).

Run: `cargo test -p ferrolite-pipeline --test prewarm` — Expected: PASS on a GPU box (skips headless).

- [ ] **Step 3: Call it at startup**

In `ferrolite-app/src/app.rs`, right after `ferrolite_pipeline::prewarm_shaders(&gpu);` (~line 77), add — using an `Arc<GpuContext>` (wrap `gpu` or build a fresh `Arc`; match how `gpu` is typed there):
```rust
            ferrolite_pipeline::prewarm_pipelines(std::sync::Arc::new(
                ferrolite_gpu::GpuContext::from_render_state(rs),
            ));
```
> If `gpu` at that site is already an `Arc<GpuContext>`, pass `gpu.clone()` instead of building a new one. Confirm `rs`/`gpu` are in scope at line 77.

- [ ] **Step 4: Verify**

Run: `cargo build --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test -p ferrolite-pipeline` — all clean/pass.
> Optional sanity (if you can run the app): first open should no longer show the ~2.4 s cold spike in `[open-profile]` (cold ≈ warm now).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/src/lib.rs ferrolite-app/src/app.rs ferrolite-pipeline/tests/prewarm.rs
git commit -m "perf(pipeline): prewarm edit pipeline objects at startup (kill first-open compile spike)"
```

---

### Task 2: Render the rung-1 reveal at viewport resolution (fix C)

Cut the ~674 ms full-res reveal by rendering it from a one-shot viewport-res downsample of the image instead of the full-res buffer. The full-res `image` is untouched (still feeds the pyramid job).

**Files:**
- Modify: `ferrolite-app/src/app.rs` (`apply_full_decoded`, the reveal block ~846–896)

**Interfaces:**
- Consumes: `ferrolite_vt::PyramidTileSource`-style box-downsample — reuse the crate's public downsample if one is exported; otherwise downsample via `ferrolite_vt::PyramidTileSource::new(image.clone())` and take its base-appropriate level. Prefer a direct single downsample (see Step 2).

- [ ] **Step 1: Determine the reveal target size**

In `apply_full_decoded`, before the reveal `EditPipeline::new`, compute a viewport-fit target size from the full image dims. Use the viewer's viewport when known, else a cap:
```rust
        // Reveal is a transient rung-1 stopgap the sparse full VT replaces within a
        // moment; at fit-zoom a viewport-res reveal is pixel-identical to full-res.
        // Render it small to avoid allocating ~11 full-res intermediate textures.
        const REVEAL_MAX_DIM: u32 = 2048; // ≈ ≤ a 4K-ish viewport; cap when unknown
        let (fw, fh) = (image.width, image.height);
        let vp = self
            .state
            .viewer
            .as_ref()
            .map(|v| v.viewport)
            .filter(|(w, h)| *w > 0.0 && *h > 0.0);
        let target_long = vp
            .map(|(w, h)| w.max(h).ceil() as u32)
            .unwrap_or(REVEAL_MAX_DIM)
            .clamp(256, REVEAL_MAX_DIM);
        let scale = (target_long as f32 / fw.max(fh) as f32).min(1.0);
        let (rw, rh) = (((fw as f32 * scale) as u32).max(1), ((fh as f32 * scale) as u32).max(1));
```
> Verify `v.viewport` is `(f32, f32)` (physical px) — it is used elsewhere in this function (`v.viewport.0 > 0.0`). If the field name differs, match it.

- [ ] **Step 2: Downsample once and build the reveal on the small source**

Replace the reveal source. Currently `raw_preview_source = is_raw.then(|| Arc::new(image.clone()))` and the reveal `EditPipeline::new(ctx, src, …)` uses that full-res `src`. Change the reveal to use a downsampled buffer when `(rw, rh)` is smaller than the image (keep the full-res `image.clone()` Arc for the pyramid job, which is added in Task 3):
```rust
        let reveal_src: std::sync::Arc<ferrolite_image::LinearRgbaF32> = if scale < 1.0 {
            std::sync::Arc::new(ferrolite_vt::box_downsample_to(image, rw, rh))
        } else {
            std::sync::Arc::new(image.clone())
        };
```
Use `reveal_src` as the `EditPipeline::new(..., &reveal_src, ...)` source and as `v.raw_preview_source`.
> `ferrolite_vt::box_downsample_to` may not be public. If `ferrolite-vt` does not expose a downsample-to-size fn, add a small pure `pub fn box_downsample_to(src: &LinearRgbaF32, w: u32, h: u32) -> LinearRgbaF32` in `ferrolite-vt/src/source.rs` wrapping the existing private `box_downsample` (it already exists there, used by `PyramidTileSource`), export it from `ferrolite-vt/src/lib.rs`, and unit-test it (output dims == (w,h); a flat image stays flat). If adding it, do that as this task's first sub-step (TDD: dims test RED → impl → GREEN).

- [ ] **Step 3: Verify**

Run: `cargo build --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace` (+ the `box_downsample_to` test if added) — clean/pass.
> Optional (if you can run the app): the `[open-profile] reveal …` line should drop from ~674 ms to a small fraction.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/app.rs ferrolite-vt/src/source.rs ferrolite-vt/src/lib.rs
git commit -m "perf(app): render rung-1 reveal at viewport resolution (cut full-res reveal cost)"
```

---

### Task 3: Build both pyramids off the UI thread (fix B)

The atomic threading change: add the `PyramidReady` event, submit the off-thread pyramid job from `apply_full_decoded` (installing a preview-only holder), and install the sparse full VT + producer in a new `apply_pyramid_ready`. Event + producer + consumer land together (an unused variant / half-moved VT install would break the build or leave images stuck at preview).

**Files:**
- Modify: `ferrolite-app/src/events.rs` (`PyramidReady` variant + fold arm)
- Modify: `ferrolite-pipeline/src/gpu_pyramid.rs`, `ferrolite-pipeline/src/image.rs` (derive `Debug`)
- Modify: `ferrolite-app/src/app.rs` (`apply_full_decoded` submit + preview-only holder; new `apply_pyramid_ready`; update-loop arm)

**Interfaces:**
- Produces: `AppEvent::PyramidReady { image_id: i64, tile_source: std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync>, gpu_pyramid: std::sync::Arc<ferrolite_pipeline::GpuPyramidSource> }`; `fn apply_pyramid_ready(&mut self, frame: &eframe::Frame, image_id: i64, tile_source: &Arc<dyn ferrolite_vt::TileSource + Send + Sync>, gpu_pyramid: &Arc<ferrolite_pipeline::GpuPyramidSource>)`.
- Consumes: `self.state.{jobs, tx}`, `ferrolite_jobs::Priority::Background`, `viewer::{ViewerGpu, ViewerPipelines, EditTileProducer}`, `ferrolite_vt::VirtualTexture::sparse`, `ferrolite_pipeline::TileEditPipeline`, `crate::develop::vignette_mode::vig_pair`, `VIEWER_TILE_BUDGET`.

- [ ] **Step 1: Derive `Debug` where the event requires it**

`AppEvent` derives `#[derive(Debug)]` (events.rs:7), so `Arc<GpuPyramidSource>` in the variant needs `GpuPyramidSource: Debug`. Ensure `#[derive(Debug)]` on `GpuPyramidSource` (gpu_pyramid.rs) and `PipelineImage` (image.rs) — its fields (`Arc<wgpu::Texture>`, `u32`) are all `Debug`. (`Arc<dyn TileSource + Send + Sync>` is `Debug` only if the trait has `Debug` as a supertrait or the trait object impls it — CHECK: if `dyn TileSource` is not `Debug`, either add `Debug` as a supertrait of `TileSource` in `ferrolite-vt` (all impls already derivable) OR remove `#[derive(Debug)]` from `AppEvent` and add a manual `impl Debug` that skips the pyramid/source fields. Prefer the manual `impl Debug` on `AppEvent` if touching the `TileSource` trait is broad — decide based on what compiles with the smallest blast radius, and report which you chose.)

Run: `cargo build -p ferrolite-pipeline` — clean.

- [ ] **Step 2: Add the `PyramidReady` variant + fold arm**

In `ferrolite-app/src/events.rs`, add next to `FullDecoded`:
```rust
    /// Both full-res pyramids finished building off-thread (tier-2 open path):
    /// the sparse-VT CPU tile source and the GPU-resident edit pyramid. Installed
    /// on the UI thread (needs render state + the `Rc`-based tile pipeline).
    PyramidReady {
        image_id: i64,
        tile_source: std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync>,
        gpu_pyramid: std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>,
    },
```
And the fold arm near `AppEvent::FullDecoded { .. } => None,`:
```rust
            // Handled in `app.rs` (needs GPU state); nothing to fold here.
            AppEvent::PyramidReady { .. } => None,
```
Do not build in isolation yet (unused variant → `dead_code`); Steps 3–4 add producer + consumer.

- [ ] **Step 3: `apply_full_decoded` — preview-only holder + submit the job**

In `apply_full_decoded`:
- **Move OUT** (delete here; they move to `apply_pyramid_ready` in Step 4): the `PyramidTileSource::new` + `let source` (~898–899), the `VirtualTexture::sparse(...)` full-VT construction (~912–918), and the whole pyramid+producer+`set_producing` block (~1006–1048).
- **Keep**: the reveal (Task 2 form), the `preview_vt` build, and the holder install — but install the holder with **`full: None`** (only the reveal preview). Confirm the `ViewerGpu` holder + the crossfade/`full_ready` logic tolerate `full: None` at install (the state machine already models "preview shown, full not ready"; if `ViewerGpu.full` is `Option`, set `None`; adjust `full_ready`/`loaded` so the reveal shows and the viewer is not marked full-ready until Step 4).
- **Add**, after the holder install (viewer borrow released), the Background job — reuse the full-res `raw_preview_source`/`image` clone Arc for the source build, and pass an `Arc<GpuContext>`:
```rust
        if full_installed {
            let image_full: std::sync::Arc<ferrolite_image::LinearRgbaF32> =
                std::sync::Arc::new(image.clone());
            let gpu_job = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
            let tx = self.state.tx.clone();
            let repaint = ctx.clone();
            self.state.jobs.submit(ferrolite_jobs::Priority::Background, move |cancel| {
                if cancel.is_cancelled() {
                    return;
                }
                // Both pyramids are CPU box-downsample heavy (~1.2s total) — off the
                // UI thread (CLAUDE.md rule 1).
                let tile_source: std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync> =
                    std::sync::Arc::new(ferrolite_vt::PyramidTileSource::new((*image_full).clone()));
                if cancel.is_cancelled() {
                    return;
                }
                let gpu_pyramid = std::sync::Arc::new(
                    ferrolite_pipeline::GpuPyramidSource::new(&gpu_job, &image_full),
                );
                if cancel.is_cancelled() {
                    return;
                }
                let _ = tx.send(crate::events::AppEvent::PyramidReady {
                    image_id,
                    tile_source,
                    gpu_pyramid,
                });
                repaint.request_repaint();
            });
        }
```
> If reusing the already-existing `raw_preview_source` Arc for `image_full` avoids the extra clone, prefer that; but note Task 2 may have made `raw_preview_source` the *downsampled* reveal source — the pyramid needs the FULL-res image, so clone `image` (the full buffer) here regardless.

- [ ] **Step 4: `apply_pyramid_ready` + update-loop arm**

Add the handler after `apply_full_decoded` — it is the sparse-VT + producer + `set_producing` logic removed in Step 3, now fed by the delivered `tile_source`/`gpu_pyramid`, with a staleness guard:
```rust
    fn apply_pyramid_ready(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        tile_source: &std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync>,
        gpu_pyramid: &std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>,
    ) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        // Stale guard: user navigated to another image while this built.
        if self.state.viewer.as_ref().map(|v| v.image_id) != Some(image_id) {
            return;
        }
        let cam = self.camera_to_working(self.current_wb_temp());
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);

        // Build the sparse full VT from the off-thread tile source (needs the
        // pre-warmed ViewerPipelines; read lock released before the write install).
        let full = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            self.apply_display_tail(&gpu, vp);
            ferrolite_vt::VirtualTexture::sparse(
                &gpu,
                std::sync::Arc::clone(tile_source),
                std::sync::Arc::clone(&self.state.jobs),
                VIEWER_TILE_BUDGET,
                &vp.pipelines,
            )
        };

        // Build the edit producer from the off-thread GPU pyramid + install both.
        let version;
        {
            let Some(v) = self.state.viewer.as_mut() else {
                return;
            };
            if v.image_id != image_id {
                return;
            }
            v.pyramid = Some(std::sync::Arc::clone(gpu_pyramid));
            let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
            let (vig_amount, vig_manual) = crate::develop::vignette_mode::vig_pair(
                v.op_stack.lens_correction().as_ref(),
                v.lens_vignette.is_some(),
            );
            let tep = ferrolite_pipeline::TileEditPipeline::new(
                ctx_arc,
                std::sync::Arc::clone(gpu_pyramid),
                v.op_stack.clone(),
                cam,
                v.lens_warp.as_ref(),
                v.lens_vignette.as_ref(),
            );
            let mut producer = viewer::EditTileProducer::new(tep);
            producer.set_vig_amount(vig_amount);
            producer.set_vig_manual(vig_manual);
            v.edit_producer = Some(producer);
            v.full_ready = true;
            version = v.opstack_version.max(1);
        }

        // Install the full VT into the existing holder + start producing.
        let mut renderer = rs.renderer.write();
        if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
            if g.image_id == image_id {
                g.full = Some(full);
                if let Some(full) = g.full.as_mut() {
                    full.set_producing(true);
                    full.set_opstack_version(&g.ctx, version);
                }
            }
        }
    }
```
> This mirrors the removed code; match the exact `VirtualTexture::sparse`, `TileEditPipeline::new`, `EditTileProducer`, and `ViewerGpu.full` field usages from what `apply_full_decoded` did before Step 3 (argument order, `full_ready`/`loaded` handling). If `ViewerGpu.full` is not an `Option`, adjust the holder to make it one (initialized `None` in Step 3) — confirm against `viewer/callback.rs`.

Add the update-loop arm (next to `FullDecoded`, app.rs ~2745):
```rust
                crate::events::AppEvent::PyramidReady {
                    image_id,
                    tile_source,
                    gpu_pyramid,
                } => {
                    self.apply_pyramid_ready(frame, *image_id, tile_source, gpu_pyramid);
                    self.state.dirty = true;
                    continue;
                }
```

- [ ] **Step 5: Verify**

Run: `cargo build --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test -p ferrolite-app --lib` and `cargo test -p ferrolite-pipeline` — clean/pass. Watch for a now-unused `gpu`/import in `apply_full_decoded` (the inline pyramid build is gone) and remove it.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/events.rs ferrolite-app/src/app.rs ferrolite-pipeline/src/gpu_pyramid.rs ferrolite-pipeline/src/image.rs
git commit -m "perf(app): build both full-res pyramids off the UI thread; install VT+producer on PyramidReady"
```

---

### Task 4: Remove instrumentation, green gate, self-review

**Files:** `ferrolite-app/src/app.rs` (remove the 5 `[open-profile]` timing blocks from commit `9ca300e`); verification.

- [ ] **Step 1: Remove the temporary profiling instrumentation.** Delete the `let __prof* = std::time::Instant::now();` lines and their `eprintln!("[open-profile] …")` blocks added for profiling. Grep to confirm none remain: `git grep "open-profile"` → no matches.
- [ ] **Step 2: Format** — `cargo fmt --all` then `cargo fmt --all --check` (no diff).
- [ ] **Step 3: Clippy** — `cargo clippy --workspace --all-targets -- -D warnings` (clean).
- [ ] **Step 4: Tests** — `cargo test --workspace`. Expected PASS.
  > Known pre-existing/environmental failures NOT from this branch (do not chase): the `ferrolite-app state::tests::cancel_pending_jobs_drains_thumb_handles` timing flake (passes isolated), and `ferrolite-decode` tests failing on local uncommitted `.ARW` fixtures. Re-run any failure in isolation to confirm it is one of these.
- [ ] **Step 5: Self-review vs the spec.** Confirm: (A) `prewarm_pipelines` called once at startup; (B) both pyramids build in the Background job, the VT+producer install in `apply_pyramid_ready` with the `image_id` staleness guard, the holder installs `full: None` in `apply_full_decoded` and stays not-producing until ready; (C) the reveal renders from the viewport-res downsample while the full-res `image` still feeds the pyramid job. No `[open-profile]` left.
- [ ] **Step 6: Commit**
```bash
git add -A
git commit -m "chore(fast-open): remove profiling instrumentation; workspace gate green"
```

---

## Visual test plan (hand to the author after the gate is green — per CLAUDE.md)

Run `cargo run --release -p ferrolite-app` and open a large RAW.

1. **No freeze — first open.** Immediately after the image appears the UI is interactive (pan/zoom/slider responds at once); no ~2.4 s first-open stall (fix A). *Fail:* first open still hangs seconds.
2. **No freeze — subsequent opens.** Open several images; each stays interactive on open, no ~1.8 s stall (fixes B+C). *Fail:* recurring per-open freeze.
3. **Immediate color-correct image.** On open the image shows color-correct right away (the reveal), not a raw/greenish flash (fix C keeps the reveal, just smaller). *Fail:* wrong-color flash or blank.
4. **Full-res resolves.** A moment after open the image sharpens to full resolution (the off-thread pyramids land, VT produces). Zoom in — it's crisp. *Fail:* stays soft / never sharpens.
5. **Rapid navigation (staleness).** Open A then immediately B before A finishes. B ends up at full-res; A's late pyramid must not install onto B (no A flash, B not stuck at preview). *Fail:* A's full-res appears over B, or B stuck soft.
6. **Editing after open.** Once full-res is up, adjust exposure / a grade wheel / a curve — the full-res view updates (producer built in `PyramidReady` drives it). *Fail:* edits don't reach full-res tiles.
7. **Quality at fit-zoom.** At fit-to-window the reveal looks the same as before this change (viewport-res == full-res on screen at fit). *Fail:* visibly soft at fit even after full-res should have landed.

**Fixtures:** a large multi-MP RAW makes the before/after obvious.
