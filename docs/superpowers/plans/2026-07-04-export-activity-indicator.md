# Export Activity Indicator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a persistent "an export is running" indicator (current filename, count + progress bar, failed count, cancel ✕) in the global bottom status bar, visible in every view, covering both single-file and batch export.

**Architecture:** Replace the batch-only `BatchExportState` with one unified `ExportActivity` (in `export/activity.rs`) that both export flows populate; the status bar renders a segment from it while `!is_done()`. Single and batch feed it via events (`ExportItemStarted`, existing `ExportProgress`/`ExportFinished`/`BatchItemFinished`). Cancel ✕ calls `cancel_all()` on the stored job handle(s).

**Tech Stack:** Rust, egui (status bar), `ferrolite-jobs` (`JobHandle`/`CancelToken`), existing app event channel.

## Global Constraints

- **`ferrolite-app` only.** No engine/GPU/decode/catalog/`ferrolite-export` crate changes. No new dependencies.
- **Threading (CLAUDE.md):** all export work stays on `ferrolite-jobs`; the indicator is cheap UI-thread state reads only. Never block the UI thread.
- **Unified state:** one `ExportActivity` is the single source of truth; single export = `total: 1`.
- **Progress fraction:** `(completed + tile_done/tile_total) / total`, clamped `0.0..=1.0` (`0` when `tile_total == 0` or `total == 0`).
- **Completion:** the segment is gated on `!is_done()` (it vanishes when done); the activity is NOT nulled on completion, so the Export module's existing "Done — N exported, M failed" summary is preserved. A new export replaces the activity.
- **Batch-only gates:** the Export module's queue-lock / "running" checks apply to **batch** only (`kind == ExportKind::Batch`), so a single export doesn't lock the queue.
- **Filename:** the output file basename (e.g. `sunset.avif`), truncated to 24 chars with an ellipsis (multi-byte-safe).
- **Failed count:** shown only when `failed > 0`.
- Rust edition 2021, rustfmt 100-col, clippy `-D warnings`. Gate: `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace`, then hold for the author's visual test.

---

## File Structure

- **Create** `ferrolite-app/src/export/activity.rs` — `ExportKind`, `ExportActivity` (state + `new_single`/`new_batch`/`is_done`/`cancel_all`/`fraction`/`start_item`/`set_tiles`/`item_finished`) + unit tests.
- **Modify** `ferrolite-app/src/export/mod.rs` — `pub mod activity; pub use activity::{ExportActivity, ExportKind};`; `spawn_export` returns `JobHandle`.
- **Modify** `ferrolite-app/src/export/batch.rs` — remove `BatchExportState` (moved to `activity.rs`); `run_one` gains a progress callback; `spawn_batch`'s process closure sends `ExportItemStarted` + throttled `ExportProgress`.
- **Modify** `ferrolite-app/src/state.rs` — `batch: Option<BatchExportState>` → `export_activity: Option<ExportActivity>` (field + both `Self { .. }` inits); add `AppState::batch_running()`.
- **Modify** `ferrolite-app/src/events.rs` — add `AppEvent::ExportItemStarted { name: String }`; fold `BatchItemFinished` into `export_activity`; update the fold test.
- **Modify** `ferrolite-app/src/app.rs` — `start_batch` (create `ExportActivity::new_batch(items.len())`, fix count); `do_export` (create `new_single`, store handle); event handlers for `ExportItemStarted`/`ExportProgress`/`ExportFinished`; cancel button.
- **Modify** `ferrolite-app/src/export_module/{bottom_bar,mod,queue_list}.rs` — batch-running checks via `state.batch_running()` / `kind == Batch`.
- **Modify** `ferrolite-app/src/status_bar.rs` — `export_status_text` + `truncate_name` pure fns (+ tests) and render the segment + cancel ✕.

---

## Task 1: Unified `ExportActivity` model (replace `BatchExportState`)

Introduce the shared type and migrate every `state.batch`/`BatchExportState` site to it, preserving today's batch behavior and fixing the item-count (it must be the number of images, not the now-single job-handle count).

**Files:**
- Create: `ferrolite-app/src/export/activity.rs`
- Modify: `ferrolite-app/src/export/mod.rs`, `ferrolite-app/src/export/batch.rs`, `ferrolite-app/src/state.rs`, `ferrolite-app/src/events.rs`, `ferrolite-app/src/app.rs`, `ferrolite-app/src/export_module/bottom_bar.rs`, `ferrolite-app/src/export_module/mod.rs`, `ferrolite-app/src/export_module/queue_list.rs`

**Interfaces:**
- Produces: `ExportKind::{Single, Batch}`; `ExportActivity { kind, total, completed, failed, current_name: Option<String>, tile_done: u32, tile_total: u32, handles: Vec<JobHandle>, warnings: Vec<String> }`; `ExportActivity::{new_single(Option<String>), new_batch(usize), is_done()->bool, cancel_all(), fraction()->f32, start_item(Option<String>), set_tiles(u32,u32), item_finished(bool,String)}`; `AppState::batch_running()->bool`; `AppState.export_activity: Option<ExportActivity>`.

- [ ] **Step 1: Write the failing model tests**

Create `ferrolite-app/src/export/activity.rs` with ONLY a test module first:

```rust
//! Unified export activity: one source of truth for the status-bar indicator,
//! populated by both the single-file (`export/mod.rs`) and batch (`export/batch.rs`)
//! flows. Replaces the batch-only `BatchExportState`.

use ferrolite_jobs::JobHandle;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_batch_and_single_defaults() {
        let b = ExportActivity::new_batch(8);
        assert_eq!(b.kind, ExportKind::Batch);
        assert_eq!(b.total, 8);
        assert_eq!(b.completed, 0);
        assert!(!b.is_done());
        let s = ExportActivity::new_single(Some("hero.avif".into()));
        assert_eq!(s.kind, ExportKind::Single);
        assert_eq!(s.total, 1);
        assert_eq!(s.current_name.as_deref(), Some("hero.avif"));
    }

    #[test]
    fn item_finished_folds_completed_failed_and_warnings() {
        let mut a = ExportActivity::new_batch(2);
        a.item_finished(true, "ok".into());
        a.item_finished(false, "disk full".into());
        assert_eq!(a.completed, 2);
        assert_eq!(a.failed, 1);
        assert!(a.is_done());
        assert_eq!(a.warnings, vec!["disk full".to_string()]);
    }

    #[test]
    fn fraction_blends_completed_with_current_tiles() {
        let mut a = ExportActivity::new_batch(4);
        assert_eq!(a.fraction(), 0.0);
        a.completed = 2;
        a.set_tiles(1, 2); // half of the in-flight image
        // (2 + 0.5) / 4 = 0.625
        assert!((a.fraction() - 0.625).abs() < 1e-6);
        a.completed = 4;
        a.set_tiles(0, 0);
        assert_eq!(a.fraction(), 1.0, "clamped to 1.0");
    }

    #[test]
    fn fraction_zero_total_is_zero_not_nan() {
        let a = ExportActivity::new_batch(0);
        assert_eq!(a.fraction(), 0.0);
    }

    #[test]
    fn start_item_sets_name_and_resets_tiles() {
        let mut a = ExportActivity::new_batch(3);
        a.set_tiles(5, 10);
        a.start_item(Some("next.avif".into()));
        assert_eq!(a.current_name.as_deref(), Some("next.avif"));
        assert_eq!(a.tile_done, 0);
        assert_eq!(a.tile_total, 0);
    }
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p ferrolite-app --lib export::activity`
Expected: FAIL to compile (`ExportActivity`/`ExportKind` not defined).

- [ ] **Step 3: Implement the model**

In `ferrolite-app/src/export/activity.rs`, above the test module, add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Single,
    Batch,
}

/// Live state of the in-progress export (single or batch). While `!is_done()`
/// the status bar shows an indicator built from it; the Export module reads it
/// (batch only) for its queue-lock and aggregate summary.
pub struct ExportActivity {
    pub kind: ExportKind,
    /// Images to export (1 for single).
    pub total: usize,
    /// Images finished (ok or failed).
    pub completed: usize,
    pub failed: usize,
    /// Output filename of the in-flight image (already basename + truncatable).
    pub current_name: Option<String>,
    /// Per-image render progress for the current image.
    pub tile_done: u32,
    pub tile_total: u32,
    /// Cancellation targets: the single export job, or the one batch job.
    pub handles: Vec<JobHandle>,
    /// Per-image failure messages, rolled into the final summary.
    pub warnings: Vec<String>,
}

impl ExportActivity {
    pub fn new_batch(total: usize) -> Self {
        Self {
            kind: ExportKind::Batch,
            total,
            completed: 0,
            failed: 0,
            current_name: None,
            tile_done: 0,
            tile_total: 0,
            handles: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn new_single(name: Option<String>) -> Self {
        Self {
            kind: ExportKind::Single,
            total: 1,
            completed: 0,
            failed: 0,
            current_name: name,
            tile_done: 0,
            tile_total: 0,
            handles: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn is_done(&self) -> bool {
        self.completed >= self.total
    }

    pub fn cancel_all(&self) {
        for h in &self.handles {
            h.cancel();
        }
    }

    /// (completed + current-image tile fraction) / total, clamped to 0..=1.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        let tile_frac = if self.tile_total == 0 {
            0.0
        } else {
            self.tile_done as f32 / self.tile_total as f32
        };
        ((self.completed as f32 + tile_frac) / self.total as f32).clamp(0.0, 1.0)
    }

    /// A new image started: set its name and reset per-image tile progress.
    pub fn start_item(&mut self, name: Option<String>) {
        self.current_name = name;
        self.tile_done = 0;
        self.tile_total = 0;
    }

    pub fn set_tiles(&mut self, done: u32, total: u32) {
        self.tile_done = done;
        self.tile_total = total;
    }

    /// One image finished; folds into the aggregate counts.
    pub fn item_finished(&mut self, ok: bool, message: String) {
        self.completed += 1;
        if !ok {
            self.failed += 1;
            self.warnings.push(message);
        }
    }
}
```

- [ ] **Step 4: Run the model tests to confirm they pass**

Run: `cargo test -p ferrolite-app --lib export::activity`
Expected: PASS (5 tests). The crate won't fully build yet (old `BatchExportState` refs remain); fixed in the next steps.

- [ ] **Step 5: Export the type and remove `BatchExportState`**

In `ferrolite-app/src/export/mod.rs`, add after the existing `pub mod batch;` / `pub mod settings_form;` lines:

```rust
pub mod activity;
```

And add a re-export near the other `use`s at the top level of the module (after the `use settings_form::settings_form;` line):

```rust
pub use activity::{ExportActivity, ExportKind};
```

In `ferrolite-app/src/export/batch.rs`, delete the entire `BatchExportState` struct and its `impl` block (the `#[derive(Default)] pub struct BatchExportState { .. }` and `impl BatchExportState { .. }`). Leave `BatchItem`, `spawn_batch`, `run_batch_sequential`, `run_one`, and the tests. If `JobHandle` becomes unused in `batch.rs` after this, keep it — `spawn_batch` still returns `Vec<JobHandle>`.

- [ ] **Step 6: Migrate `AppState`**

In `ferrolite-app/src/state.rs`, change the field (around line 120):

```rust
    /// Live export activity (single or batch); `None` when no export has run this
    /// session. Drives the status-bar indicator and the Export module's batch UI.
    pub export_activity: Option<crate::export::ExportActivity>,
```

Update BOTH `Self { .. }` initializers (the main constructor ~line 245 and the `for_test` one ~line 764): replace `batch: None,` with `export_activity: None,`.

Add this helper method inside `impl AppState` (near the other small helpers):

```rust
    /// True while a BATCH export is running. Single export does not lock the
    /// Export-module queue, so queue gates check this, not merely "an export runs".
    pub fn batch_running(&self) -> bool {
        self.export_activity
            .as_ref()
            .is_some_and(|a| a.kind == crate::export::ExportKind::Batch && !a.is_done())
    }
```

- [ ] **Step 7: Migrate the `BatchItemFinished` fold + its test**

In `ferrolite-app/src/events.rs`, replace the `BatchItemFinished` arm body:

```rust
            AppEvent::BatchItemFinished {
                image_id: _,
                ok,
                message,
            } => {
                if let Some(a) = self.export_activity.as_mut() {
                    a.item_finished(ok, message);
                }
                None
            }
```

Update the fold test `batch_item_finished_folds_into_aggregate`:

```rust
    #[test]
    fn batch_item_finished_folds_into_aggregate() {
        let mut s = AppState::for_test();
        s.export_activity = Some(crate::export::ExportActivity::new_batch(2));
        s.apply(AppEvent::BatchItemFinished {
            image_id: 1,
            ok: true,
            message: "ok".into(),
        });
        s.apply(AppEvent::BatchItemFinished {
            image_id: 2,
            ok: false,
            message: "disk full".into(),
        });
        let a = s.export_activity.as_ref().unwrap();
        assert_eq!(a.completed, 2);
        assert_eq!(a.failed, 1);
        assert!(a.is_done());
        assert_eq!(a.warnings, vec!["disk full".to_string()]);
    }
```

- [ ] **Step 8: Migrate `app.rs` (start_batch count fix + cancel button)**

In `ferrolite-app/src/app.rs` `start_batch`, replace the tail (the `let handles = spawn_batch(..); let total = handles.len(); let mut bs = BatchExportState::new(total); bs.handles = handles; self.state.batch = Some(bs);` block) with:

```rust
        // Item count is the number of images (NOT the job-handle count — the
        // batch is a single sequential job, so it returns one handle).
        let total = items.len();
        let handles =
            crate::export::batch::spawn_batch(&self.state, ctx, gpu, items, working_space, options);
        let mut activity = crate::export::ExportActivity::new_batch(total);
        activity.handles = handles;
        self.state.export_activity = Some(activity);
```

In the Export-module cancel handler (around line 2694), replace `self.state.batch.as_ref()` with `self.state.export_activity.as_ref()`:

```rust
                            crate::export_module::ExportModuleAction::Cancel => {
                                if let Some(a) = self.state.export_activity.as_ref() {
                                    a.cancel_all();
                                }
                            }
```

- [ ] **Step 9: Migrate the Export-module readers**

In `ferrolite-app/src/export_module/mod.rs` (~line 27) and `ferrolite-app/src/export_module/queue_list.rs` (~line 51), replace:

```rust
        let running = state.batch.as_ref().is_some_and(|b| !b.is_done());
```

with:

```rust
        let running = state.batch_running();
```

In `ferrolite-app/src/export_module/bottom_bar.rs`, replace the `let running = state.batch.as_ref()...` line (~line 70) with `let running = state.batch_running();`, and change the summary block (~line 83) from `if let Some(b) = state.batch.as_ref() {` to only show for a batch:

```rust
        if let Some(b) = state
            .export_activity
            .as_ref()
            .filter(|a| a.kind == crate::export::ExportKind::Batch)
        {
```

(The body using `b.is_done()`, `b.completed`, `b.total`, `b.failed` is unchanged.)

- [ ] **Step 10: Build, lint, and run the whole workspace**

Run: `cargo test --workspace`
Expected: PASS (new activity tests + updated fold test; everything compiles).
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 11: Commit**

```bash
git add ferrolite-app/src/export/activity.rs ferrolite-app/src/export/mod.rs ferrolite-app/src/export/batch.rs ferrolite-app/src/state.rs ferrolite-app/src/events.rs ferrolite-app/src/app.rs ferrolite-app/src/export_module/
git commit -m "refactor(app): unify export state into ExportActivity (fixes batch item count)"
```

---

## Task 2: Populate `ExportActivity` from both export flows

Wire single and batch export so `current_name` and per-image tile progress reach the activity, and single export stores its cancel handle.

**Files:**
- Modify: `ferrolite-app/src/events.rs` (new `ExportItemStarted` variant)
- Modify: `ferrolite-app/src/export/mod.rs` (`spawn_export` returns `JobHandle`)
- Modify: `ferrolite-app/src/export/batch.rs` (`run_one` progress callback; `spawn_batch` emits started + progress)
- Modify: `ferrolite-app/src/app.rs` (`do_export` builds `new_single` + stores handle; event handlers)

**Interfaces:**
- Consumes: `ExportActivity` (Task 1), `AppState.export_activity`.
- Produces: `AppEvent::ExportItemStarted { name: String }`; `spawn_export(..) -> ferrolite_jobs::JobHandle`; `run_one(gpu, item, working_space, options, cancel, progress: &mut dyn FnMut(u32, u32)) -> (bool, String)`.

- [ ] **Step 1: Add the `ExportItemStarted` event**

In `ferrolite-app/src/events.rs`, add a variant to `AppEvent` (next to `ExportProgress`):

```rust
    /// A batch export started a new image (carries the output file basename for
    /// the status-bar indicator's "current file"). Single export sets its name at
    /// spawn, so it does not emit this.
    ExportItemStarted { name: String },
```

Add a fold arm in `apply` (with the other export arms that return `None`):

```rust
            AppEvent::ExportItemStarted { .. } => None, // handled in app.rs (sets current_name)
```

- [ ] **Step 2: Make `spawn_export` return its `JobHandle`**

In `ferrolite-app/src/export/mod.rs`, change `spawn_export`'s signature to return the handle and return it. Change the header:

```rust
pub fn spawn_export(
    state: &AppState,
    egui_ctx: &egui::Context,
    gpu: Arc<GpuContext>,
    source: ExportSource,
    stack: ferrolite_pipeline::OpStack,
    camera_to_working: [[f32; 3]; 3],
    working_space: WorkingSpace,
    options: ExportOptions,
    source_path: PathBuf,
    dest: PathBuf,
    image_id: i64,
) -> ferrolite_jobs::JobHandle {
```

Change the `state.jobs.submit(Priority::Background, move |cancel| { .. });` statement to bind and return it: `let handle = state.jobs.submit(Priority::Background, move |cancel| { .. }); handle` (i.e. remove the trailing `;` semantics by assigning to `handle` and adding `handle` as the final expression of the function).

- [ ] **Step 3: Give `run_one` a progress callback (batch)**

In `ferrolite-app/src/export/batch.rs`, change `run_one`'s signature and its `run_export` call:

```rust
fn run_one(
    gpu: &Arc<GpuContext>,
    item: &BatchItem,
    working_space: WorkingSpace,
    options: &ExportOptions,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u32, u32),
) -> (bool, String) {
```

Delete the `let mut noop = |_done: u32, _total: u32| {};` line and change `run_export(req, cancel, &mut noop)` to `run_export(req, cancel, progress)`.

- [ ] **Step 4: Emit started + throttled progress in `spawn_batch`**

In `ferrolite-app/src/export/batch.rs`, replace the `process` closure passed to `run_batch_sequential` (the `|item| { .. run_one(..) .. }` block) with one that announces the item and forwards throttled tile progress:

```rust
            |item| {
                // Announce the file now being written (output basename) for the
                // status-bar indicator.
                let name = item
                    .dest
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let _ = tx.send(AppEvent::ExportItemStarted { name });
                egui_ctx.request_repaint();

                crate::diag::export_item_begin();
                let t0 = std::time::Instant::now();
                let image_id = item.image_id;
                let mut last = 0u32;
                let mut progress = |done: u32, total: u32| {
                    // Throttle repaints like the single-file path (every 8 tiles
                    // + on completion) so progress advances without flooding.
                    let _ = tx.send(AppEvent::ExportProgress {
                        image_id,
                        done,
                        total,
                    });
                    if done == total || done.saturating_sub(last) >= 8 {
                        last = done;
                        egui_ctx.request_repaint();
                    }
                };
                let (ok, message) =
                    run_one(&gpu, item, working_space, &options, cancel, &mut progress);
                crate::diag::export_item_end(ok, t0.elapsed().as_millis() as u64);
                (ok, message)
            },
```

(`tx` and `egui_ctx` are already cloned into the job closure in `spawn_batch`; `AppEvent` is already imported.)

- [ ] **Step 5: Build single-export activity + wire event handlers (app.rs)**

In `ferrolite-app/src/app.rs` `do_export`, replace the `spawn_export(..);` call + the `self.state.warning = Some("Exporting…".to_string());` line with:

```rust
        let current_name = dest
            .file_name()
            .map(|s| s.to_string_lossy().to_string());
        let handle = crate::export::spawn_export(
            &self.state,
            ctx,
            gpu,
            source,
            stack,
            camera_to_working,
            working_space,
            options,
            source_path,
            dest,
            image_id,
        );
        let mut activity = crate::export::ExportActivity::new_single(current_name);
        activity.handles = vec![handle];
        self.state.export_activity = Some(activity);
```

(`dest` is used by `spawn_export` after this snippet reads `dest.file_name()`; compute `current_name` BEFORE the `spawn_export(.. dest ..)` call since `dest` is moved into it — the snippet above already orders it correctly.)

Replace the `ExportProgress` handler body (currently sets `warning`) with a tile update:

```rust
                crate::events::AppEvent::ExportProgress {
                    image_id: _,
                    done,
                    total,
                } => {
                    if let Some(a) = self.state.export_activity.as_mut() {
                        a.set_tiles(*done, *total);
                    }
                    ctx.request_repaint();
                    continue;
                }
```

Replace the `ExportFinished` handler body so a single export folds into the activity (batch completion is driven by `BatchItemFinished`) and still flashes the summary text:

```rust
                crate::events::AppEvent::ExportFinished {
                    image_id: _,
                    ok,
                    message,
                } => {
                    if let Some(a) = self.state.export_activity.as_mut() {
                        if a.kind == crate::export::ExportKind::Single {
                            a.item_finished(*ok, message.clone());
                        }
                    }
                    self.state.warning = Some(message.clone());
                    ctx.request_repaint();
                    continue;
                }
```

Add an `ExportItemStarted` handler (next to the other export arms):

```rust
                crate::events::AppEvent::ExportItemStarted { name } => {
                    if let Some(a) = self.state.export_activity.as_mut() {
                        a.start_item(Some(name.clone()));
                    }
                    ctx.request_repaint();
                    continue;
                }
```

- [ ] **Step 6: Build, lint, test**

Run: `cargo test --workspace`
Expected: PASS.
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/events.rs ferrolite-app/src/export/mod.rs ferrolite-app/src/export/batch.rs ferrolite-app/src/app.rs
git commit -m "feat(app): feed ExportActivity from single + batch flows (name, tiles, cancel handle)"
```

---

## Task 3: Render the status-bar indicator + cancel ✕

Add the pure text formatter and the status-bar segment (filename, count, progress bar, failed count, cancel ✕), shown in every view while an export is running.

**Files:**
- Modify: `ferrolite-app/src/status_bar.rs`

**Interfaces:**
- Consumes: `ExportActivity` (Task 1), `AppState.export_activity`.
- Produces: `export_status_text(&ExportActivity) -> String`, `truncate_name(&str, usize) -> String`.

- [ ] **Step 1: Write the failing formatter tests**

In `ferrolite-app/src/status_bar.rs` `mod tests`, add:

```rust
    #[test]
    fn truncate_name_keeps_short_and_ellipsizes_long() {
        assert_eq!(truncate_name("sunset.avif", 24), "sunset.avif");
        let long = "a_very_long_filename_that_overflows.avif";
        let t = truncate_name(long, 24);
        assert_eq!(t.chars().count(), 24);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn truncate_name_is_multibyte_safe() {
        // 30 accented chars — must not panic on a char boundary and must cap at 24.
        let s: String = "é".repeat(30);
        let t = truncate_name(&s, 24);
        assert_eq!(t.chars().count(), 24);
    }

    #[test]
    fn export_status_text_single_shows_filename_only() {
        let a = crate::export::ExportActivity::new_single(Some("hero.avif".into()));
        assert_eq!(export_status_text(&a), "Exporting hero.avif");
    }

    #[test]
    fn export_status_text_batch_shows_name_and_count() {
        let mut a = crate::export::ExportActivity::new_batch(8);
        a.completed = 3;
        a.start_item(Some("sunset.avif".into()));
        assert_eq!(export_status_text(&a), "Exporting sunset.avif  3/8");
    }

    #[test]
    fn export_status_text_appends_failed_only_when_nonzero() {
        let mut a = crate::export::ExportActivity::new_batch(8);
        a.completed = 5;
        a.failed = 1;
        a.start_item(Some("x.avif".into()));
        assert!(export_status_text(&a).ends_with("(1 failed)"));
        let mut b = crate::export::ExportActivity::new_batch(8);
        b.start_item(Some("x.avif".into()));
        assert!(!export_status_text(&b).contains("failed"));
    }

    #[test]
    fn export_status_text_missing_name_uses_placeholder() {
        let a = crate::export::ExportActivity::new_batch(2);
        assert!(export_status_text(&a).starts_with("Exporting …"));
    }
```

- [ ] **Step 2: Run to confirm they fail**

Run: `cargo test -p ferrolite-app --lib status_bar`
Expected: FAIL to compile (`export_status_text`/`truncate_name` not defined).

- [ ] **Step 3: Implement the pure formatters**

In `ferrolite-app/src/status_bar.rs`, add (above `#[cfg(test)]`):

```rust
use crate::export::{ExportActivity, ExportKind};

/// Truncate a filename to at most `max` chars, appending an ellipsis when cut.
/// Char-based (never splits a multi-byte codepoint).
pub fn truncate_name(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        return name.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = name.chars().take(keep).collect();
    out.push('…');
    out
}

/// Label text for the export indicator: filename (+ `completed/total` for a
/// batch), plus `(K failed)` when any failed. Single omits the count (total = 1).
pub fn export_status_text(a: &ExportActivity) -> String {
    let name = a
        .current_name
        .as_deref()
        .map(|n| truncate_name(n, 24))
        .unwrap_or_else(|| "…".to_string());
    let mut s = match a.kind {
        ExportKind::Single => format!("Exporting {name}"),
        ExportKind::Batch => format!("Exporting {name}  {}/{}", a.completed, a.total),
    };
    if a.failed > 0 {
        s.push_str(&format!("  ({} failed)", a.failed));
    }
    s
}
```

- [ ] **Step 4: Run the formatter tests to confirm they pass**

Run: `cargo test -p ferrolite-app --lib status_bar`
Expected: PASS.

- [ ] **Step 5: Render the segment in the status bar**

In `ferrolite-app/src/status_bar.rs` `show`, insert the export segment immediately after `ui.monospace(selected_exif(state));` and before the `ui.with_layout(egui::Layout::right_to_left(..))` call, so it reads left-to-right next to the EXIF text:

```rust
        // Export activity indicator — visible in every view while an export runs
        // (hidden once done; the Export module keeps the "Done" summary).
        if let Some(a) = &state.export_activity {
            if !a.is_done() {
                ui.separator();
                ui.label(egui::RichText::new(export_status_text(a)).size(11.0));
                ui.add(egui::ProgressBar::new(a.fraction()).desired_width(70.0));
                if ui
                    .small_button("✕")
                    .on_hover_text("Cancel export")
                    .clicked()
                {
                    a.cancel_all();
                }
            }
        }
```

- [ ] **Step 6: Build + lint (egui render has no unit test — visual test covers it)**

Run: `cargo build -p ferrolite-app`
Expected: PASS.
Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/status_bar.rs
git commit -m "feat(app): export progress indicator in the global status bar (filename, count, cancel)"
```

---

## Task 4: Workspace gate + hold for the author's visual test

**Files:** none (verification only).

- [ ] **Step 1: Full gate**

Run: `cargo fmt --all -- --check` (expected: clean; if not, `cargo fmt --all` and amend the last commit).
Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings` (expected: no warnings).
Run: `cargo test --workspace` (expected: all green).

- [ ] **Step 2: Present finish options, then HOLD**

Do not merge/PR. Present the results and ask the author to visually test:
- Single export (Develop → Photo → Export): the status bar shows `Exporting <file>` + a moving bar + ✕ in every view; ✕ cancels; it vanishes when done.
- Batch export (Export module, 8 images): the bar advances per image, `current file` + `N/M` update, `(K failed)` appears if any fail, ✕ cancels the batch; the Export module still shows its "Done — …" summary afterward.
- Switch views (Library/Develop/Export) mid-export: the indicator is present in all.

---

## Self-Review

**Spec coverage:**
- §1 indicator in global status bar, every view, both flows → Task 3 (render) + Tasks 1–2 (state/data). ✓
- §2 unified `ExportActivity` replacing `BatchExportState` → Task 1. ✓
- §3 fraction `(completed+tile_frac)/total`; single sets name+handle at spawn; batch `ExportItemStarted` + wired `run_one` progress; cancel via `cancel_all` → Task 1 (`fraction`), Task 2 (all wiring). ✓
- §4 render segment gated on `!is_done()`, active-info style, filename truncated, count/failed/✕; pure `export_status_text`/`truncate_name` tested → Task 3. Completion via `!is_done()` gate (preserves Export-module "Done" summary) is documented in Global Constraints. ✓
- §5 app-only, jobs unchanged → Global Constraints; no engine files touched. ✓
- §6 per-image failure count; cancel; missing name → placeholder (`export_status_text` placeholder test); activity never sticks (segment hidden on `is_done`) → Tasks 1–3. ✓
- §7 pure-fn tests (status text, truncate, fraction, is_done, folds) + visual test → Tasks 1 & 3. ✓

**Placeholder scan:** no TBD/TODO; every code step is complete. The "�placeholder" `…` string is intentional UI copy (missing filename), covered by a test.

**Type consistency:** `ExportActivity`/`ExportKind` field + method names (`new_single`/`new_batch`/`is_done`/`cancel_all`/`fraction`/`start_item`/`set_tiles`/`item_finished`/`current_name`/`tile_done`/`tile_total`/`handles`) are consistent across Tasks 1–3; `AppState.export_activity` + `batch_running()` used uniformly; `AppEvent::ExportItemStarted { name: String }` and `spawn_export(..) -> JobHandle` / `run_one(.., progress: &mut dyn FnMut(u32,u32))` signatures match their call sites. `total = items.len()` in `start_batch` (Task 1 Step 8) fixes the count regression.
