# ferrolite — Library Reindex, Folder Watcher & Tree-Icon Fix (design)

> **Status:** Design — approved by user; pending writing-plans.
> **Date:** 2026-06-29
> **Parent specs:** `2026-06-29-library-ingest-and-folder-management-design.md` (Plan 3.5,
> merged) and `2026-06-28-ferrolite-v1-architecture-map.md` (cross-cutting contracts).
> **Position:** a follow-up polish/QoL increment on the just-merged Library tree.
> **UI target:** the Library module (left-panel folder tree, status bar).

---

## 1. Goal

Three user-requested improvements to the freshly-merged Library folder tree:

1. **Fix the tree icons** — the disclosure arrows (`▾`/`▸`) and the hover remove (`✕`) render as
   tofu placeholders because they are font glyphs absent from the bundled IBM Plex fonts (and from
   egui's fallback fonts). Render them as **painted shapes** instead.
2. **Reindex a folder**, with two modes: **Soft** (add only new/changed files) and **Hard**
   (force re-decode + re-thumbnail everything, and prune catalog rows for files/folders deleted
   from disk).
3. **Periodic new-file watcher** — cheaply poll the selected folder's subtree (~10s) and
   auto-ingest newly-appeared files silently, as a quality-of-life convenience.

## 2. Cross-cutting contracts honored

- **`ferrolite-jobs` stays photo-agnostic** — reindex and the watcher submit ordinary jobs; no
  jobs-crate changes. Reindex/watcher ingests are cancellable and prioritized exactly like Plan 3.5
  ingest (Interactive for user actions; Background for the watcher).
- **Catalog is a rebuildable cache** — Hard reindex's prune deletes catalog rows only, never disk
  files; the directory remains the source of truth.
- **WAL writer + ReadPool** — new reads (`folder_path`) come from the pool; prune writes go through
  the single `Catalog` writer.
- **No schema change** — all three items reuse the Plan 3.5 schema.

---

## 3. Item 1 — Tree icons (paint, don't render glyphs)

`ferrolite-app/src/library/panel.rs` currently uses `egui::Button::new("▾"/"▸")` and
`small_button("✕")`. The bundled fonts (IBM Plex Sans/Mono in `theme.rs`) lack U+25BE/U+25B8/U+2715,
so they render as `□`. Replace both with painted shapes (no font dependency, always crisp):

- **Disclosure arrow:** allocate a ~14×14 click-sensed rect (`ui.allocate_response(vec2(14.0, 14.0),
  Sense::click())`) and paint egui's native triangle via
  `egui::collapsing_header::paint_default_icon(ui, openness, &response)` with `openness = 1.0` when
  expanded else `0.0` (the helper rotates the triangle for the open/closed state). Toggle
  `expanded_folders` on `response.clicked()`. Non-expandable rows reserve the same 14px width
  (existing `add_space`).
- **Remove ✕:** **always reserve** a ~14px trailing slot per row (allocate the rect every row,
  right-aligned) — this removes the prior hover-induced row relayout. Paint an `✕` as two
  `egui::Shape::line_segment`s in `theme::TEXT_DIM`, brightening to `TEXT_PRIMARY` when the slot (or
  row) is hovered, and **only when hovered**. Click on the slot → `request_remove(...)` (unchanged
  leaf-vs-subtree logic).

No behavior change beyond rendering; the click semantics (toggle expand, select, remove) are
identical to today.

---

## 4. Item 2 — Reindex (Soft / Hard)

### 4.1 `ReindexMode`

Introduce `ReindexMode { Incremental, Full }` (defined in `ferrolite-app`, the orchestration layer;
the catalog stays mode-agnostic). It is threaded into the recursive ingest job — the same
walk → upsert folder rows → upsert image rows → fan-out thumbnail jobs machinery from Plan 3.5
(DRY; no parallel ingest path).

- **Incremental (Soft):** unchanged Plan 3.5 behavior — `needs_reingest` skips unchanged
  `(mtime, size)`; only new/changed files are decoded and (re)thumbnailed.
- **Full (Hard):**
  - **Force** every file through decode + thumbnail (bypass the `needs_reingest` skip).
    `upsert_image` (ON CONFLICT DO UPDATE) and `put_thumbnail` (ON CONFLICT DO UPDATE) already
    overwrite in place, so this regenerates rows and thumbnail BLOBs.
  - **Prune** after the walk: delete catalog rows for files and subdirectories no longer on disk so
    the catalog mirrors the tree exactly.

### 4.2 Catalog `prune_subtree`

New writer method:

```rust
pub fn prune_subtree(
    &self,
    root_folder_id: i64,
    kept_folder_ids: &HashSet<i64>,
    kept_image_ids: &HashSet<i64>,
) -> Result<(), CatalogError>
```

In one `unchecked_transaction()`, scoped to the subtree rooted at `root_folder_id` (the recursive
`parent_id` CTE already used by `remove_folder`/`list_images_recursive`):

1. Delete `thumbnails` for images in the subtree whose `image_id ∉ kept_image_ids`.
2. Delete `images` in the subtree whose `id ∉ kept_image_ids`.
3. Delete `folders` in the subtree whose `id ∉ kept_folder_ids` (vanished subdirectories; the root
   is always in `kept_folder_ids` so it is never pruned).

Implementation: read the subtree's `(folder_id, image_id)` and folder ids via the CTE, diff against
the kept sets in Rust, and `DELETE … WHERE id = ?` the absent ones (avoids building giant `IN`
clauses). The Full ingest job collects `kept_folder_ids` (= `dir_ids` values) and `kept_image_ids`
(the ids returned by each `upsert_image`) and calls `prune_subtree` once, after all upserts, before
`IngestDone`.

### 4.3 Orchestration & UI

- `spawn_reindex(state, ctx, folder_id, folder_path, mode)` in `ingest.rs`:
  - cancels any in-flight ingest + pending thumbnail jobs (a new `AppState::cancel_pending_jobs()`,
    extracted from `reset_for_new_folder`), **without** clearing `images`/`current_folder`/selection
    (reindex updates the view in place, unlike "Open folder");
  - for `Full`, zeroes `thumb_total`/`thumb_done` for a clean status-bar progress readout;
  - submits an **Interactive** ingest job carrying `mode` on `folder_path`.
- `spawn_ingest` (Open folder) is refactored to share the job body, passing `ReindexMode::Incremental`
  and the existing full `reset_for_new_folder`.
- **Left-panel context menu** (`panel.rs`) gains, above "Remove from catalog":
  - **"Reindex — new files"** → `spawn_reindex(.., Incremental)`
  - **"Reindex — full rebuild"** → `spawn_reindex(.., Full)`
  Both use the row's `FolderRecord.path` and `id`.

`reset_for_new_folder` is refactored to call `cancel_pending_jobs()` then do the view/counter reset,
so existing behavior is preserved.

---

## 5. Item 3 — Periodic new-file watcher

A cheap, silent background poll that auto-ingests files newly dropped into the selected folder's
subtree.

### 5.1 State & timing

- `AppState` gains `last_watch_check: Option<std::time::Instant>` and `ingest_active: bool`.
- `WATCH_INTERVAL: Duration = 10s` (named constant in `ingest.rs` or `app.rs`).
- `app.rs::update` calls `ctx.request_repaint_after(WATCH_INTERVAL)` every frame so idle frames still
  wake to tick the watcher.
- `ingest_active` is set `true` when any ingest job is spawned (open/reindex/watcher) and `false` on
  `AppEvent::IngestDone`.

### 5.2 Tick logic (pure, testable)

A pure predicate decides whether to fire:

```rust
fn should_watch(now, last_check, interval, current_folder, ingest_active) -> bool
```

returns `true` iff a folder is selected, no ingest is active, and `now - last_check ≥ interval`
(or `last_check` is `None`). `app.rs` calls it each frame; on `true` it records `now`, looks up the
current folder's path, and spawns the watcher job.

### 5.3 Watcher job

Reuses the **Incremental** ingest path (DRY with Item 2), submitted at **Background** priority on the
current folder's path. It:

- walks the subtree (`scan_tree`) and runs `needs_reingest` per file (indexed lookups — **no decode**
  when nothing changed, satisfying "very cheap" for the common case);
- decodes + thumbnails only genuinely-new/changed files;
- emits the normal `Indexed`/`ThumbRegistered`/`ThumbReady`/`IngestDone` events, so new files appear
  via the existing dirty-flag grid refresh.

It does **not** reset the view or counters. The `ingest_active` guard prevents overlapping ticks;
switching folders (`reset_for_new_folder`) cancels any in-flight watcher ingest. It needs the current
folder's path:

```rust
// ReadPool + Catalog
pub fn folder_path(&self, folder_id: i64) -> Result<Option<String>, CatalogError>
```

an indexed single-row lookup on `folders.id`.

---

## 6. Error handling

- Reindex/watcher decode failures route through the existing `decode_status = Failed` path (broken
  placeholder); one bad file never downs the pass (Plan 3.5 §7).
- `prune_subtree` runs in a transaction: a failure rolls back, leaving the catalog consistent.
- Watcher on a vanished folder: `scan_tree` returns empty / `folder_path` returns the stale path;
  `needs_reingest` finds nothing new → no-op. (Hard reindex is the path that prunes vanished
  entries.)
- If the watcher races a manual reindex, the `ingest_active` guard suppresses the watcher tick.

---

## 7. Testing

Pure/headless logic carries coverage; painted icons and `request_repaint_after` are UI glue
(verified by build + manual eyeball):

- **`prune_subtree` (catalog integration):** after a Full-style prune with a kept set missing one
  file and one subdirectory, those rows + their thumbnails are gone, sibling folders/images remain,
  and the root folder is never pruned.
- **Full reindex re-thumbnails:** an already-indexed file with an existing thumbnail is re-processed
  (decode + `put_thumbnail` overwrite) under `Full`, whereas `Incremental` skips it (assert via a
  spy/҂count or a changed-thumbnail check at the catalog level — e.g. force-mode processes a file
  whose `(mtime,size)` is unchanged).
- **`should_watch` (pure):** fires only when a folder is selected, no ingest active, and the interval
  elapsed; respects `None` last-check.
- **`cancel_pending_jobs` / `reset_for_new_folder`:** the refactor preserves existing reset behavior
  (existing state tests stay green; add one asserting `cancel_pending_jobs` leaves `images`/
  `current_folder` intact while draining `thumb_jobs`).
- 80%+ on non-GPU logic, continuing the project standard. Full workspace gate
  (`cargo fmt --check`, `clippy --workspace --all-targets -D warnings`, `cargo test --workspace`).

---

## 8. Plan shape (for writing-plans)

Four independently-testable tasks:

1. **Tree-icon paint fix** (`panel.rs`) — painted disclosure triangle + reserved/painted ✕; build +
   manual verify.
2. **Catalog prune + ingest `ReindexMode`** — `prune_subtree` (+ integration test); thread
   `ReindexMode { Incremental, Full }` and the force/prune logic into the ingest job; `folder_path`
   query.
3. **Reindex orchestration + UI** — `cancel_pending_jobs` extraction, `spawn_reindex`, refactor
   `spawn_ingest` to share the job body, context-menu entries.
4. **Watcher** — `last_watch_check`/`ingest_active` state, `should_watch` predicate (+ test),
   `request_repaint_after` wiring, Background watcher spawn.

---

## 9. Deliverable

Crisp disclosure/remove icons in the folder tree; right-click Reindex (new-files / full-rebuild)
that updates the view in place, with Full mirroring the directory (re-thumbnail + prune deleted);
and a silent ~10s watcher that auto-ingests files dropped into the selected subtree — all on the
existing jobs/WAL/cache contracts, with no schema change.
