# Edited-Thumbnail Regeneration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Library grid thumbnail reflect Develop edits — auto-regenerate on leaving Develop for an image whose edit stack changed this session, plus an on-demand "Regenerate thumbnail" grid context-menu action for the back-catalog.

**Architecture:** A headless `ferrolite-jobs` **Background** job (the `export/batch.rs` Plan-5 pattern) with a shared `Arc<GpuContext>` renders the **preview-resolution** source through the real `EditPipeline`, reads it back to RGBA8, downscales via the existing `generate_thumbnail`, persists via `ThumbnailStore::put_thumbnail`, and emits `AppEvent::ThumbReady` so the existing bounded per-frame texture-upload path swaps the grid texture. All decode / GPU-render / readback / encode / DB-write happens **inside the job**; only the existing `ThumbReady` upload touches the UI thread.

**Tech Stack:** Rust, egui/eframe 0.29.1, wgpu, `ferrolite-jobs`, `ferrolite-pipeline` (`EditPipeline`), `ferrolite-catalog` (`generate_thumbnail`/`ThumbnailStore`), `ferrolite-decode` (`decode_preview`), `ferrolite-color` (`camera_to_working`).

## Global Constraints

- **Never block the UI/update thread** (CLAUDE.md): all decode/GPU-render/readback/encode/DB-write runs inside a `ferrolite-jobs` Background job with a `CancelToken`; the job calls `egui_ctx.request_repaint()` when done. Only the existing per-frame `ThumbReady` upload (`MAX_THUMB_UPLOADS_PER_FRAME = 16`) touches the UI thread. Grid stays virtualized — do not add O(all-items) work.
- **GPU pipelines**: `EditPipeline` is built inside the job and dropped there (it holds `Rc<Cell<…>>`, so it is *not* `Send` — construct and use it entirely within the worker closure, exactly like `TileEditPipeline` in `run_one`). Do not cache it across jobs.
- **Error handling** (spec §8): decode / render / store failure → `eprintln!` log + keep the existing thumbnail. Never panic, never emit a failure event that removes the thumbnail. One bad image never affects others. Sidecar missing/malformed → treat as identity (`OpStack::default()`).
- **Per-component reset** (CLAUDE.md): not applicable — this feature adds no editable control.
- **Preview-resolution source only**: `decode_preview` for both `FileKind::Raw` and `FileKind::Standard` (no full-res re-decode). The embedded preview is sRGB-primaries, so `camera_to_working` is derived from `ColorProfile::srgb_fallback()` for both kinds (this matches the `batch.rs` Standard branch and keeps the edited thumbnail consistent with the ingest thumbnail's source).
- **No attribution trailers** in commits (`Co-Authored-By` is disabled globally).
- Gate before finishing: `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` — then STOP and hold for the author's hands-on visual test (CLAUDE.md).
- Windows build note: if `cargo test` hits `LNK1104: cannot open …ferrolite_app-<hash>.exe`, re-run with an isolated `CARGO_TARGET_DIR` rather than killing the process.

---

## File Structure

- **Create** `ferrolite-app/src/develop/thumb_regen.rs` — the regen worker: pure helpers (gating predicate, sidecar-stack resolver, sRGB-fallback matrix), the blocking `regenerate_edited_thumbnail_blocking`, and the `spawn_regen_edited_thumbnail` Background job with its `RegenStackSource`. One clear responsibility: "render an image's edited thumbnail off-thread and publish it."
- **Modify** `ferrolite-app/src/develop/mod.rs` — declare `pub mod thumb_regen;`.
- **Modify** `ferrolite-app/src/viewer/mod.rs` — add `edits_dirty: bool` to `ViewerState` + init in `open()`.
- **Modify** `ferrolite-app/src/app.rs` — set `edits_dirty` in `apply_edit` + undo/redo; add `maybe_regen_on_leave` + wire into the four leave transitions; add `drain_thumb_regen_requests` + call it once per `update()`.
- **Modify** `ferrolite-app/src/state.rs` — add `pending_thumb_regen: Vec<i64>` to `AppState` + init at both constructor sites.
- **Modify** `ferrolite-app/src/library/image_context_menu.rs` — pure `regen_target_ids` helper + the "Regenerate thumbnail" menu button.

## Verified current-code facts (line numbers as of the rebased branch head)

- `AppEvent::ThumbReady { image_id: i64, rgba: Vec<u8>, w: u32, h: u32 }` — `ferrolite-app/src/events.rs:16`. Its fold clears `thumb_pending`/`thumb_handles`/`thumb_missing` and returns the pixels for upload; the per-frame loop in `app.rs` uploads via `upload_thumbnail` (which also refreshes `thumb_pixels`). Emitting this event is the *only* thing the job must do to refresh the grid.
- `ferrolite_catalog::generate_thumbnail(preview: &ImageBuffer) -> Result<(Thumbnail, DecodedThumb), CatalogError>` — `DecodedThumb { rgba: Vec<u8>, w: u32, h: u32 }`; downscales to ≤256px, JPEG q85. Re-exported from `ferrolite_catalog`.
- `ferrolite_catalog::ThumbnailStore::put_thumbnail(&self, image_id: i64, thumb: &Thumbnail) -> Result<(), CatalogError>` — implemented for `Catalog`; upsert. Trait must be `use`d to call it.
- `ferrolite_decode::decode_preview(path: &Path, kind: FileKind) -> Result<ImageBuffer, DecodeError>`.
- `crate::viewer::load::preview_to_linear(buf: &ImageBuffer) -> LinearRgbaF32` (pub, same crate).
- `ferrolite_pipeline::EditPipeline::new(ctx: Arc<GpuContext>, source: &LinearRgbaF32, stack: OpStack, camera_to_working: [[f32;3];3]) -> Self`; `pub fn evaluate(&mut self) -> PipelineImage`; `ferrolite_pipeline::blit_to_rgba8(ctx: &GpuContext, img: &PipelineImage) -> Vec<u8>` (RGBA8 sRGB, row-unpadded). `PipelineImage { pub width: u32, pub height: u32, .. }` — read output dims from the evaluated image (a crop changes them).
- `ferrolite_image::ImageBuffer::new(w: u32, h: u32, format: PixelFormat, pixels: Vec<u8>) -> Result<Self, ImageBufferError>`; `PixelFormat::Rgba8`.
- `ferrolite_color::camera_to_working(xyz_to_cam, Xy{x,y}, working_space) -> [[f32;3];3]`; `ferrolite_decode::ColorProfile::srgb_fallback()` has `xyz_to_cam: [[f32;3];3]` + `white_xy: [f32;2]`.
- `ferrolite_pipeline::deserialize(&str) -> Option<OpStack>`; `OpStack::default()` = identity; `ferrolite_catalog::read_ops(&Path) -> Option<String>`; `ferrolite_catalog::sidecar_path(&Path) -> PathBuf`.
- Jobs API: `state.jobs.submit(Priority::Background, move |cancel: &CancelToken| { … })`; `Priority` from `ferrolite_jobs`.
- `AppState`: `jobs: Arc<JobSystem>`, `writer: Arc<Mutex<Catalog>>`, `tx: std::sync::mpsc::Sender<AppEvent>`, `reads: Arc<ReadPool>` (`reads.folder_path(folder_id) -> Result<Option<String>, CatalogError>`), `images: Vec<ImageRecord>`, `selection: HashSet<i64>`, `working_space: WorkingSpace`, `warning: Option<String>`.
- `GpuContext::from_render_state(rs)` builds a `GpuContext` from `frame.wgpu_render_state()` (see `app.rs:441` in `confirm_batch`). `GpuContext::headless() -> Option<GpuContext>` exists for tests (see `ferrolite-pipeline/tests/golden.rs`).
- `ViewerState` (`viewer/mod.rs:75`): `image_id: i64`, `path: PathBuf`, `kind: FileKind`, `op_stack: OpStack`, `color_profile: ColorProfile`, constructed only by `ViewerState::open(image_id, path, kind)` (`viewer/mod.rs:200`).
- `apply_edit` (`app.rs:970`): single funnel for both panel (`app.rs:2162`) and crop (`app.rs:2281`) edits; sets `rec.has_edits` + calls `persist_ops`. Undo/redo at `app.rs:1978`. `OpsLoaded` handler at `app.rs:1546` (must NOT mark dirty).
- Leave transitions: Esc-close `app.rs:1814`; arrow-nav `app.rs:1942` and filmstrip-click `app.rs:1758` both funnel through `open_record` (`app.rs:1333`); title-bar Develop→Library detected via `module_at_frame_start` at `app.rs:2167`.
- Context menu: `image_context_menu::show(ui, state, image_id, single_image)`; scoping = `!single_image && state.selection.contains(&image_id)`; existing "Add to export queue" action sets `state.warning`.

---

## Task 1: Regen worker + Background job

**Files:**
- Create: `ferrolite-app/src/develop/thumb_regen.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (add `pub mod thumb_regen;` after `pub mod split;`)
- Test: unit tests inside `ferrolite-app/src/develop/thumb_regen.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `ferrolite_pipeline::{EditPipeline, blit_to_rgba8, deserialize, OpStack}`, `ferrolite_catalog::{generate_thumbnail, put_thumbnail via ThumbnailStore, DecodedThumb, Catalog, read_ops, sidecar_path}`, `ferrolite_decode::{decode_preview, ColorProfile}`, `ferrolite_color::{camera_to_working, Xy, WorkingSpace}`, `ferrolite_image::{FileKind, ImageBuffer, PixelFormat}`, `ferrolite_gpu::GpuContext`, `ferrolite_jobs::{JobSystem, Priority}`, `crate::viewer::load::preview_to_linear`, `crate::events::AppEvent`.
- Produces (later tasks rely on these exact names/signatures):
  - `pub fn should_regenerate_on_leave(edits_dirty: bool) -> bool`
  - `pub fn resolve_regen_stack(payload: Option<String>) -> ferrolite_pipeline::OpStack`
  - `pub fn srgb_fallback_camera_to_working(working_space: ferrolite_color::WorkingSpace) -> [[f32; 3]; 3]`
  - `pub enum RegenStackSource { InMemory(ferrolite_pipeline::OpStack), Sidecar }`
  - `pub fn regenerate_edited_thumbnail_blocking(writer: &std::sync::Arc<std::sync::Mutex<ferrolite_catalog::Catalog>>, gpu: &std::sync::Arc<ferrolite_gpu::GpuContext>, image_id: i64, path: &std::path::Path, kind: ferrolite_image::FileKind, stack: ferrolite_pipeline::OpStack, camera_to_working: [[f32; 3]; 3]) -> Result<ferrolite_catalog::DecodedThumb, String>`
  - `pub fn spawn_regen_edited_thumbnail(jobs: &std::sync::Arc<ferrolite_jobs::JobSystem>, writer: &std::sync::Arc<std::sync::Mutex<ferrolite_catalog::Catalog>>, tx: &std::sync::mpsc::Sender<crate::events::AppEvent>, egui_ctx: &egui::Context, gpu: std::sync::Arc<ferrolite_gpu::GpuContext>, image_id: i64, path: std::path::PathBuf, kind: ferrolite_image::FileKind, camera_to_working: [[f32; 3]; 3], stack_source: RegenStackSource)`

- [ ] **Step 1: Write the failing tests for the pure helpers**

Create `ferrolite-app/src/develop/thumb_regen.rs` with only the test module first (the functions don't exist yet, so it won't compile — that is the RED state):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{serialize, OpStack};

    #[test]
    fn regenerates_only_when_edits_dirty() {
        assert!(should_regenerate_on_leave(true));
        assert!(!should_regenerate_on_leave(false));
    }

    #[test]
    fn missing_sidecar_resolves_to_identity() {
        assert!(resolve_regen_stack(None).is_identity());
    }

    #[test]
    fn malformed_sidecar_resolves_to_identity() {
        assert!(resolve_regen_stack(Some("not json".to_string())).is_identity());
    }

    #[test]
    fn valid_sidecar_payload_round_trips() {
        let mut stack = OpStack::default();
        stack.ops.push(ferrolite_pipeline::Op::Exposure(
            ferrolite_pipeline::Exposure { ev: 1.0 },
        ));
        let payload = serialize(&stack);
        let resolved = resolve_regen_stack(Some(payload));
        assert!(!resolved.is_identity());
        assert_eq!(resolved, stack);
    }

    #[test]
    fn srgb_fallback_matrix_is_near_identity_for_srgb_working_space() {
        let m = srgb_fallback_camera_to_working(ferrolite_color::WorkingSpace::Srgb);
        // sRGB source → sRGB working space is (numerically) the identity map.
        for r in 0..3 {
            for c in 0..3 {
                let expected = if r == c { 1.0 } else { 0.0 };
                assert!(
                    (m[r][c] - expected).abs() < 1e-3,
                    "m[{r}][{c}] = {} not ~{expected}",
                    m[r][c]
                );
            }
        }
    }
}
```

> Verify the exact `Op::Exposure`/`Exposure { ev }` shape and the `WorkingSpace::Srgb` variant name against `ferrolite-pipeline/src/op.rs` and `ferrolite-color/src/working_space.rs` while writing; adjust the constructor literal in the round-trip test to whatever the real single-field op is if `Exposure` differs. The test only needs *some* non-identity op.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-app --lib develop::thumb_regen`
Expected: FAIL — `cannot find function should_regenerate_on_leave` (and the others). This confirms the test targets the not-yet-written functions.

- [ ] **Step 3: Implement the pure helpers**

Add above the test module in `ferrolite-app/src/develop/thumb_regen.rs`:

```rust
//! Headless edited-thumbnail regeneration: render an image's persisted edit
//! stack at preview resolution through the real `EditPipeline`, downscale to a
//! grid thumbnail, persist it, and publish it via `AppEvent::ThumbReady`.
//!
//! All decode / GPU-render / readback / encode / DB-write runs inside a
//! `ferrolite-jobs` Background job (CLAUDE.md responsiveness rules); only the
//! existing per-frame `ThumbReady` upload touches the UI thread.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ferrolite_catalog::{
    generate_thumbnail, read_ops, sidecar_path, Catalog, DecodedThumb, ThumbnailStore,
};
use ferrolite_color::WorkingSpace;
use ferrolite_decode::{decode_preview, ColorProfile};
use ferrolite_gpu::GpuContext;
use ferrolite_image::{FileKind, ImageBuffer, PixelFormat};
use ferrolite_jobs::{JobSystem, Priority};
use ferrolite_pipeline::{blit_to_rgba8, deserialize, EditPipeline, OpStack};

use crate::events::AppEvent;
use crate::viewer::load::preview_to_linear;

/// Gate for the auto-on-leave trigger: regenerate only when the edit stack
/// changed during this Develop session. Merely viewing never regenerates.
pub fn should_regenerate_on_leave(edits_dirty: bool) -> bool {
    edits_dirty
}

/// Resolve the stack for the on-demand path from a sidecar `frl:ops` payload.
/// Missing (`None`) or malformed (`deserialize` → `None`) → identity.
pub fn resolve_regen_stack(payload: Option<String>) -> OpStack {
    payload.and_then(|p| deserialize(&p)).unwrap_or_default()
}

/// camera→working for a preview-resolution (sRGB-primaries) source under the
/// current working space. The embedded preview is already sRGB for BOTH RAW and
/// Standard, so we use the sRGB-fallback profile (no `normalize_neutral`) — this
/// matches `export/batch.rs`'s Standard branch and the ingest thumbnail source.
pub fn srgb_fallback_camera_to_working(working_space: WorkingSpace) -> [[f32; 3]; 3] {
    let p = ColorProfile::srgb_fallback();
    ferrolite_color::camera_to_working(
        p.xyz_to_cam,
        ferrolite_color::Xy {
            x: p.white_xy[0],
            y: p.white_xy[1],
        },
        working_space,
    )
}
```

- [ ] **Step 4: Run tests to verify the pure helpers pass**

Run: `cargo test -p ferrolite-app --lib develop::thumb_regen`
Expected: PASS (all 5 tests). If the `Op::Exposure` literal was wrong it will fail to compile — fix the literal to the real single-field op.

- [ ] **Step 5: Implement the blocking render+store core**

Append to `thumb_regen.rs` (before the test module):

```rust
/// Blocking regenerate: decode the preview → apply the edit stack via the real
/// `EditPipeline` → read back RGBA8 → downscale to a grid thumbnail → persist.
/// Returns the decoded thumbnail pixels for the `ThumbReady` upload.
///
/// GPU + I/O; MUST run inside a Background job. `EditPipeline` is built and
/// dropped here (it is not `Send`). Any failure returns `Err(String)` so the
/// caller logs and keeps the existing thumbnail — never panics.
pub fn regenerate_edited_thumbnail_blocking(
    writer: &Arc<Mutex<Catalog>>,
    gpu: &Arc<GpuContext>,
    image_id: i64,
    path: &Path,
    kind: FileKind,
    stack: OpStack,
    camera_to_working: [[f32; 3]; 3],
) -> Result<DecodedThumb, String> {
    // 1. Preview-resolution source (both kinds), sRGB → display-linear.
    let preview = decode_preview(path, kind).map_err(|e| format!("decode_preview: {e}"))?;
    let linear = preview_to_linear(&preview);

    // 2. Render the edit stack. Read output dims from the evaluated image — a
    //    crop/geometry op changes them.
    let mut pipeline = EditPipeline::new(Arc::clone(gpu), &linear, stack, camera_to_working);
    let out = pipeline.evaluate();
    let (w, h) = (out.width, out.height);
    let rgba = blit_to_rgba8(gpu, &out); // RGBA8 sRGB, row-unpadded, len w*h*4

    // 3. Downscale + JPEG-encode via the existing thumbnail path.
    let edited = ImageBuffer::new(w, h, PixelFormat::Rgba8, rgba)
        .map_err(|e| format!("edited buffer: {e}"))?;
    let (thumb, decoded) =
        generate_thumbnail(&edited).map_err(|e| format!("generate_thumbnail: {e}"))?;

    // 4. Persist (cache-safe single-writer lock, released immediately).
    {
        let db = writer.lock().expect("writer");
        db.put_thumbnail(image_id, &thumb)
            .map_err(|e| format!("put_thumbnail: {e}"))?;
    }

    Ok(decoded)
}
```

- [ ] **Step 6: Implement the Background spawn + `RegenStackSource`**

Append to `thumb_regen.rs` (before the test module):

```rust
/// Where the job obtains the edit stack.
pub enum RegenStackSource {
    /// From the just-closed viewer (auto-on-leave) — no sidecar read.
    InMemory(OpStack),
    /// Read the `.xmp` sidecar inside the job (on-demand catch-up); missing or
    /// malformed → identity.
    Sidecar,
}

/// Submit a Background regen job. On success it emits `AppEvent::ThumbReady`
/// (the existing grid texture-swap signal); on failure it logs and keeps the
/// existing thumbnail. Always requests a repaint so the UI drains the event.
#[allow(clippy::too_many_arguments)]
pub fn spawn_regen_edited_thumbnail(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    egui_ctx: &egui::Context,
    gpu: Arc<GpuContext>,
    image_id: i64,
    path: PathBuf,
    kind: FileKind,
    camera_to_working: [[f32; 3]; 3],
    stack_source: RegenStackSource,
) {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let egui_ctx = egui_ctx.clone();
    jobs.submit(Priority::Background, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        let stack = match stack_source {
            RegenStackSource::InMemory(s) => s,
            RegenStackSource::Sidecar => resolve_regen_stack(read_ops(&sidecar_path(&path))),
        };
        match regenerate_edited_thumbnail_blocking(
            &writer,
            &gpu,
            image_id,
            &path,
            kind,
            stack,
            camera_to_working,
        ) {
            Ok(decoded) => {
                let _ = tx.send(AppEvent::ThumbReady {
                    image_id,
                    rgba: decoded.rgba,
                    w: decoded.w,
                    h: decoded.h,
                });
            }
            Err(e) => {
                eprintln!("ferrolite: edited-thumbnail regen failed for #{image_id}: {e}");
            }
        }
        egui_ctx.request_repaint();
    });
}
```

- [ ] **Step 7: Register the module**

In `ferrolite-app/src/develop/mod.rs`, add after line `pub mod split;`:

```rust
pub mod thumb_regen;
```

- [ ] **Step 8: Run the full build + clippy + tests**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` then `cargo test -p ferrolite-app --lib develop::thumb_regen`
Expected: clippy clean; the 5 unit tests PASS. (The blocking/spawn fns are GPU+I/O — compile-checked here, exercised by the author's visual test.)

- [ ] **Step 9: Commit**

```bash
git add ferrolite-app/src/develop/thumb_regen.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(app): edited-thumbnail regen worker + Background job

Decode preview -> EditPipeline render -> RGBA8 readback -> generate_thumbnail
-> put_thumbnail -> ThumbReady, all inside a ferrolite-jobs Background job
(the export/batch GpuContext pattern). Preview-resolution source, sRGB-fallback
camera_to_working for both RAW and Standard. Pure gating/stack-source/matrix
helpers unit-tested."
```

---

## Task 2: Auto-trigger on leaving Develop

**Files:**
- Modify: `ferrolite-app/src/viewer/mod.rs` (`ViewerState` struct + `open()`)
- Modify: `ferrolite-app/src/app.rs` (`apply_edit`, undo/redo, new `maybe_regen_on_leave`, four leave sites)
- Test: unit test inside `ferrolite-app/src/viewer/mod.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes (from Task 1): `crate::develop::thumb_regen::{should_regenerate_on_leave, srgb_fallback_camera_to_working, spawn_regen_edited_thumbnail, RegenStackSource}`.
- Produces: `ViewerState.edits_dirty: bool` (init `false`); `App::maybe_regen_on_leave(&mut self, ctx: &egui::Context, frame: &eframe::Frame)`.

- [ ] **Step 1: Write the failing test for the new flag's default**

In `ferrolite-app/src/viewer/mod.rs` `#[cfg(test)] mod tests`, add (mirrors the existing `new_viewer_has_no_raw_preview_source` test):

```rust
#[test]
fn new_viewer_is_not_edits_dirty() {
    let v = ViewerState::open(1, std::path::PathBuf::from("x.jpg"), FileKind::Standard);
    assert!(!v.edits_dirty, "a freshly opened viewer has no session edits");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app --lib viewer::`
Expected: FAIL — `no field edits_dirty on type ViewerState`.

- [ ] **Step 3: Add the `edits_dirty` field + init**

In `ferrolite-app/src/viewer/mod.rs`, add to the `ViewerState` struct (end of the field list, near `prefetch_handles`):

```rust
    /// True once any edit is applied to this image THIS session (including a
    /// reset-to-identity, which is also a change). Drives auto-regeneration of
    /// the Library thumbnail when leaving Develop. Set only in the app's
    /// `apply_edit`/undo/redo paths — NOT in the `OpsLoaded` load path.
    pub edits_dirty: bool,
```

In `ViewerState::open(...)`, add to the returned struct literal (near `prefetch_requested: false`):

```rust
            edits_dirty: false,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app --lib viewer::`
Expected: PASS.

- [ ] **Step 5: Set `edits_dirty = true` in the edit-apply paths**

In `ferrolite-app/src/app.rs`, inside `apply_edit`, after the `commit` guard and the `let Some(v) = self.state.viewer.as_mut() else { return; };` (i.e. right before `v.history.push(kind, stack.clone());`), add:

```rust
        v.edits_dirty = true;
```

In the undo/redo handler (`app.rs:~1978`), after `self.set_preview_and_full(frame, stack.clone());` (the `if let Some(stack) = result {` block), add:

```rust
            if let Some(v) = self.state.viewer.as_mut() {
                v.edits_dirty = true;
            }
```

> Do NOT touch the `OpsLoaded` handler (`app.rs:~1546`) or the before/after toggle — loading and view toggles are not edits.

- [ ] **Step 6: Add the `maybe_regen_on_leave` helper**

In `ferrolite-app/src/app.rs`, add this method to the same `impl` block as `open_record`/`camera_to_working`:

```rust
    /// If the current viewer's edit stack changed this session, spawn a
    /// Background job to regenerate its Library thumbnail from the in-memory
    /// stack, then clear the flag so re-entrant frames do not double-spawn.
    /// Called at every "leave Develop for this image" transition. No-op when
    /// there is no viewer, no session edits, or no GPU render state.
    fn maybe_regen_on_leave(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let (image_id, path, kind, stack) = {
            let Some(v) = self.state.viewer.as_mut() else {
                return;
            };
            if !crate::develop::thumb_regen::should_regenerate_on_leave(v.edits_dirty) {
                return;
            }
            // Clear before spawning so an edge-triggered re-check this frame
            // (e.g. module switch) cannot enqueue a duplicate job.
            v.edits_dirty = false;
            (v.image_id, v.path.clone(), v.kind, v.op_stack.clone())
        };
        let Some(rs) = frame.wgpu_render_state() else {
            // No GPU this frame: keep the existing thumbnail. An on-demand
            // "Regenerate thumbnail" can recover it later.
            return;
        };
        let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let cam = crate::develop::thumb_regen::srgb_fallback_camera_to_working(
            self.state.working_space,
        );
        crate::develop::thumb_regen::spawn_regen_edited_thumbnail(
            &self.state.jobs,
            &self.state.writer,
            &self.state.tx,
            ctx,
            gpu,
            image_id,
            path,
            kind,
            cam,
            crate::develop::thumb_regen::RegenStackSource::InMemory(stack),
        );
    }
```

- [ ] **Step 7: Wire the four leave transitions**

(a) `open_record` (`app.rs:~1333`) — covers arrow-nav + filmstrip-click. Add as the FIRST statement of the function body (before the `if let Some(old) = self.state.viewer.as_ref()`):

```rust
        self.maybe_regen_on_leave(ctx, frame);
```

(b) Esc-close (`app.rs:~1814`) — add immediately before `if let Some(v) = self.state.viewer.take() {`:

```rust
            self.maybe_regen_on_leave(ctx, frame);
```

(c) Title-bar Develop→Library switch (`app.rs:~2167`, the `if !module_at_frame_start.is_library() && self.module.is_library() {` block) — add as the first statement inside that block, before `self.pending_texture_clear = true;`:

```rust
            self.maybe_regen_on_leave(ctx, frame);
```

> Ordering safety: the Esc handler runs before the title-bar check and `take()`s the viewer, so the title-bar check finds `None` and no-ops on Esc — no double spawn. `open_record` (arrow/filmstrip) stays in Develop, so the title-bar check does not fire there. `maybe_regen_on_leave` clears `edits_dirty` on first call, so the title-bar edge-trigger cannot re-fire on subsequent frames.

- [ ] **Step 8: Build + clippy + tests**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` then `cargo test -p ferrolite-app --lib`
Expected: clippy clean; all unit tests PASS (including `new_viewer_is_not_edits_dirty`).

- [ ] **Step 9: Commit**

```bash
git add ferrolite-app/src/viewer/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(app): auto-regenerate thumbnail on leaving Develop when edits changed

Add ViewerState::edits_dirty (set in apply_edit + undo/redo, not on OpsLoaded).
maybe_regen_on_leave spawns the regen job with the in-memory stack at the four
leave transitions (Esc, arrow/filmstrip nav via open_record, title-bar switch),
clearing the flag to avoid double-spawn."
```

---

## Task 3: On-demand "Regenerate thumbnail" context-menu action

**Files:**
- Modify: `ferrolite-app/src/state.rs` (`AppState.pending_thumb_regen` + init at both constructor sites)
- Modify: `ferrolite-app/src/app.rs` (`drain_thumb_regen_requests` + call in `update()`)
- Modify: `ferrolite-app/src/library/image_context_menu.rs` (`regen_target_ids` helper + menu button)
- Test: unit test inside `ferrolite-app/src/library/image_context_menu.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes (from Task 1): `crate::develop::thumb_regen::{srgb_fallback_camera_to_working, spawn_regen_edited_thumbnail, RegenStackSource}`.
- Produces: `AppState.pending_thumb_regen: Vec<i64>`; `image_context_menu::regen_target_ids(single_image: bool, right_clicked: i64, selection: &std::collections::HashSet<i64>) -> Vec<i64>`; `App::drain_thumb_regen_requests(&mut self, ctx: &egui::Context, frame: &eframe::Frame)`.

- [ ] **Step 1: Write the failing test for the selection-scoping helper**

In `ferrolite-app/src/library/image_context_menu.rs`, add a `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn single_image_mode_targets_only_that_image() {
        let sel: HashSet<i64> = [1, 2, 3].into_iter().collect();
        assert_eq!(regen_target_ids(true, 2, &sel), vec![2]);
    }

    #[test]
    fn grid_right_click_inside_selection_targets_whole_selection() {
        let sel: HashSet<i64> = [1, 2, 3].into_iter().collect();
        assert_eq!(regen_target_ids(false, 2, &sel), vec![1, 2, 3]);
    }

    #[test]
    fn grid_right_click_outside_selection_targets_only_that_image() {
        let sel: HashSet<i64> = [1, 2, 3].into_iter().collect();
        assert_eq!(regen_target_ids(false, 9, &sel), vec![9]);
    }

    #[test]
    fn grid_right_click_with_empty_selection_targets_only_that_image() {
        let sel: HashSet<i64> = HashSet::new();
        assert_eq!(regen_target_ids(false, 5, &sel), vec![5]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app --lib image_context_menu`
Expected: FAIL — `cannot find function regen_target_ids`.

- [ ] **Step 3: Implement the pure `regen_target_ids` helper**

At the top of `ferrolite-app/src/library/image_context_menu.rs` (module level, above `show`):

```rust
/// Selection scoping for context-menu actions: if the right-clicked image is
/// part of the current grid multi-selection, act on the whole selection; else
/// act on just that image. `single_image` (loupe/filmstrip) always scopes to
/// the one image. Returns a sorted id list (stable, dedup'd via the set).
pub fn regen_target_ids(
    single_image: bool,
    right_clicked: i64,
    selection: &std::collections::HashSet<i64>,
) -> Vec<i64> {
    if !single_image && selection.contains(&right_clicked) {
        let mut ids: Vec<i64> = selection.iter().copied().collect();
        ids.sort_unstable();
        ids
    } else {
        vec![right_clicked]
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app --lib image_context_menu`
Expected: PASS (all 4 tests).

- [ ] **Step 5: Add the `pending_thumb_regen` queue to `AppState`**

In `ferrolite-app/src/state.rs`, add to the `AppState` struct (near `warning`):

```rust
    /// Image ids queued by the "Regenerate thumbnail" context-menu action,
    /// drained in `update()` where the GPU render state is available.
    pub pending_thumb_regen: Vec<i64>,
```

Add `pending_thumb_regen: Vec::new(),` to BOTH `AppState` construction sites (near the `warning: None,` lines at `state.rs:~248` and `state.rs:~796`).

- [ ] **Step 6: Add the drain method + call it in `update()`**

In `ferrolite-app/src/app.rs`, add to the `App` impl:

```rust
    /// Drain "Regenerate thumbnail" requests queued by the grid context menu.
    /// Runs once per frame where the GPU render state is available; each image
    /// loads its edit stack from its `.xmp` sidecar inside the Background job
    /// (missing/malformed → identity, i.e. a color-managed unedited thumbnail).
    fn drain_thumb_regen_requests(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.state.pending_thumb_regen.is_empty() {
            return;
        }
        let Some(rs) = frame.wgpu_render_state() else {
            // No GPU this frame; keep the requests for a later frame.
            return;
        };
        let ids = std::mem::take(&mut self.state.pending_thumb_regen);
        let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let cam = crate::develop::thumb_regen::srgb_fallback_camera_to_working(
            self.state.working_space,
        );
        for id in ids {
            let Some(rec) = self.state.images.iter().find(|r| r.id == id).cloned() else {
                continue;
            };
            let Ok(Some(folder)) = self.state.reads.folder_path(rec.folder_id) else {
                continue;
            };
            let path = std::path::PathBuf::from(folder).join(&rec.filename);
            crate::develop::thumb_regen::spawn_regen_edited_thumbnail(
                &self.state.jobs,
                &self.state.writer,
                &self.state.tx,
                ctx,
                std::sync::Arc::clone(&gpu),
                id,
                path,
                rec.kind,
                cam,
                crate::develop::thumb_regen::RegenStackSource::Sidecar,
            );
        }
    }
```

Call it once per frame in `update()`, near the per-frame thumbnail upload loop (after the event drain, where `ctx` and `frame` are in scope):

```rust
        self.drain_thumb_regen_requests(ctx, frame);
```

> `frame` in `update()` is `&mut eframe::Frame`; the method takes `&eframe::Frame`, so pass `frame` (auto-reborrow). If a borrow-checker conflict arises with an existing `frame` use on the same line region, hoist this call to its own statement.

- [ ] **Step 7: Add the menu button**

In `ferrolite-app/src/library/image_context_menu.rs` `show(...)`, after the existing "Add to export queue" action (following its `ui.separator()` group), add:

```rust
    if ui.button("Regenerate thumbnail").clicked() {
        let ids = regen_target_ids(single_image, image_id, &state.selection);
        let n = ids.len();
        state.pending_thumb_regen.extend(ids);
        state.warning = Some(if n == 1 {
            "Regenerating thumbnail…".to_string()
        } else {
            format!("Regenerating {n} thumbnails…")
        });
        ui.close_menu();
    }
```

- [ ] **Step 8: Build + clippy + tests**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` then `cargo test -p ferrolite-app --lib`
Expected: clippy clean; all unit tests PASS.

- [ ] **Step 9: Commit**

```bash
git add ferrolite-app/src/state.rs ferrolite-app/src/app.rs ferrolite-app/src/library/image_context_menu.rs
git commit -m "feat(app): on-demand \"Regenerate thumbnail\" grid context-menu action

Selection-scoped (single image or whole multi-selection) enqueue into
AppState::pending_thumb_regen; update() drains it with the GPU render state and
spawns the regen job per image, loading each edit stack from its .xmp sidecar
(missing/malformed -> identity). Pure regen_target_ids scoping unit-tested."
```

---

## Final gate (after all three tasks)

- [ ] Run the full workspace gate from the repo root:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all green. (Windows `LNK1104` → re-run with an isolated `CARGO_TARGET_DIR`.)

- [ ] **STOP.** Per CLAUDE.md, green automated checks are necessary but NOT sufficient. Hand off to the author (Jann) for the hands-on visual test:
  - Edit an image in Develop, leave (Esc / arrow / filmstrip / Library button) → its grid thumbnail updates to the edited result.
  - Reset all edits, leave → thumbnail self-heals to the color-managed unedited result.
  - Right-click a back-catalog edited image (single + multi-selection) → "Regenerate thumbnail" refreshes it.
  - Merely viewing (no edit) → no regeneration.
  - Do NOT merge/PR/finish until the author confirms.

---

## Self-Review

**1. Spec coverage:**
- §2/§3/§5 headless Background job, shared `Arc<GpuContext>`, decode_preview → EditPipeline → readback → generate_thumbnail → put_thumbnail → ThumbReady → **Task 1**. ✓
- §4 `edits_dirty` flag + auto-trigger at Esc / navigate / module switch → **Task 2** (four sites; nav funnels through `open_record`). ✓
- §6 on-demand context-menu action (single + multi-selection, sidecar stack) → **Task 3**. ✓
- §7 reset self-heals (identity stack → unedited thumbnail): covered — reset marks `edits_dirty`, job renders identity; on-demand missing sidecar → identity. ✓
- §8 error handling (log + keep thumbnail, never panic, one bad image isolated, sidecar missing/malformed → identity): `Result<_, String>` + `eprintln!` in the job, `resolve_regen_stack` fallback. ✓
- §9 testing (pure gating predicate + stack-source helper unit-tested; decode→render→store validated by build/clippy + visual test): Task 1 tests `should_regenerate_on_leave` + `resolve_regen_stack` (+ matrix); Task 3 tests `regen_target_ids`. ✓
- Out-of-scope (batch export edits, background sweep, freshness marker/schema, full-res re-decode): none introduced. ✓

**2. Placeholder scan:** No TBD/TODO/"add error handling"/"similar to Task N" — every step has literal code or an exact command. The only conditional note (verify `Op::Exposure` literal, `WorkingSpace::Srgb` variant) is a real name-check with a concrete fallback instruction, not a deferred implementation.

**3. Type consistency:** `spawn_regen_edited_thumbnail`, `RegenStackSource`, `regenerate_edited_thumbnail_blocking`, `should_regenerate_on_leave`, `resolve_regen_stack`, `srgb_fallback_camera_to_working`, `regen_target_ids`, `maybe_regen_on_leave`, `drain_thumb_regen_requests`, `pending_thumb_regen`, `edits_dirty` are named identically across the tasks that define and consume them. `AppEvent::ThumbReady { image_id, rgba, w, h }` matches `events.rs`. `DecodedThumb { rgba, w, h }` matches `thumbnail.rs`.
