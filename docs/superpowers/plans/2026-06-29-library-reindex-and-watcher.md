# Library Reindex, Folder Watcher & Tree-Icon Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the folder-tree icons + window-control alignment, add Soft/Hard folder reindex, and add a periodic + one-time-startup background watcher that auto-ingests new files.

**Architecture:** All four items build on the merged Plan 3.5 Library. Icons become painted shapes (no font dependency). Reindex threads a `ReindexMode {Incremental, Full}` through the existing recursive ingest job (DRY); Full additionally force-redecodes and prunes via a new transactional `Catalog::prune_subtree`. A pure `should_watch` predicate drives a Background incremental scan of the selected subtree every ~10s, and a one-time startup sweep scans every root — both reuse the same ingest job and are gated by an `active_ingests` counter.

**Tech Stack:** Rust 2021, egui/eframe 0.29, rusqlite 0.32 (WAL), rayon, walkdir, `ferrolite-jobs`.

## Global Constraints

- **rustfmt** default, max width **100**; `cargo fmt` before every commit.
- **clippy** clean: final gate `cargo clippy --workspace --all-targets -- -D warnings`.
- **No schema change** — all four items reuse the Plan 3.5 schema. **rusqlite stays pinned at 0.32** (do not bump).
- **`ferrolite-jobs` stays photo-agnostic** — do not touch it. Reindex/watcher/startup submit ordinary jobs (Interactive for user actions, Background for watcher/startup).
- **Catalog is a rebuildable cache** — Hard reindex's prune deletes catalog rows only, **never disk files**.
- **WAL writer + ReadPool** — reads (`folder_path`) via the pool; prune writes via the single `Catalog` writer.
- **Icons are painted, not font glyphs** — the bundled IBM Plex fonts lack the arrow/✕ code points.
- **TDD** for pure/catalog logic (80%+ on non-GPU logic); egui rendering & `request_repaint_after` are UI glue verified by build + manual eyeball. Conventional commits; no attribution footer.

---

## File Structure

- `ferrolite-app/src/library/panel.rs` — MODIFY: painted disclosure triangle + reserved/painted ✕; add Reindex context-menu entries (Task 3).
- `ferrolite-app/src/chrome/mod.rs` — MODIFY: window-control right-edge alignment in `title_bar`.
- `ferrolite-catalog/src/queries.rs` — MODIFY: add `folder_path`.
- `ferrolite-catalog/src/catalog.rs` — MODIFY: add `folder_path`, `prune_subtree`.
- `ferrolite-catalog/src/read_pool.rs` — MODIFY: add `folder_path` passthrough.
- `ferrolite-catalog/tests/tree.rs` — MODIFY: add `prune_subtree` integration test.
- `ferrolite-app/src/ingest.rs` — MODIFY: `ReindexMode`, `mode` param on `ingest_job` (force + prune), `submit_ingest` helper, `spawn_reindex`, `WATCH_INTERVAL`, `should_watch` (+ test), `spawn_watch_scan`, `spawn_startup_rescan`.
- `ferrolite-app/src/state.rs` — MODIFY: `active_ingests`, `last_watch_check`, `startup_rescan_done` fields; `cancel_pending_jobs`; refactor `reset_for_new_folder`.
- `ferrolite-app/src/events.rs` — MODIFY: `IngestDone` decrements `active_ingests`.
- `ferrolite-app/src/app.rs` — MODIFY: `request_repaint_after`, first-frame startup sweep, per-frame watcher tick.

---

## Task 1: UI fixes — painted tree icons + window-control alignment

**Files:**
- Modify: `ferrolite-app/src/library/panel.rs` (icon rendering only; context menu stays as-is this task)
- Modify: `ferrolite-app/src/chrome/mod.rs`

**Interfaces:**
- Consumes: `egui::collapsing_header::paint_default_icon`, `egui::Shape::line_segment`, `theme::{TEXT_DIM, TEXT_PRIMARY}`.
- Produces: no new public API; pure rendering changes.

This task has no unit test (egui rendering glue; the `chrome::window_controls::command` mapping already has unit tests and is unchanged). Verify by build + clippy + eyeball.

- [ ] **Step 1: Replace the disclosure arrow + ✕ with painted shapes in `panel.rs`**

Replace the body of the `for node in nodes` loop (the `ui.horizontal(|ui| { … })` block) in `ferrolite-app/src/library/panel.rs` with:

```rust
    for node in nodes {
        ui.horizontal(|ui| {
            ui.add_space(node.depth as f32 * 14.0);

            // Disclosure triangle — painted (egui's native rotating icon), never a
            // font glyph. Non-expandable rows reserve the same 14px width.
            if node.has_children {
                let open = state.expanded_folders.contains(&node.id);
                let resp = ui.allocate_response(egui::vec2(14.0, 14.0), egui::Sense::click());
                let openness = if open { 1.0 } else { 0.0 };
                egui::collapsing_header::paint_default_icon(ui, openness, &resp);
                if resp.clicked() {
                    if open {
                        state.expanded_folders.remove(&node.id);
                    } else {
                        state.expanded_folders.insert(node.id);
                    }
                }
            } else {
                ui.add_space(14.0);
            }

            let selected = state.current_folder == Some(node.id);
            let label = format!("{}  ({})", node.name, node.rollup_count);
            let resp = ui.selectable_label(selected, label);
            if resp.clicked() {
                state.select_folder(node.id);
            }
            resp.context_menu(|ui| {
                if ui.button("Remove from catalog").clicked() {
                    request_remove(state, &folders, node.id, &node.name);
                    ui.close_menu();
                }
            });

            // Remove ✕ — always reserve a 14px slot (no hover relayout); paint an
            // X (two line segments) only when the row or slot is hovered.
            let x_slot = ui.allocate_response(egui::vec2(14.0, 14.0), egui::Sense::click());
            if resp.hovered() || x_slot.hovered() {
                let r = x_slot.rect.shrink(4.0);
                let color = if x_slot.hovered() {
                    theme::TEXT_PRIMARY
                } else {
                    theme::TEXT_DIM
                };
                let stroke = egui::Stroke::new(1.2, color);
                let p = ui.painter();
                p.line_segment([r.left_top(), r.right_bottom()], stroke);
                p.line_segment([r.left_bottom(), r.right_top()], stroke);
            }
            if x_slot.clicked() {
                request_remove(state, &folders, node.id, &node.name);
            }
        });
    }
```

(The `request_remove` helper below the loop is unchanged.)

- [ ] **Step 2: Align the window controls flush to the right edge in `chrome/mod.rs`**

In `ferrolite-app/src/chrome/mod.rs::title_bar`, the right-group closure currently is:

```rust
            |ui| {
                ui.add_space(8.0);
                let clicked = window_controls::controls_ui(ui, is_maximized);
                ui.add_space(8.0);
                ui.monospace(version);
                clicked
            },
```

Replace it with (drop the leading space; zero the control group's item spacing so the three 44px buttons stay contiguous and the close button is flush with `bar.right()`):

```rust
            |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let clicked = window_controls::controls_ui(ui, is_maximized);
                ui.add_space(8.0);
                ui.monospace(version);
                clicked
            },
```

- [ ] **Step 3: Build, lint, and verify**

Run: `cargo build -p ferrolite-app` then `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: compiles, no warnings.

Then `cargo run -p ferrolite-app` and confirm by eye: disclosure triangles render (rotate on expand/collapse), the ✕ appears on row hover without shifting the row width, and the close button's right edge touches the window's right edge (only the 1px border hairline over it).

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add ferrolite-app/src/library/panel.rs ferrolite-app/src/chrome/mod.rs
git commit -m "fix(app): paint tree disclosure/remove icons; align window controls flush right"
```

---

## Task 2: Catalog `prune_subtree` + `folder_path`; ingest `ReindexMode` (force + prune)

**Files:**
- Modify: `ferrolite-catalog/src/queries.rs`, `src/catalog.rs`, `src/read_pool.rs`
- Modify: `ferrolite-catalog/tests/tree.rs`
- Modify: `ferrolite-app/src/ingest.rs`

**Interfaces:**
- Consumes: existing recursive `parent_id` CTE pattern (see `Catalog::remove_folder`); `scan_tree`/`collect_dirs`.
- Produces:
  - `Catalog::folder_path(&self, folder_id: i64) -> Result<Option<String>, CatalogError>` and the same on `ReadPool`.
  - `Catalog::prune_subtree(&self, root_folder_id: i64, kept_folder_ids: &HashSet<i64>, kept_image_ids: &HashSet<i64>) -> Result<(), CatalogError>`.
  - `ferrolite_app::ingest::ReindexMode { Incremental, Full }` (`pub`, derives `Debug, Clone, Copy, PartialEq, Eq`).
  - `ingest_job(folder, mode, writer, reads, jobs, tx, ctx, cancel)` — gains a `mode: ReindexMode` second parameter.

- [ ] **Step 1: Add the failing `folder_path` test**

Append to `ferrolite-catalog/tests/catalog.rs`:

```rust
#[test]
fn folder_path_round_trips() {
    let cat = ferrolite_catalog::Catalog::open_in_memory().unwrap();
    let id = cat
        .upsert_folder(std::path::Path::new("/photos/a"), None)
        .unwrap();
    assert_eq!(cat.folder_path(id).unwrap().as_deref(), Some("/photos/a"));
    assert_eq!(cat.folder_path(999_999).unwrap(), None);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p ferrolite-catalog --test catalog folder_path_round_trips`
Expected: FAIL (compile error — `folder_path` not defined).

- [ ] **Step 3: Implement `folder_path`**

In `ferrolite-catalog/src/queries.rs`, add (near `image_count`):

```rust
pub(crate) fn folder_path(conn: &Connection, folder_id: i64) -> Result<Option<String>, CatalogError> {
    let p = conn
        .query_row(
            "SELECT path FROM folders WHERE id = ?1",
            rusqlite::params![folder_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(p)
}
```

In `ferrolite-catalog/src/catalog.rs`, add a method on `Catalog` (next to `image_count`):

```rust
    pub fn folder_path(&self, folder_id: i64) -> Result<Option<String>, CatalogError> {
        crate::queries::folder_path(self.conn(), folder_id)
    }
```

In `ferrolite-catalog/src/read_pool.rs`, add (next to `list_folders`):

```rust
    pub fn folder_path(&self, folder_id: i64) -> Result<Option<String>, CatalogError> {
        self.with_conn(|c| crate::queries::folder_path(c, folder_id))
    }
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p ferrolite-catalog --test catalog folder_path_round_trips`
Expected: PASS.

- [ ] **Step 5: Add the failing `prune_subtree` integration test**

Append to `ferrolite-catalog/tests/tree.rs` (it already has `make_png` + `nested_fixture(tag)` helpers and `use ferrolite_catalog::{...}`; add `std::collections::HashSet` if absent):

```rust
#[test]
fn prune_subtree_deletes_absent_files_and_folders() {
    use std::collections::HashSet;
    let root = nested_fixture("prune");
    let db = std::env::temp_dir().join(format!("ferro-prune-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let cat = Catalog::open(&db).unwrap();
    cat.ingest_folder(&root).unwrap();

    let reads = ReadPool::open(&db, 1).unwrap();
    let folders = reads.list_folders().unwrap();
    let root_id = folders.iter().find(|f| f.parent_id.is_none()).unwrap().id;
    let folder_2025 = folders.iter().find(|f| f.path.ends_with("2025")).unwrap().id;
    let all = reads.list_images_recursive(root_id).unwrap();
    assert_eq!(all.len(), 3);

    // Simulate a Full rescan where 2025 (folder + its image) vanished from disk
    // and one 2024 image was deleted: keep everything else.
    let drop_2024_img = all
        .iter()
        .find(|i| i.folder_id != root_id && i.folder_id != folder_2025)
        .map(|i| i.id)
        .unwrap();
    let kept_folders: HashSet<i64> = folders
        .iter()
        .map(|f| f.id)
        .filter(|id| *id != folder_2025)
        .collect();
    let kept_images: HashSet<i64> = all
        .iter()
        .map(|i| i.id)
        .filter(|id| *id != drop_2024_img)
        .filter(|id| {
            // also drop 2025's image (its folder vanished)
            all.iter()
                .find(|i| i.id == *id)
                .map(|i| i.folder_id != folder_2025)
                .unwrap_or(false)
        })
        .collect();

    cat.prune_subtree(root_id, &kept_folders, &kept_images).unwrap();

    let after_folders = reads.list_folders().unwrap();
    assert!(
        after_folders.iter().all(|f| f.id != folder_2025),
        "vanished folder pruned"
    );
    let after_images = reads.list_images_recursive(root_id).unwrap();
    assert!(
        after_images.iter().all(|i| i.id != drop_2024_img),
        "deleted file pruned"
    );
    assert_eq!(after_images.len(), 1, "only the kept top-level image remains");
    assert!(reads.get_thumbnail(drop_2024_img).unwrap().is_none());

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&db);
}
```

- [ ] **Step 6: Run it to verify it fails**

Run: `cargo test -p ferrolite-catalog --test tree prune_subtree`
Expected: FAIL (compile error — `prune_subtree` not defined).

- [ ] **Step 7: Implement `prune_subtree`**

In `ferrolite-catalog/src/catalog.rs`, add `use std::collections::HashSet;` at the top, and add this method on `Catalog` (next to `remove_folder`):

```rust
    /// Prune a folder subtree to mirror disk after a full rescan: delete images
    /// (and their thumbnails) whose id is not in `kept_image_ids`, and folders
    /// (vanished subdirectories) whose id is not in `kept_folder_ids`. The root
    /// is expected to be in `kept_folder_ids` and is never pruned. Cache only —
    /// never touches files on disk. Runs in one transaction.
    pub fn prune_subtree(
        &self,
        root_folder_id: i64,
        kept_folder_ids: &HashSet<i64>,
        kept_image_ids: &HashSet<i64>,
    ) -> Result<(), CatalogError> {
        const SUBTREE_CTE: &str = "WITH RECURSIVE subtree(id) AS (
                 SELECT id FROM folders WHERE id = ?1
                 UNION ALL
                 SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id
             )";
        let tx = self.conn().unchecked_transaction()?;

        let subtree_image_ids: Vec<i64> = {
            let sql = format!(
                "{SUBTREE_CTE} SELECT id FROM images WHERE folder_id IN (SELECT id FROM subtree)"
            );
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params![root_folder_id], |r| r.get::<_, i64>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for img in subtree_image_ids {
            if !kept_image_ids.contains(&img) {
                tx.execute("DELETE FROM thumbnails WHERE image_id = ?1", rusqlite::params![img])?;
                tx.execute("DELETE FROM images WHERE id = ?1", rusqlite::params![img])?;
            }
        }

        let subtree_folder_ids: Vec<i64> = {
            let sql = format!("{SUBTREE_CTE} SELECT id FROM subtree");
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params![root_folder_id], |r| r.get::<_, i64>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for fid in subtree_folder_ids {
            if !kept_folder_ids.contains(&fid) {
                tx.execute("DELETE FROM folders WHERE id = ?1", rusqlite::params![fid])?;
            }
        }

        tx.commit()?;
        Ok(())
    }
```

- [ ] **Step 8: Run it to verify it passes**

Run: `cargo test -p ferrolite-catalog --test tree prune_subtree`
Expected: PASS. Then `cargo test -p ferrolite-catalog` — Expected: all green.

- [ ] **Step 9: Add `ReindexMode` + `mode` param to the ingest job**

In `ferrolite-app/src/ingest.rs`:

Add the enum near the top (after the imports):

```rust
/// How a (re)ingest treats already-indexed files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReindexMode {
    /// Skip files whose (mtime, size) are unchanged (default / soft).
    Incremental,
    /// Force re-decode + re-thumbnail every file, and prune catalog rows for
    /// files/folders no longer on disk (hard / full rebuild).
    Full,
}
```

Add `use std::collections::HashSet;` to the imports (alongside `HashMap`).

Change `spawn_ingest`'s job closure to pass the mode (the only current caller of `ingest_job`):

```rust
    let handle = jobs.submit(Priority::Interactive, move |cancel| {
        ingest_job(
            folder,
            ReindexMode::Incremental,
            writer,
            reads,
            jobs_for_closure,
            tx,
            ctx,
            cancel,
        );
    });
```

Rewrite `ingest_job` to take `mode` and apply force + prune. Replace the whole `fn ingest_job(...) { ... }`:

```rust
#[allow(clippy::too_many_arguments)]
fn ingest_job(
    folder: PathBuf,
    mode: ReindexMode,
    writer: Arc<Mutex<Catalog>>,
    reads: Arc<ReadPool>,
    jobs: Arc<JobSystem>,
    tx: Sender<AppEvent>,
    ctx: egui::Context,
    cancel: &CancelToken,
) {
    let files = scan_tree(&folder);
    let force = matches!(mode, ReindexMode::Full);

    // Create folder rows top-down, wiring parent_id.
    let mut dir_ids: HashMap<PathBuf, i64> = HashMap::new();
    for dir in collect_dirs(&files, &folder) {
        if cancel.is_cancelled() {
            return;
        }
        let parent_id = dir.parent().and_then(|p| dir_ids.get(p).copied());
        match writer.lock().expect("writer").upsert_folder(&dir, parent_id) {
            Ok(id) => {
                dir_ids.insert(dir, id);
            }
            Err(e) => eprintln!("ferrolite: upsert_folder failed: {e}"),
        }
    }
    let root_folder_id = dir_ids.get(&folder).copied();

    // Parallel metadata decode. Incremental skips unchanged files; Full forces all.
    let rows: Vec<(NewImage, PathBuf, FileKind)> = files
        .par_iter()
        .filter(|_| !cancel.is_cancelled())
        .filter_map(|f| {
            let folder_id = *f.path.parent().and_then(|p| dir_ids.get(p))?;
            if !force {
                match reads.needs_reingest(folder_id, &f.filename, f.mtime, f.size) {
                    Ok(true) => {}
                    _ => return None,
                }
            }
            let new_image = match ferrolite_decode::read_metadata(&f.path, f.kind) {
                Ok(meta) => NewImage::from_metadata(
                    folder_id,
                    f.filename.clone(),
                    f.mtime,
                    f.size,
                    &meta,
                    f.kind,
                ),
                Err(_) => NewImage::failed(folder_id, f.filename.clone(), f.mtime, f.size, f.kind),
            };
            Some((new_image, f.path.clone(), f.kind))
        })
        .collect();

    // Serial row upserts under the writer lock; enqueue a thumbnail job per row.
    // For Full, collect every present file's id so prune can delete the rest.
    let mut kept_image_ids: HashSet<i64> = HashSet::new();
    for (new_image, path, kind) in rows {
        if cancel.is_cancelled() {
            break;
        }
        let id = match writer.lock().expect("writer").upsert_image(&new_image) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("ferrolite: upsert_image failed: {e}");
                continue;
            }
        };
        if force {
            kept_image_ids.insert(id);
        }
        let _ = tx.send(AppEvent::Indexed { added: 1 });
        if new_image.decode_status != DecodeStatus::Failed {
            let job_id = spawn_thumbnail(&jobs, &writer, &tx, &ctx, id, path, kind);
            let _ = tx.send(AppEvent::ThumbRegistered {
                image_id: id,
                job_id,
            });
        }
        ctx.request_repaint();
    }

    // Full: prune catalog rows for files/folders no longer on disk. Skip if
    // cancelled (kept set would be incomplete).
    if force && !cancel.is_cancelled() {
        if let Some(root) = root_folder_id {
            let kept_folder_ids: HashSet<i64> = dir_ids.values().copied().collect();
            if let Err(e) = writer
                .lock()
                .expect("writer")
                .prune_subtree(root, &kept_folder_ids, &kept_image_ids)
            {
                eprintln!("ferrolite: prune_subtree failed: {e}");
            }
        }
    }

    let _ = tx.send(AppEvent::IngestDone);
    ctx.request_repaint();
}
```

- [ ] **Step 10: Build the workspace and run the affected suites**

Run: `cargo test -p ferrolite-catalog` then `cargo test -p ferrolite-app`
Expected: PASS (the existing `ingest_tree` e2e still green with the new `Incremental` arg; catalog prune + folder_path green).

- [ ] **Step 11: Commit**

```bash
cargo fmt
git add ferrolite-catalog ferrolite-app/src/ingest.rs
git commit -m "feat(catalog): prune_subtree + folder_path; ingest ReindexMode force+prune"
```

---

## Task 3: Reindex orchestration + context-menu UI

**Files:**
- Modify: `ferrolite-app/src/state.rs`, `src/events.rs`, `src/ingest.rs`, `src/library/panel.rs`

**Interfaces:**
- Consumes: `ReindexMode` (Task 2), `Catalog`/`ReadPool` Arcs in `AppState`.
- Produces:
  - `AppState.active_ingests: usize`; `AppState::cancel_pending_jobs(&mut self)`.
  - `ingest::submit_ingest(state: &mut AppState, ctx: &egui::Context, folder: PathBuf, mode: ReindexMode, priority: Priority) -> JobHandle` (`pub(crate)`; increments `active_ingests`).
  - `ingest::spawn_reindex(state: &mut AppState, ctx: &egui::Context, folder_path: PathBuf, mode: ReindexMode)`.

- [ ] **Step 1: Write the failing `cancel_pending_jobs` test**

Add to the `tests` module in `ferrolite-app/src/state.rs`:

```rust
    #[test]
    fn cancel_pending_jobs_keeps_view_but_drains_jobs() {
        let mut s = AppState::for_test();
        s.current_folder = Some(7);
        s.images = vec![]; // (kept as-is; view not cleared)
        s.selected = Some(3);
        s.indexed = 5;
        s.thumb_jobs.insert(1, ferrolite_jobs::JobId(100));
        s.thumb_jobs.insert(2, ferrolite_jobs::JobId(101));

        s.cancel_pending_jobs();

        assert!(s.thumb_jobs.is_empty(), "thumb jobs drained");
        assert_eq!(s.current_folder, Some(7), "current folder preserved");
        assert_eq!(s.selected, Some(3), "selection preserved");
        assert_eq!(s.indexed, 5, "counters not zeroed by cancel_pending_jobs");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p ferrolite-app --lib cancel_pending_jobs`
Expected: FAIL (compile error — `cancel_pending_jobs` not defined).

- [ ] **Step 3: Add the state fields and refactor reset**

In `ferrolite-app/src/state.rs`:

Add to the `AppState` struct (near `dirty`):

```rust
    /// Number of ingest jobs currently in flight (open/reindex/watcher/startup).
    /// The watcher fires only when this is 0. Incremented on spawn, decremented
    /// on `IngestDone`.
    pub active_ingests: usize,
    /// Wall-clock of the last watcher tick (for the periodic check).
    pub last_watch_check: Option<std::time::Instant>,
    /// One-time startup rescan guard (fires on the first update frame).
    pub startup_rescan_done: bool,
```

Initialize all three in **both** `new()` and `for_test()` (alongside `dirty: true,`):

```rust
            active_ingests: 0,
            last_watch_check: None,
            startup_rescan_done: false,
```

Extract `cancel_pending_jobs` and have `reset_for_new_folder` call it. Replace the existing `reset_for_new_folder` with:

```rust
    /// Cancel any in-flight ingest + pending thumbnail jobs, without touching the
    /// view (images/current_folder/selection) or counters. Used by reindex.
    pub fn cancel_pending_jobs(&mut self) {
        if let Some(h) = self.ingest_handle.take() {
            h.cancel();
        }
        for (_image_id, job_id) in self.thumb_jobs.drain() {
            self.jobs.cancel(job_id);
        }
    }

    /// Reset per-folder job + counter state when switching folders.
    pub fn reset_for_new_folder(&mut self) {
        self.cancel_pending_jobs();
        self.indexed = 0;
        self.thumb_total = 0;
        self.thumb_done = 0;
        self.images.clear();
        self.selected = None;
        self.dirty = true;
    }
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p ferrolite-app --lib state`
Expected: PASS (the new test + the existing `reset_for_new_folder_*` / `select_folder_*` tests still green).

- [ ] **Step 5: Decrement `active_ingests` on `IngestDone`**

In `ferrolite-app/src/events.rs`, change the `IngestDone` arm of `apply`:

```rust
            AppEvent::IngestDone => {
                self.active_ingests = self.active_ingests.saturating_sub(1);
                None
            }
```

- [ ] **Step 6: Add the `submit_ingest` helper and refactor `spawn_ingest`; add `spawn_reindex`**

In `ferrolite-app/src/ingest.rs`, add `JobHandle` to the `ferrolite_jobs` import:

```rust
use ferrolite_jobs::{CancelToken, JobHandle, JobSystem, Priority};
```

Add the shared submit helper (it increments `active_ingests` for every ingest spawn — open/reindex/watcher/startup):

```rust
/// Submit one ingest job for `folder` at `priority` with `mode`, incrementing
/// the in-flight counter. Returns the handle so the caller can store it for
/// cancellation. Does NOT reset the view — callers decide that.
pub(crate) fn submit_ingest(
    state: &mut AppState,
    ctx: &egui::Context,
    folder: PathBuf,
    mode: ReindexMode,
    priority: Priority,
) -> JobHandle {
    state.active_ingests += 1;
    let writer = Arc::clone(&state.writer);
    let reads = Arc::clone(&state.reads);
    let jobs = Arc::clone(&state.jobs);
    let jobs_for_closure = Arc::clone(&jobs);
    let tx = state.tx.clone();
    let ctx = ctx.clone();
    jobs.submit(priority, move |cancel| {
        ingest_job(folder, mode, writer, reads, jobs_for_closure, tx, ctx, cancel);
    })
}
```

Replace `spawn_ingest`'s body (Open-folder path: full reset, upsert root, Interactive incremental) with:

```rust
pub fn spawn_ingest(state: &mut AppState, ctx: &egui::Context, folder: PathBuf) {
    state.reset_for_new_folder();

    let folder_id = match state.writer.lock().expect("writer").upsert_folder(&folder, None) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("ferrolite: upsert_folder failed: {e}");
            return;
        }
    };
    state.current_folder = Some(folder_id);

    let handle = submit_ingest(state, ctx, folder, ReindexMode::Incremental, Priority::Interactive);
    state.ingest_handle = Some(handle);
}
```

Add `spawn_reindex` (user-triggered; updates the view in place — no `reset_for_new_folder`):

```rust
/// Reindex a folder's subtree in place (does not clear the grid like Open Folder).
/// `Full` zeroes the thumbnail-progress counters for a clean status-bar readout.
pub fn spawn_reindex(
    state: &mut AppState,
    ctx: &egui::Context,
    folder_path: PathBuf,
    mode: ReindexMode,
) {
    state.cancel_pending_jobs();
    if matches!(mode, ReindexMode::Full) {
        state.thumb_total = 0;
        state.thumb_done = 0;
    }
    state.dirty = true;
    let handle = submit_ingest(state, ctx, folder_path, mode, Priority::Interactive);
    state.ingest_handle = Some(handle);
}
```

- [ ] **Step 7: Add the Reindex context-menu entries in `panel.rs`**

In `ferrolite-app/src/library/panel.rs`, extend the `resp.context_menu(...)` closure (added in Task 1) to include the two reindex actions above Remove. Replace that closure with:

```rust
            resp.context_menu(|ui| {
                if ui.button("Reindex — new files").clicked() {
                    crate::ingest::spawn_reindex(
                        state,
                        ctx,
                        std::path::PathBuf::from(&node_path),
                        crate::ingest::ReindexMode::Incremental,
                    );
                    ui.close_menu();
                }
                if ui.button("Reindex — full rebuild").clicked() {
                    crate::ingest::spawn_reindex(
                        state,
                        ctx,
                        std::path::PathBuf::from(&node_path),
                        crate::ingest::ReindexMode::Full,
                    );
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Remove from catalog").clicked() {
                    request_remove(state, &folders, node.id, &node.name);
                    ui.close_menu();
                }
            });
```

The context menu needs the folder's path. The `FolderNode` (from `folder_tree`) carries `id`/`name`/`rollup_count`/`depth`/`has_children` but **not** the path, so look it up from the `folders` slice once per row, before the `ui.horizontal` block. Add at the top of the `for node in nodes` loop body (before `ui.horizontal`):

```rust
        let node_path = folders
            .iter()
            .find(|f| f.id == node.id)
            .map(|f| f.path.clone())
            .unwrap_or_default();
```

(`node_path` is moved into the closure via `&node_path`; since the closure may run later, clone into the `PathBuf` at call time as shown. `node_path` is captured by reference in `context_menu`'s closure, which runs within this frame, so a borrow is fine.)

- [ ] **Step 8: Build, lint, and run app tests**

Run: `cargo build -p ferrolite-app` then `cargo clippy -p ferrolite-app --all-targets -- -D warnings` then `cargo test -p ferrolite-app`
Expected: compiles, no warnings, tests pass.

- [ ] **Step 9: Commit**

```bash
cargo fmt
git add ferrolite-app/src/state.rs ferrolite-app/src/events.rs ferrolite-app/src/ingest.rs ferrolite-app/src/library/panel.rs
git commit -m "feat(app): in-place Soft/Hard reindex via context menu; active_ingests counter"
```

---

## Task 4: Periodic watcher + one-time startup rescan

**Files:**
- Modify: `ferrolite-app/src/ingest.rs` (`WATCH_INTERVAL`, `should_watch` + test, `spawn_watch_scan`, `spawn_startup_rescan`)
- Modify: `ferrolite-app/src/app.rs` (repaint scheduling, first-frame startup sweep, per-frame watcher tick)

**Interfaces:**
- Consumes: `submit_ingest`, `ReindexMode::Incremental` (Task 3); `ReadPool::folder_path`/`list_folders` (Task 2); `AppState.{active_ingests,last_watch_check,startup_rescan_done,current_folder}`.
- Produces:
  - `ingest::WATCH_INTERVAL: std::time::Duration`.
  - `ingest::should_watch(now: Instant, last_check: Option<Instant>, interval: Duration, current_folder: Option<i64>, active_ingests: usize) -> bool`.
  - `ingest::spawn_watch_scan(state, ctx)` and `ingest::spawn_startup_rescan(state, ctx)`.

- [ ] **Step 1: Write the failing `should_watch` test**

Add a test module entry in `ferrolite-app/src/ingest.rs` (create a `#[cfg(test)] mod tests { ... }` at the bottom if none exists):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn should_watch_fires_only_when_idle_selected_and_elapsed() {
        let iv = Duration::from_secs(10);
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(11);
        let soon = t0 + Duration::from_secs(3);

        // Happy path: folder selected, no ingest, interval elapsed.
        assert!(should_watch(later, Some(t0), iv, Some(1), 0));
        // First-ever check (no last_check) fires.
        assert!(should_watch(t0, None, iv, Some(1), 0));
        // Not enough time elapsed.
        assert!(!should_watch(soon, Some(t0), iv, Some(1), 0));
        // No folder selected.
        assert!(!should_watch(later, Some(t0), iv, None, 0));
        // An ingest is in flight.
        assert!(!should_watch(later, Some(t0), iv, Some(1), 2));
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p ferrolite-app --lib should_watch`
Expected: FAIL (compile error — `should_watch`/`WATCH_INTERVAL` not defined).

- [ ] **Step 3: Implement `WATCH_INTERVAL` + `should_watch`**

In `ferrolite-app/src/ingest.rs`, add near the top (after `ReindexMode`):

```rust
/// How often the background watcher polls the selected folder for new files.
pub const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Pure predicate: should the periodic watcher fire this frame? True iff a
/// folder is selected, no ingest is in flight, and at least `interval` has
/// elapsed since `last_check` (or there has been no check yet).
pub fn should_watch(
    now: std::time::Instant,
    last_check: Option<std::time::Instant>,
    interval: std::time::Duration,
    current_folder: Option<i64>,
    active_ingests: usize,
) -> bool {
    if current_folder.is_none() || active_ingests != 0 {
        return false;
    }
    match last_check {
        None => true,
        Some(t) => now.duration_since(t) >= interval,
    }
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p ferrolite-app --lib should_watch`
Expected: PASS.

- [ ] **Step 5: Implement the watcher + startup spawn helpers**

In `ferrolite-app/src/ingest.rs`, add:

```rust
/// Spawn a silent Background incremental scan of the currently-selected folder's
/// subtree (picks up newly-added files). No view/counter reset.
pub fn spawn_watch_scan(state: &mut AppState, ctx: &egui::Context) {
    let Some(folder_id) = state.current_folder else {
        return;
    };
    let path = match state.reads.folder_path(folder_id) {
        Ok(Some(p)) => PathBuf::from(p),
        _ => return,
    };
    let handle = submit_ingest(state, ctx, path, ReindexMode::Incremental, Priority::Background);
    state.ingest_handle = Some(handle);
}

/// One-time startup sweep: a Background incremental scan of every root folder
/// (parent_id is NULL) so on-disk changes since last launch appear immediately.
/// A recursive scan per root covers all descendants.
pub fn spawn_startup_rescan(state: &mut AppState, ctx: &egui::Context) {
    let roots: Vec<PathBuf> = state
        .reads
        .list_folders()
        .unwrap_or_default()
        .into_iter()
        .filter(|f| f.parent_id.is_none())
        .map(|f| PathBuf::from(f.path))
        .collect();
    for root in roots {
        // Each increments active_ingests; handles are not individually tracked
        // (cheap, silent, idempotent incremental scans).
        let _ = submit_ingest(state, ctx, root, ReindexMode::Incremental, Priority::Background);
    }
}
```

- [ ] **Step 6: Wire the startup sweep, watcher tick, and repaint scheduling in `app.rs`**

In `ferrolite-app/src/app.rs`, inside `impl eframe::App for FerroliteApp`'s `update`, immediately after the event-drain / dirty-refresh block (after the `if self.state.dirty { … }` block, before the `TopBottomPanel::top("titlebar")`), insert:

```rust
        // One-time startup rescan of all roots (first frame, ctx available here).
        if !self.state.startup_rescan_done {
            crate::ingest::spawn_startup_rescan(&mut self.state, ctx);
            self.state.startup_rescan_done = true;
        }

        // Periodic background watcher for new files in the selected subtree.
        let now = std::time::Instant::now();
        if crate::ingest::should_watch(
            now,
            self.state.last_watch_check,
            crate::ingest::WATCH_INTERVAL,
            self.state.current_folder,
            self.state.active_ingests,
        ) {
            self.state.last_watch_check = Some(now);
            crate::ingest::spawn_watch_scan(&mut self.state, ctx);
        }
        // Wake on the watcher cadence even when otherwise idle.
        ctx.request_repaint_after(crate::ingest::WATCH_INTERVAL);
```

- [ ] **Step 7: Build, lint, and run the full workspace suite**

Run: `cargo build -p ferrolite-app` then `cargo clippy --workspace --all-targets -- -D warnings` then `cargo test --workspace`
Expected: compiles, no warnings, all tests pass.

- [ ] **Step 8: Manual verification (real app)**

Run: `cargo run -p ferrolite-app`. Verify by eye:
- On launch, previously-opened folders' counts update if files changed while closed (status bar shows brief thumbnail activity).
- With a folder selected, drop a new image into it (or a subfolder); within ~10s it appears in the grid without any click.
- Right-click a folder → "Reindex — new files" picks up additions; "Reindex — full rebuild" regenerates thumbnails and, after deleting a file/subfolder on disk, removes it from the tree/grid. Disk files are untouched by Remove/Reindex.

- [ ] **Step 9: Commit**

```bash
cargo fmt
git add ferrolite-app/src/ingest.rs ferrolite-app/src/app.rs
git commit -m "feat(app): periodic new-file watcher + one-time startup rescan of all roots"
```

---

## Self-Review (completed during planning)

**Spec coverage:**
- Item 1 tree icons → Task 1 Step 1 (paint disclosure + reserved/painted ✕). ✓
- Item 4 window-control alignment → Task 1 Step 2 (drop leading `add_space`, zero `item_spacing.x`). ✓
- Item 2 Soft/Hard reindex → Task 2 (`ReindexMode` force + `prune_subtree`) + Task 3 (`spawn_reindex`, in-place, Full zeroes counters, context menu). Prune deletes deleted files AND vanished folders (spec §4.2). ✓
- Item 3 watcher → Task 4 (`should_watch`, `spawn_watch_scan`, `request_repaint_after`, Background). Auto-ingest silent, current-subtree, ~10s. ✓
- Item 3 startup rescan → Task 4 (`spawn_startup_rescan` over `parent_id IS NULL` roots, Background, once via `startup_rescan_done`). ✓
- Contracts: jobs untouched; cache-only prune; WAL writer/ReadPool; no schema change. ✓
- `active_ingests` counter (not bool) gates the watcher across N startup jobs (spec §5.1). ✓

**Placeholder scan:** No TBD/TODO; every code step shows complete code; tests are concrete. ✓

**Type consistency:** `ReindexMode {Incremental, Full}` consistent across ingest_job/submit_ingest/spawn_reindex/spawn_watch_scan/spawn_startup_rescan/panel. `submit_ingest(state, ctx, folder, mode, priority) -> JobHandle` consistent. `prune_subtree(root, &HashSet<i64>, &HashSet<i64>)` consistent between catalog impl, the ingest call site, and the test. `folder_path -> Result<Option<String>>` consistent across queries/Catalog/ReadPool. `should_watch(now, last_check, interval, current_folder, active_ingests)` consistent between def, test, and the `app.rs` call. ✓
