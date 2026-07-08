# Full-Res Pyramid Off the UI Thread Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the ~1s UI freeze on image open by building the full-res GPU pyramid on a `ferrolite-jobs` worker instead of inline on the UI thread.

**Architecture:** On tier-2 `FullDecoded`, reveal the color-correct preview as today but submit a Background job that runs `GpuPyramidSource::new` off-thread; it delivers the finished pyramid via a new `AppEvent::PyramidReady`, whose UI-thread handler installs the pyramid, builds the (`Rc`-based, `!Send`, must-be-UI-thread) `TileEditPipeline` + producer, and switches the full VT to producing. The full VT stays not-producing during the gap so only the already-shown preview is visible.

**Tech Stack:** Rust, `ferrolite-app` (eframe/egui app loop + `ferrolite-jobs`), `ferrolite-pipeline` (`GpuPyramidSource`, `TileEditPipeline`), `ferrolite-gpu` (`GpuContext`), `wgpu`.

## Global Constraints

Copied from the design (`docs/superpowers/specs/2026-07-08-full-res-pyramid-off-thread-design.md`).

- **Never block the UI/update thread (CLAUDE.md rule 1, load-bearing).** The full-res pyramid build (`GpuPyramidSource::new` — CPU box-downsample across mip levels + uploads) MUST run on a `ferrolite-jobs` worker, not in `update()`.
- **`GpuPyramidSource` is `Send`** (levels are `Arc<Texture>`) → build off-thread, deliver over the channel. **`TileEditPipeline` is `!Send`** (uses `Rc`) → build ONLY on the UI thread. Move only the pyramid off-thread.
- **Preserve the color-correctness invariant:** the full VT must pass through the edit producer (camera→working); it must NOT produce raw camera-native tiles. So the VT stays **not-producing** until the producer exists (in `PyramidReady`).
- **Scope:** `ferrolite-app` only, plus a possible `#[derive(Debug)]` addition in `ferrolite-pipeline`. No changes to decode, VT, shaders, `ferrolite-gpu` executor, or the export path. No new dependencies.
- **Cancellation/staleness:** Background priority + cancel token; the `PyramidReady` handler guards on `image_id == current viewer image_id` and drops stale results.
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean; no `unwrap()` outside tests.

**Branch:** `feat/full-res-pyramid-off-thread` off `main` (already created; the design doc is committed on it).

---

### Task 1: Build the pyramid off-thread and install it via `PyramidReady`

Add the `AppEvent::PyramidReady` variant, replace the inline pyramid build in `apply_full_decoded` with a Background job submit (leaving the full VT not-producing), and add the `apply_pyramid_ready` UI-thread handler that installs the pyramid + builds the tile pipeline/producer + switches the VT to producing. This is one atomic change: an unused enum variant would trip `dead_code`, so producer (job) and consumer (handler) land together.

> **On testing:** this task is eframe/GPU threading glue — it needs the live render state and a real decode, and `GpuPyramidSource` can't be constructed without a GPU device, so there is no honest unit test to write (a contrived compile-only "test" that asserts nothing would be a defect, not coverage). The automated gate for this task is therefore: the change compiles, `clippy --workspace --all-targets -D warnings` is clean, and the full existing suite stays green. The behavior is confirmed by the author's visual test at the end. This is a deliberate, justified departure from TDD for untestable integration glue.

**Files:**
- Modify: `ferrolite-app/src/events.rs` (add `PyramidReady` variant + fold arm)
- Modify: `ferrolite-app/src/app.rs` (`apply_full_decoded`: submit job instead of inline build; new `apply_pyramid_ready`; new update-loop arm)
- Modify (only if needed): `ferrolite-pipeline/src/gpu_pyramid.rs` and `ferrolite-pipeline/src/image.rs` (derive `Debug` so `AppEvent`'s `#[derive(Debug)]` holds)

**Interfaces:**
- Consumes: `ferrolite_pipeline::{GpuPyramidSource, TileEditPipeline, lens_uniform}`, `ferrolite_gpu::GpuContext`, `viewer::EditTileProducer`, `crate::develop::vignette_mode::vig_pair`, `self.state.{jobs, tx}`, `ferrolite_jobs::Priority`.
- Produces: `AppEvent::PyramidReady { image_id: i64, pyramid: std::sync::Arc<ferrolite_pipeline::GpuPyramidSource> }`; `fn apply_pyramid_ready(&mut self, frame: &eframe::Frame, image_id: i64, pyramid: &std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>)`.

- [ ] **Step 1: Ensure `GpuPyramidSource` (and its element type) derive `Debug`**

`AppEvent` derives `#[derive(Debug)]` (events.rs:7), so the new variant's `Arc<GpuPyramidSource>` requires `GpuPyramidSource: Debug`. Check both types and add `Debug` to the derive list if absent.

In `ferrolite-pipeline/src/gpu_pyramid.rs`, the `GpuPyramidSource` struct — ensure its derive includes `Debug`:
```rust
#[derive(Debug)]
pub struct GpuPyramidSource {
    levels: Vec<PipelineImage>,
}
```
In `ferrolite-pipeline/src/image.rs`, ensure `PipelineImage` derives `Debug` (its fields — `Arc<wgpu::Texture>` + `u32`s — are all `Debug`):
```rust
#[derive(Clone, Debug)]
pub struct PipelineImage { /* fields unchanged */ }
```
Run `cargo build -p ferrolite-pipeline` — expected: clean. (If either type already derives `Debug`, leave it; do not duplicate the derive.)

- [ ] **Step 2: Add the `PyramidReady` variant + fold arm**

In `ferrolite-app/src/events.rs`, add the variant to `enum AppEvent` (next to `FullDecoded`):
```rust
    /// A full-res GPU pyramid finished building off-thread (tier-2 open path).
    /// Carries the ready pyramid for install on the UI thread. Handled directly
    /// in `app.rs` (needs the GPU render state + the `Rc`-based tile pipeline),
    /// not folded by `apply`.
    PyramidReady {
        image_id: i64,
        pyramid: std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>,
    },
```
And add the fold arm next to the other GPU-handled events (near `AppEvent::FullDecoded { .. } => None,`):
```rust
            // Handled in `app.rs` (needs GPU state); nothing to fold here.
            AppEvent::PyramidReady { .. } => None,
```
The variant is now unused until Steps 3–4 add its producer and consumer — do NOT build/clippy in isolation here (an unused variant + a possibly non-exhaustive `match` over `AppEvent` will error under `-D warnings`). Build once at Step 5 after the producer and consumer both exist.

- [ ] **Step 3: Replace the inline pyramid build in `apply_full_decoded` with a job submit**

In `ferrolite-app/src/app.rs`, inside `apply_full_decoded`, locate the reveal block (the `if full_installed { if let Some(v) = self.state.viewer.as_mut() { if v.image_id == image_id { … } } }` around lines 971–1050). It currently ends with the pyramid build, producer build, and the `set_producing(true)` block.

**Remove** from inside that block everything from the `// Build the GPU-resident pyramid UNCONDITIONALLY …` comment through the closing of the `renderer.write()` / `set_producing(true)` / `set_opstack_version` block (the current lines ~1006–1048). **Keep** the reveal lines above it (`v.loaded = true; v.full_ready = true; v.image_dims = …; v.view = ViewTransform::fit(…)`).

**Add**, AFTER that `if full_installed { … }` block closes (so the `&mut self.state.viewer` borrow is released), a Background job submit that builds the pyramid off-thread. Reuse the already-cloned full-res buffer `raw_preview_source` (an `Option<Arc<LinearRgbaF32>>` built at app.rs ~846 — `is_raw.then(|| Arc::new(image.clone()))`) so no second full-res copy happens:
```rust
        // Build the full-res GPU pyramid OFF the UI thread (CLAUDE.md rule 1):
        // GpuPyramidSource::new CPU box-downsamples the full-res image across every
        // mip level — hundreds of ms on a multi-MP RAW. Reuse the already-cloned
        // `raw_preview_source` Arc (no second full-res memcpy); the completed
        // pyramid arrives via AppEvent::PyramidReady and is installed on the UI
        // thread (the Rc-based TileEditPipeline must be built there). The full VT
        // stays not-producing until then, so only the color-correct preview shows.
        if full_installed {
            let image_for_pyramid = raw_preview_source
                .clone()
                .unwrap_or_else(|| std::sync::Arc::new(image.clone()));
            let gpu_for_pyramid = std::sync::Arc::new(
                ferrolite_gpu::GpuContext::from_render_state(rs),
            );
            let tx = self.state.tx.clone();
            let repaint_ctx = ctx.clone();
            self.state.jobs.submit(ferrolite_jobs::Priority::Background, move |cancel| {
                if cancel.is_cancelled() {
                    return;
                }
                let pyramid = std::sync::Arc::new(
                    ferrolite_pipeline::GpuPyramidSource::new(&gpu_for_pyramid, &image_for_pyramid),
                );
                if cancel.is_cancelled() {
                    return;
                }
                let _ = tx.send(crate::events::AppEvent::PyramidReady { image_id, pyramid });
                repaint_ctx.request_repaint();
            });
        }
```
> Confirm `self.state.tx` is the `AppEvent` sender used by other jobs (it is — see `preview_cache::spawn_cache_write(&self.state.jobs, …, &self.state.tx, ctx, …)` at app.rs ~1103) and that `ferrolite_jobs::Priority::Background` is the correct import/path (grep an existing `Priority::Background` use). If `raw_preview_source` was moved earlier in the function, capture the needed `Arc` before it is consumed. `&gpu_for_pyramid` / `&image_for_pyramid` deref-coerce `&Arc<T>` → `&T` for the `new(&GpuContext, &LinearRgbaF32)` signature.

- [ ] **Step 4: Add `apply_pyramid_ready` + the update-loop arm**

In `ferrolite-app/src/app.rs`, add the handler (place it right after `apply_full_decoded`). It is the ~15 lines removed in Step 4, plus a staleness guard and reading the pyramid from the argument:
```rust
    /// Install a full-res GPU pyramid that finished building off-thread, then
    /// build the (UI-thread-only, `Rc`-based) full-res tile pipeline + producer and
    /// switch the sparse full VT to producing. Stale results (the user navigated
    /// away) are dropped by the `image_id` guard.
    fn apply_pyramid_ready(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        pyramid: &std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>,
    ) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        // camera→working borrows the viewer immutably; compute before the &mut borrow.
        let cam = self.camera_to_working(self.current_wb_temp());
        {
            let Some(v) = self.state.viewer.as_mut() else {
                return; // viewer closed
            };
            if v.image_id != image_id {
                return; // stale: a different image is open now
            }
            v.pyramid = Some(std::sync::Arc::clone(pyramid));
            let ctx_arc =
                std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
            let (vig_amount, vig_manual) = crate::develop::vignette_mode::vig_pair(
                v.op_stack.lens_correction().as_ref(),
                v.lens_vignette.is_some(),
            );
            let tep = ferrolite_pipeline::TileEditPipeline::new(
                ctx_arc,
                std::sync::Arc::clone(pyramid),
                v.op_stack.clone(),
                cam,
                v.lens_warp.as_ref(),
                v.lens_vignette.as_ref(),
            );
            let mut producer = viewer::EditTileProducer::new(tep);
            producer.set_vig_amount(vig_amount);
            producer.set_vig_manual(vig_manual);
            v.edit_producer = Some(producer);
        }
        // Now switch the sparse full VT to producing (borrow of `viewer` released).
        let version = self
            .state
            .viewer
            .as_ref()
            .map(|v| v.opstack_version.max(1))
            .unwrap_or(1);
        let mut renderer = rs.renderer.write();
        if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
            if g.image_id == image_id {
                if let Some(full) = g.full.as_mut() {
                    full.set_producing(true);
                    full.set_opstack_version(&g.ctx, version);
                }
            }
        }
    }
```
> Match the exact `TileEditPipeline::new`/`EditTileProducer` calls that were in `apply_full_decoded` before Step 4 (same argument order, same `vig_pair` usage). If the original passed additional/renamed args, mirror them verbatim.

Then add the update-loop arm in the event `match` (next to the `FullDecoded` arm at app.rs ~2745):
```rust
                crate::events::AppEvent::PyramidReady { image_id, pyramid } => {
                    self.apply_pyramid_ready(frame, *image_id, pyramid);
                    self.state.dirty = true;
                    continue;
                }
```

- [ ] **Step 5: Verify build, clippy, and the full existing suite**

Run: `cargo build --workspace` — expected: clean (the `match` over `AppEvent` is now exhaustive; no unused variant).
Run: `cargo clippy --workspace --all-targets -- -D warnings` — expected: clean.
Run: `cargo test -p ferrolite-app --lib` and `cargo test -p ferrolite-pipeline` — expected: PASS (no behavior change to folded events or pipeline; the pyramid Debug derive is additive).
Run: `cargo fmt --all` then `cargo fmt --all --check` — expected: no diff.

> Do not attempt to unit-test the threading/GPU path — it needs the live eframe render state and a real decode. The real confirmation is the visual test below.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/events.rs ferrolite-app/src/app.rs ferrolite-pipeline/src/gpu_pyramid.rs ferrolite-pipeline/src/image.rs
git commit -m "perf(app): build full-res pyramid off the UI thread (fix ~1s open freeze)"
```

---

### Task 2: Workspace green gate + self-review

Final verification against the CLAUDE.md gate.

**Files:** none (verification) — plus any small fixups the gate surfaces.

- [ ] **Step 1: Format** — `cargo fmt --all --check` (no diff).
- [ ] **Step 2: Clippy** — `cargo clippy --workspace --all-targets -- -D warnings` (clean; watch for an unused import like `Priority` or a now-unused `gpu` binding in `apply_full_decoded` after the inline build was removed — remove if flagged).
- [ ] **Step 3: Tests** — `cargo test --workspace`. Expected PASS.
  > Known pre-existing/environmental failures NOT caused by this branch (do not chase): a timing flake in `ferrolite-app state::tests::cancel_pending_jobs_drains_thumb_handles` (passes in isolation), and `ferrolite-decode` tests that fail when local uncommitted `.ARW` files sit in `fixtures/raw/`. Re-run any failing test in isolation to confirm it is one of these before treating the gate as green.
- [ ] **Step 4: Self-review against the spec.** Confirm: only the pyramid build moved off-thread; `TileEditPipeline` is still built on the UI thread (in `apply_pyramid_ready`); the full VT is left not-producing in `apply_full_decoded` and switched on only in `apply_pyramid_ready`; the staleness `image_id` guard is present; no second full-res buffer copy was introduced (the job reuses `raw_preview_source`).
- [ ] **Step 5: Commit any gate fixups**
```bash
git add -A
git commit -m "chore(pyramid-off-thread): workspace gate green (fmt/clippy/test)"
```

---

## Visual test plan (hand to the author after the gate is green — per CLAUDE.md)

This changes reachable open-time behavior (full-res load threading) and can only be judged by running the app.

1. **No freeze on open** — open a large RAW. The UI must stay interactive immediately after the preview appears — no ~1s stall. Try panning/zooming or moving a slider right after open; it should respond at once (previously it froze). *Failure:* the UI still hangs for ~1s on open.
2. **Full-res still resolves** — a beat after open, the image should sharpen to full resolution (the sparse full VT starts producing once the pyramid lands). *Failure:* the image stays soft / preview-only indefinitely, or never sharpens.
3. **Color stays correct through the gap** — during the brief pyramid-build window the image must look color-correct (the color-managed preview), never a raw/greenish camera-native flash. *Failure:* a wrong-color flash before full-res appears.
4. **Edited-on-open** — open an image that already has edits (curve/grade/exposure). The edited look must show immediately (edited preview) and remain correct when full-res resolves. *Failure:* edits missing at open or a visible shift when full-res lands.
5. **Rapid navigation (staleness)** — open image A then immediately open image B before A's full-res finishes. B must end up showing B at full-res; A's late pyramid must not install onto B (no flicker of A, no wrong-image full-res). *Failure:* A's full-res briefly appears over B, or B never reaches full-res.
6. **Editing after open** — once full-res is up, make an edit (e.g. exposure, or a grade wheel) and confirm the full-res view updates as before (the producer built in `PyramidReady` drives it). *Failure:* edits don't reach the full-res tiles.

**Fixtures:** a large multi-megapixel RAW makes the before/after freeze difference obvious; a smaller image may not have shown much freeze to begin with.
