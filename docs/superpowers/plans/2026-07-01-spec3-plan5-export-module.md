# Spec 3 Plan 5 — Export Module (batch) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a third top-level **Export** module with a catalog-persisted queue, add-to-queue actions from Library and Develop, a shared export-settings panel + destination + filename token-template + Start, and batch orchestration that runs one `ferrolite-export` Background job per queued image with aggregate progress and cancellation.

**Architecture:** The `export_queue` catalog table (`image_id`, `position`, `added_at`) persists the queue but is a **cache** — the in-memory `Vec<i64>` is authoritative for the session and its loss never loses photos or edits. Each queued image is exported by a Background job that decodes the image on the worker thread (RAW: `decode_full`→`QuadBin`; Standard: `decode_preview`→`preview_to_linear`), builds the GPU pyramid inside the job (reusing the Plan-4 `ExportSource::FullResCpu` path), computes camera→working from the decoded `ColorProfile`, and calls `run_export` with `OpStack::default()`. A pure `expand`/collision module produces per-image output filenames. Batch aggregate progress and cancellation are tracked in `AppState`.

**Tech Stack:** Rust, egui/eframe, wgpu, rusqlite (pinned 0.32), ferrolite-jobs, ferrolite-export, ferrolite-color, ferrolite-decode, ferrolite-catalog.

## Global Constraints

- **CLAUDE.md responsiveness:** never block the UI/update thread. All decode + encode work runs on `ferrolite-jobs` at `Priority::Background`; results flow back over the `AppState` event channel (`state.tx`) followed by `ctx.request_repaint()`. Batch decode happens **inside** the Background job, never on the UI thread.
- **CLAUDE.md GPU rule:** build pipelines once; do not rebuild per image/open. Batch reuses the existing per-job `GpuPyramidSource::new` + `run_export` path (which builds the tile pipeline inside the job, off the UI thread) exactly as the single-file flow does — no new per-frame GPU work.
- **§5 catalog-is-a-cache contract:** the `export_queue` table is persisted UI state, not source-of-truth. Any `export_queue` DB error is treated like a catalog-cache error: the in-memory queue stays authoritative for the session, a warning is surfaced, images/edits are never lost, and nothing panics.
- **rusqlite is pinned to 0.32** — do not bump it (see `[[rusqlite-pinned-0-32]]`).
- **Per-component reset (design, load-bearing):** N/A here — the export-settings panel controls are batch parameters (format/space/resize/quality/metadata), not per-image editable adjustments, so the per-control reset-arrow rule does not apply. (No `EguiSlider`-backed edit-DAG controls are added by this plan.)
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean; typed errors with `thiserror` in libraries; no `unwrap()` outside tests; immutable-by-default.
- **Gate (necessary, not sufficient):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → then **STOP and hold for Jann's hands-on visual test** of the running app before finishing the branch (CLAUDE.md "Finishing a branch" rule). egui UI has no golden tests.
- **Branch:** continue on `feat/color-and-export` (already checked out; Plan 4 merged).
- **Scope note — edits not persisted:** per-image `OpStack` edits are not persisted anywhere in the catalog (only a `has_edits` flag on the `images` row). Batch export therefore renders each image with `OpStack::default()` (color-managed but unedited). This matches the spec §2 non-goal "Batch edits / copy-paste adjustments … this is batch *export* only." Document this in code comments; do not attempt to hydrate per-image edits.

---

## File Structure

**Created:**
- `ferrolite-export/src/filename.rs` — pure filename token expander + collision resolver + capture-date formatter. Re-exported from `lib.rs`.
- `ferrolite-app/src/export/batch.rs` — batch orchestration: `BatchExportState`, `BatchItem`, `spawn_batch`.
- `ferrolite-app/src/export/settings_form.rs` — the shared export-options form (extracted from the Plan-4 dialog), reused by both the single-file dialog and the batch panel.
- `ferrolite-app/src/export_module/mod.rs` — the Export module UI (toolbar + content dispatch).
- `ferrolite-app/src/export_module/queue_list.rs` — the queue list widget (thumbnails, remove, reorder).
- `ferrolite-app/src/export_module/bottom_bar.rs` — destination picker + filename-template field + Start/Cancel.

**Modified:**
- `ferrolite-catalog/src/schema.rs` — bump `SCHEMA_VERSION` 4→5; add `export_queue` table migration.
- `ferrolite-catalog/src/model.rs` — add `ExportQueueEntry`.
- `ferrolite-catalog/src/queries.rs` — `list_export_queue`, `images_by_ids` free functions.
- `ferrolite-catalog/src/catalog.rs` — `add_to_export_queue`, `remove_from_export_queue`, `clear_export_queue`, `reorder_export_queue`, `list_export_queue`, `images_by_ids` write/read methods + tests.
- `ferrolite-catalog/src/read_pool.rs` — `list_export_queue`, `images_by_ids` read methods.
- `ferrolite-catalog/src/lib.rs` — re-export `ExportQueueEntry`.
- `ferrolite-export/src/lib.rs` — `pub mod filename;` + re-exports.
- `ferrolite-app/src/module.rs` — add `Module::Export`; keep/adjust helpers.
- `ferrolite-app/src/chrome/mod.rs` — third segmented-control entry; `MenuAction::AddToQueue`.
- `ferrolite-app/src/events.rs` — `AppEvent::BatchItemFinished`; `apply` fold.
- `ferrolite-app/src/state.rs` — export-module `AppState` fields; startup queue load; queue mutation helpers.
- `ferrolite-app/src/export/mod.rs` — `pub mod batch; pub mod settings_form;` + call `settings_form` from `draw_dialog`.
- `ferrolite-app/src/library/image_context_menu.rs` — "Add to export queue" item.
- `ferrolite-app/src/app.rs` — module dispatch refactor (`is_library()` → `match Module`); Export module wiring; handle `MenuAction::AddToQueue`; batch event repaint.
- `ferrolite-app/src/lib.rs` (or wherever modules are declared) — declare `pub mod export_module;`.
- `docs/design/ferrolite-design-system.md` — two modules → three.

---

## Task 1: `export_queue` catalog table + repository

**Files:**
- Modify: `ferrolite-catalog/src/schema.rs`
- Modify: `ferrolite-catalog/src/model.rs`
- Modify: `ferrolite-catalog/src/queries.rs`
- Modify: `ferrolite-catalog/src/catalog.rs`
- Modify: `ferrolite-catalog/src/read_pool.rs`
- Modify: `ferrolite-catalog/src/lib.rs`

**Interfaces:**
- Produces:
  - `ExportQueueEntry { pub image_id: i64, pub position: i64, pub added_at: i64 }` (in `model.rs`, re-exported from `lib.rs`).
  - `Catalog::add_to_export_queue(&self, image_id: i64, added_at: i64) -> Result<(), CatalogError>` — appends at `max(position)+1`; idempotent (ignores an image already queued).
  - `Catalog::remove_from_export_queue(&self, image_id: i64) -> Result<(), CatalogError>`.
  - `Catalog::clear_export_queue(&self) -> Result<(), CatalogError>`.
  - `Catalog::reorder_export_queue(&self, ordered_ids: &[i64]) -> Result<(), CatalogError>` — rewrites `position` to match `ordered_ids` order.
  - `Catalog::list_export_queue(&self) -> Result<Vec<i64>, CatalogError>` — image_ids ordered by `position` ascending.
  - `Catalog::images_by_ids(&self, ids: &[i64]) -> Result<Vec<ImageRecord>, CatalogError>` — for rendering queue rows.
  - `ReadPool::list_export_queue(&self) -> Result<Vec<i64>, CatalogError>` and `ReadPool::images_by_ids(&self, ids: &[i64]) -> Result<Vec<ImageRecord>, CatalogError>`.

- [ ] **Step 1: Add the migration (write the table, then test it).** In `ferrolite-catalog/src/schema.rs`, bump the version constant and add a `version < 5` block after the existing `version < 4` block.

Change the constant:
```rust
pub const SCHEMA_VERSION: i64 = 5;
```
Add after the `version < 4` block (before the final `debug_assert_eq!`/`pragma_update`):
```rust
    if version < 5 {
        // Persisted export queue (spec §8.4). This is UI-state CACHE, not
        // source-of-truth: ON DELETE CASCADE means a deleted image drops out of
        // the queue automatically, and losing the table never loses photos/edits.
        conn.execute_batch(
            "CREATE TABLE export_queue (
                 image_id  INTEGER NOT NULL PRIMARY KEY
                            REFERENCES images(id) ON DELETE CASCADE,
                 position  INTEGER NOT NULL,
                 added_at  INTEGER NOT NULL
             );
             CREATE INDEX idx_export_queue_position ON export_queue(position);",
        )?;
        version = 5;
    }
```

- [ ] **Step 2: Write the failing migration test.** In `schema.rs`'s `#[cfg(test)] mod tests`, add:
```rust
    #[test]
    fn migrate_creates_export_queue_v5() {
        let conn = Connection::open_in_memory().unwrap();
        super::migrate(&conn).unwrap();
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('export_queue')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(cols.contains(&"image_id".to_string()));
        assert!(cols.contains(&"position".to_string()));
        assert!(cols.contains(&"added_at".to_string()));
    }
```

- [ ] **Step 3: Run it to verify pass (migration already written).**

Run: `cargo test -p ferrolite-catalog schema::tests::migrate_creates_export_queue_v5 -- --nocapture`
Expected: PASS. (Also confirm the existing `debug_assert_eq!(version, SCHEMA_VERSION, ...)` still holds — if the existing `fresh_db_is_migrated_to_current_version` test references `SCHEMA_VERSION` it now expects 5.)

- [ ] **Step 4: Add the `ExportQueueEntry` model.** In `ferrolite-catalog/src/model.rs`, add:
```rust
/// A row of the persisted export queue (spec §8.4). Ordered by `position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportQueueEntry {
    pub image_id: i64,
    pub position: i64,
    pub added_at: i64,
}
```
In `ferrolite-catalog/src/lib.rs`, add `ExportQueueEntry` to the `pub use crate::model::{...}` re-export list (match the existing style used for `ImageRecord`, `TagRecord`, etc.).

- [ ] **Step 5: Add the read queries.** In `ferrolite-catalog/src/queries.rs`, add two free functions (mirroring the existing `list_images` / row-mapping style):
```rust
pub(crate) fn list_export_queue(conn: &Connection) -> Result<Vec<i64>, CatalogError> {
    let mut stmt = conn.prepare("SELECT image_id FROM export_queue ORDER BY position ASC")?;
    let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn images_by_ids(
    conn: &Connection,
    ids: &[i64],
) -> Result<Vec<crate::model::ImageRecord>, CatalogError> {
    // Preserve the input order; skip ids that no longer exist.
    let mut out = Vec::with_capacity(ids.len());
    let mut stmt = conn.prepare(&format!(
        "SELECT {IMAGE_COLS} FROM images WHERE id = ?1"
    ))?;
    for &id in ids {
        let mut rows = stmt.query_map(rusqlite::params![id], row_to_image)?;
        if let Some(r) = rows.next() {
            out.push(r?);
        }
    }
    Ok(out)
}
```
Note: reuse the crate's existing image-column constant + row mapper. If they are named differently than `IMAGE_COLS` / `row_to_image`, use the actual names already in `queries.rs` (grep for the existing `list_images` implementation and copy its column list + mapper). Do NOT invent a new column list — reuse the one that already maps to `ImageRecord`.

- [ ] **Step 6: Add the `Catalog` write + read methods.** In `ferrolite-catalog/src/catalog.rs`, inside the main `impl Catalog` block (near `set_has_edits`), add:
```rust
    /// Append `image_id` to the persisted export queue at the next position.
    /// Idempotent: re-adding an already-queued image is a no-op (keeps its slot).
    pub fn add_to_export_queue(&self, image_id: i64, added_at: i64) -> Result<(), CatalogError> {
        let next: i64 = self.conn().query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM export_queue",
            [],
            |r| r.get(0),
        )?;
        self.conn().execute(
            "INSERT OR IGNORE INTO export_queue (image_id, position, added_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![image_id, next, added_at],
        )?;
        Ok(())
    }

    pub fn remove_from_export_queue(&self, image_id: i64) -> Result<(), CatalogError> {
        self.conn().execute(
            "DELETE FROM export_queue WHERE image_id = ?1",
            rusqlite::params![image_id],
        )?;
        Ok(())
    }

    pub fn clear_export_queue(&self) -> Result<(), CatalogError> {
        self.conn().execute("DELETE FROM export_queue", [])?;
        Ok(())
    }

    /// Rewrite `position` so the queue matches `ordered_ids` exactly. Ids present
    /// in the table but absent from `ordered_ids` are removed; ids in
    /// `ordered_ids` absent from the table are ignored (never inserted here).
    pub fn reorder_export_queue(&self, ordered_ids: &[i64]) -> Result<(), CatalogError> {
        let tx = self.conn().unchecked_transaction()?;
        tx.execute("DELETE FROM export_queue WHERE image_id NOT IN (SELECT image_id FROM export_queue)", [])?;
        for (pos, &id) in ordered_ids.iter().enumerate() {
            tx.execute(
                "UPDATE export_queue SET position = ?1 WHERE image_id = ?2",
                rusqlite::params![pos as i64, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_export_queue(&self) -> Result<Vec<i64>, CatalogError> {
        crate::queries::list_export_queue(self.conn())
    }

    pub fn images_by_ids(&self, ids: &[i64]) -> Result<Vec<crate::model::ImageRecord>, CatalogError> {
        crate::queries::images_by_ids(self.conn(), ids)
    }
```
Note on `self.conn()`: use the exact accessor the crate already uses for the writer connection (the earlier `set_has_edits` uses `self.conn()`). If `unchecked_transaction` is unavailable on the borrow, wrap the reorder loop in a plain `execute_batch`-free loop without an explicit transaction (correctness is unaffected for a UI-state cache); prefer the transaction if it compiles.

- [ ] **Step 7: Add the `ReadPool` read methods.** In `ferrolite-catalog/src/read_pool.rs`, inside `impl ReadPool` (mirroring `list_tags`), add:
```rust
    pub fn list_export_queue(&self) -> Result<Vec<i64>, CatalogError> {
        self.with_conn(crate::queries::list_export_queue)
    }

    pub fn images_by_ids(&self, ids: &[i64]) -> Result<Vec<crate::ImageRecord>, CatalogError> {
        self.with_conn(|c| crate::queries::images_by_ids(c, ids))
    }
```

- [ ] **Step 8: Write the failing repository test.** In `ferrolite-catalog/src/catalog.rs`, add a test module (mirroring `collection_tests`):
```rust
#[cfg(test)]
mod export_queue_tests {
    use super::*;
    use ferrolite_image::FileKind;

    fn img(cat: &Catalog, folder: i64, name: &str) -> i64 {
        cat.upsert_image(&crate::model::NewImage::failed(
            folder,
            name.to_string(),
            1,
            1,
            FileKind::Raw,
            0,
        ))
        .unwrap()
    }

    #[test]
    fn add_list_reorder_remove_clear() {
        let cat = Catalog::open_in_memory().unwrap();
        let f = cat.upsert_folder(std::path::Path::new("/p"), None).unwrap();
        let a = img(&cat, f, "a.nef");
        let b = img(&cat, f, "b.nef");
        let c = img(&cat, f, "c.nef");

        cat.add_to_export_queue(a, 10).unwrap();
        cat.add_to_export_queue(b, 11).unwrap();
        cat.add_to_export_queue(c, 12).unwrap();
        // idempotent re-add keeps order/length
        cat.add_to_export_queue(a, 99).unwrap();
        assert_eq!(cat.list_export_queue().unwrap(), vec![a, b, c]);

        cat.reorder_export_queue(&[c, a, b]).unwrap();
        assert_eq!(cat.list_export_queue().unwrap(), vec![c, a, b]);

        cat.remove_from_export_queue(a).unwrap();
        assert_eq!(cat.list_export_queue().unwrap(), vec![c, b]);

        // images_by_ids preserves input order and skips missing ids
        let recs = cat.images_by_ids(&[b, 99999, c]).unwrap();
        assert_eq!(recs.iter().map(|r| r.id).collect::<Vec<_>>(), vec![b, c]);

        cat.clear_export_queue().unwrap();
        assert!(cat.list_export_queue().unwrap().is_empty());
    }

    #[test]
    fn deleting_image_cascades_out_of_queue() {
        let cat = Catalog::open_in_memory().unwrap();
        let f = cat.upsert_folder(std::path::Path::new("/p"), None).unwrap();
        let a = img(&cat, f, "a.nef");
        cat.add_to_export_queue(a, 1).unwrap();
        cat.conn()
            .execute("DELETE FROM images WHERE id = ?1", rusqlite::params![a])
            .unwrap();
        assert!(cat.list_export_queue().unwrap().is_empty(), "FK cascade removes queue row");
    }
}
```
Note: `NewImage::failed(folder, filename, mtime, size, kind, added_at)` is the existing helper used by other tests (see `has_edits_tests`); match its real signature. If `self.conn()` is private to the module, the cascade test lives in the same module so it can call it.

- [ ] **Step 9: Run the tests.**

Run: `cargo test -p ferrolite-catalog`
Expected: PASS (new `export_queue_tests` + `migrate_creates_export_queue_v5` + all existing).

- [ ] **Step 10: Commit.**
```bash
git add ferrolite-catalog/src
git commit -m "feat(catalog): export_queue table + repository (add/remove/list/reorder/clear)"
```

---

## Task 2: Pure filename token expander + collision resolver

**Files:**
- Create: `ferrolite-export/src/filename.rs`
- Modify: `ferrolite-export/src/lib.rs`

**Interfaces:**
- Produces (all pure, no I/O):
  - `FilenameCtx { pub name: String, pub seq: usize, pub date: String, pub camera: String }`
  - `expand(template: &str, ctx: &FilenameCtx) -> String` — substitutes `{name}`, `{seq}`, `{seq:0N}` (zero-pad width N), `{date}`, `{camera}`; keeps literal text; passes unknown `{token}` through verbatim (including braces).
  - `resolve_collision(stem: &str, ext: &str, taken: &mut std::collections::HashSet<String>) -> String` — returns `"{stem}.{ext}"`, or `"{stem}_1.{ext}"`, `"{stem}_2.{ext}"`, … until unused; inserts the chosen name into `taken`.
  - `format_capture_date(capture_time: Option<&str>) -> String` — `"2026:06:29 12:00:00"` → `"2026-06-29"`; `None`/malformed → `""`.

- [ ] **Step 1: Write the failing tests.** Create `ferrolite-export/src/filename.rs`:
```rust
//! Pure filename token expansion + collision resolution for batch export
//! (spec §8.4). No filesystem or clock access — every value is supplied by the
//! caller so the whole module is unit-testable on every OS.

use std::collections::HashSet;

/// Values substituted into a filename template.
#[derive(Debug, Clone)]
pub struct FilenameCtx {
    /// Original file basename (no extension), for `{name}`.
    pub name: String,
    /// 1-based counter for `{seq}` / `{seq:0N}`.
    pub seq: usize,
    /// Preformatted date string for `{date}` (e.g. "2026-06-29"); may be empty.
    pub date: String,
    /// Camera make+model for `{camera}`; may be empty.
    pub camera: String,
}

/// Expand `template` against `ctx`. Recognised tokens: `{name}`, `{seq}`,
/// `{seq:0N}` (zero-padded to width N), `{date}`, `{camera}`. Any other
/// `{...}` run is emitted verbatim (braces included). Literal text passes through.
pub fn expand(template: &str, ctx: &FilenameCtx) -> String {
    let mut out = String::with_capacity(template.len() + 8);
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(close) = template[i..].find('}') {
                let token = &template[i + 1..i + close];
                match resolve_token(token, ctx) {
                    Some(rep) => {
                        out.push_str(&rep);
                        i += close + 1;
                        continue;
                    }
                    None => {
                        // Unknown token → emit verbatim including braces.
                        out.push_str(&template[i..i + close + 1]);
                        i += close + 1;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn resolve_token(token: &str, ctx: &FilenameCtx) -> Option<String> {
    match token {
        "name" => Some(ctx.name.clone()),
        "seq" => Some(ctx.seq.to_string()),
        "date" => Some(ctx.date.clone()),
        "camera" => Some(ctx.camera.clone()),
        _ => {
            // {seq:0N} zero-padded sequence.
            if let Some(width) = token.strip_prefix("seq:0") {
                if let Ok(w) = width.parse::<usize>() {
                    return Some(format!("{:0width$}", ctx.seq, width = w));
                }
            }
            None
        }
    }
}

/// Return a filename `"{stem}.{ext}"` unique within `taken`, appending `_1`,
/// `_2`, … to the stem on collision. The chosen name is inserted into `taken`.
pub fn resolve_collision(stem: &str, ext: &str, taken: &mut HashSet<String>) -> String {
    let base = format!("{stem}.{ext}");
    if taken.insert(base.clone()) {
        return base;
    }
    let mut n = 1usize;
    loop {
        let candidate = format!("{stem}_{n}.{ext}");
        if taken.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Convert an EXIF capture time "YYYY:MM:DD HH:MM:SS" to "YYYY-MM-DD".
/// Returns "" for `None` or anything that does not match the shape.
pub fn format_capture_date(capture_time: Option<&str>) -> String {
    let Some(s) = capture_time else {
        return String::new();
    };
    let date_part = s.split_whitespace().next().unwrap_or("");
    let comps: Vec<&str> = date_part.split(':').collect();
    if comps.len() == 3 && comps.iter().all(|c| !c.is_empty()) {
        format!("{}-{}-{}", comps[0], comps[1], comps[2])
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> FilenameCtx {
        FilenameCtx {
            name: "DSC_0001".into(),
            seq: 7,
            date: "2026-06-29".into(),
            camera: "Nikon Z f".into(),
        }
    }

    #[test]
    fn substitutes_known_tokens_and_literals() {
        assert_eq!(expand("{name}_{date}", &ctx()), "DSC_0001_2026-06-29");
        assert_eq!(expand("edit-{camera}", &ctx()), "edit-Nikon Z f");
        assert_eq!(expand("{name}", &ctx()), "DSC_0001");
    }

    #[test]
    fn seq_plain_and_zero_padded() {
        assert_eq!(expand("{seq}", &ctx()), "7");
        assert_eq!(expand("img_{seq:03}", &ctx()), "img_007");
        assert_eq!(expand("{seq:05}", &ctx()), "00007");
    }

    #[test]
    fn unknown_token_passes_through_verbatim() {
        assert_eq!(expand("{name}_{bogus}", &ctx()), "DSC_0001_{bogus}");
        assert_eq!(expand("plain text", &ctx()), "plain text");
    }

    #[test]
    fn collision_auto_suffix() {
        let mut taken = HashSet::new();
        assert_eq!(resolve_collision("photo", "jpg", &mut taken), "photo.jpg");
        assert_eq!(resolve_collision("photo", "jpg", &mut taken), "photo_1.jpg");
        assert_eq!(resolve_collision("photo", "jpg", &mut taken), "photo_2.jpg");
        assert_eq!(resolve_collision("other", "jpg", &mut taken), "other.jpg");
    }

    #[test]
    fn capture_date_formatting() {
        assert_eq!(format_capture_date(Some("2026:06:29 12:00:00")), "2026-06-29");
        assert_eq!(format_capture_date(Some("garbage")), "");
        assert_eq!(format_capture_date(None), "");
    }
}
```

- [ ] **Step 2: Wire the module.** In `ferrolite-export/src/lib.rs`, add after the existing `mod` lines:
```rust
pub mod filename;
```
and add to the `pub use` block:
```rust
pub use filename::{expand as expand_filename, format_capture_date, resolve_collision, FilenameCtx};
```

- [ ] **Step 3: Run the tests.**

Run: `cargo test -p ferrolite-export filename`
Expected: PASS (all 5 tests).

- [ ] **Step 4: Commit.**
```bash
git add ferrolite-export/src/filename.rs ferrolite-export/src/lib.rs
git commit -m "feat(export): pure filename token expander + collision resolver"
```

---

## Task 3: Extract the shared export-settings form (DRY refactor)

**Files:**
- Create: `ferrolite-app/src/export/settings_form.rs`
- Modify: `ferrolite-app/src/export/mod.rs`

**Interfaces:**
- Produces: `pub fn settings_form(ui: &mut egui::Ui, o: &mut ferrolite_export::ExportOptions)` — draws Format / Output color space / Bit depth / Quality / Resize / metadata checkboxes (exactly the controls currently inline in `draw_dialog`). No buttons, no window.
- Consumes: none new.

- [ ] **Step 1: Create the form module.** Create `ferrolite-app/src/export/settings_form.rs` and move the option-editing body out of `draw_dialog` verbatim:
```rust
//! The shared export-options form (spec §8.2). Extracted from the single-file
//! dialog so the batch Export module's settings panel renders identical controls.

use ferrolite_color::WorkingSpace;
use ferrolite_export::{BitDepth, ExportFormat, ExportOptions, ResizeSpec};

/// Draw every export option control into `ui`. Callers own the surrounding
/// window/panel and any confirm/cancel affordances.
pub fn settings_form(ui: &mut egui::Ui, o: &mut ExportOptions) {
    egui::ComboBox::from_label("Format")
        .selected_text(o.format.label())
        .show_ui(ui, |ui| {
            for f in ExportFormat::ALL {
                ui.selectable_value(&mut o.format, f, f.label());
            }
        });

    egui::ComboBox::from_label("Output color space")
        .selected_text(format!("{:?}", o.output_space))
        .show_ui(ui, |ui| {
            for ws in WorkingSpace::ALL {
                ui.selectable_value(&mut o.output_space, ws, format!("{ws:?}"));
            }
        });

    ui.horizontal(|ui| {
        ui.label("Bit depth");
        ui.selectable_value(&mut o.bit_depth, BitDepth::Eight, "8-bit");
        ui.add_enabled_ui(o.format.supports_16bit(), |ui| {
            ui.selectable_value(&mut o.bit_depth, BitDepth::Sixteen, "16-bit");
        });
    });
    if !o.format.supports_16bit() {
        o.bit_depth = BitDepth::Eight;
    }

    ui.add_enabled_ui(o.format.supports_quality(), |ui| {
        ui.add(egui::Slider::new(&mut o.quality, 1..=100).text("Quality"));
    });

    let mut mode = match o.resize {
        ResizeSpec::None => 0,
        ResizeSpec::LongEdge(_) => 1,
        ResizeSpec::Exact { .. } => 2,
        ResizeSpec::Percent(_) => 3,
    };
    egui::ComboBox::from_label("Resize")
        .selected_text(["None", "Long edge", "Exact", "Percent"][mode])
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut mode, 0, "None");
            ui.selectable_value(&mut mode, 1, "Long edge");
            ui.selectable_value(&mut mode, 2, "Exact");
            ui.selectable_value(&mut mode, 3, "Percent");
        });
    o.resize = match mode {
        1 => {
            let mut px = if let ResizeSpec::LongEdge(p) = o.resize { p } else { 2048 };
            ui.add(egui::DragValue::new(&mut px).range(1..=100_000).prefix("px "));
            ResizeSpec::LongEdge(px)
        }
        2 => {
            let (mut w, mut h) = if let ResizeSpec::Exact { w, h } = o.resize { (w, h) } else { (1920, 1080) };
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut w).range(1..=100_000).prefix("W "));
                ui.add(egui::DragValue::new(&mut h).range(1..=100_000).prefix("H "));
            });
            ResizeSpec::Exact { w, h }
        }
        3 => {
            let mut pct = if let ResizeSpec::Percent(p) = o.resize { p * 100.0 } else { 50.0 };
            ui.add(egui::Slider::new(&mut pct, 1.0..=100.0).suffix("%"));
            ResizeSpec::Percent(pct / 100.0)
        }
        _ => ResizeSpec::None,
    };

    ui.separator();
    ui.checkbox(&mut o.copy_exif, "Copy EXIF metadata");
    ui.checkbox(&mut o.embed_icc, "Embed ICC profile");
    ui.checkbox(&mut o.strip_metadata, "Strip metadata");
}
```

- [ ] **Step 2: Call it from `draw_dialog`.** In `ferrolite-app/src/export/mod.rs`, add `pub mod settings_form;` near the top, and replace the option-editing body inside `draw_dialog`'s `.show(ctx, |ui| { ... })` (the block from `let o = &mut dialog.options;` down to the last `ui.checkbox(... "Strip metadata")`) with:
```rust
            crate::export::settings_form::settings_form(ui, &mut dialog.options);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Choose destination…").clicked() {
                    outcome = Some(DialogOutcome::Confirm);
                }
                if ui.button("Cancel").clicked() {
                    outcome = Some(DialogOutcome::Cancel);
                }
            });
```
Remove the now-unused `WorkingSpace`, `BitDepth`, `ExportFormat`, `ResizeSpec` imports from `mod.rs` if the compiler flags them as unused after the move (they moved to `settings_form.rs`).

- [ ] **Step 3: Verify build + no behavior change.**

Run: `cargo build -p ferrolite-app` then `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: builds clean; the single-file Export dialog still shows identical controls (verified in the visual test at the end, not here).

- [ ] **Step 4: Commit.**
```bash
git add ferrolite-app/src/export/settings_form.rs ferrolite-app/src/export/mod.rs
git commit -m "refactor(app): extract shared export settings_form from the single-file dialog"
```

---

## Task 4: Batch export events + orchestration

**Files:**
- Modify: `ferrolite-app/src/events.rs`
- Create: `ferrolite-app/src/export/batch.rs`
- Modify: `ferrolite-app/src/export/mod.rs`

**Interfaces:**
- Produces:
  - `AppEvent::BatchItemFinished { image_id: i64, ok: bool, message: String }`.
  - `BatchExportState { pub total: usize, pub completed: usize, pub failed: usize, pub handles: Vec<ferrolite_jobs::JobHandle>, pub warnings: Vec<String> }` with `pub fn is_done(&self) -> bool { self.completed >= self.total }` and `pub fn cancel_all(&self)`.
  - `BatchItem { pub image_id: i64, pub path: std::path::PathBuf, pub kind: ferrolite_image::FileKind, pub dest: std::path::PathBuf }` — `dest` is the fully-resolved output path (folder + expanded, collision-free filename).
  - `spawn_batch(state: &AppState, egui_ctx: &egui::Context, gpu: std::sync::Arc<GpuContext>, items: Vec<BatchItem>, working_space: WorkingSpace, options: ExportOptions) -> Vec<ferrolite_jobs::JobHandle>`.
- Consumes: `run_export`, `ExportRequest`, `ExportSource` path (via `GpuPyramidSource::new`), `ferrolite_color::camera_to_working`, `ferrolite_decode::{decode_full, decode_preview, QuadBin, ColorProfile}`, `crate::viewer::load::preview_to_linear`.

- [ ] **Step 1: Add the event variant + fold, with a failing test.** In `ferrolite-app/src/events.rs`, add to the `AppEvent` enum (after `ExportFinished`):
```rust
    /// One image of a running batch export finished (ok=false → failed/cancelled).
    /// Folded by `apply` into the aggregate `BatchExportState` counters.
    BatchItemFinished {
        image_id: i64,
        ok: bool,
        message: String,
    },
```
Add the fold arm inside `apply`'s `match` (before the closing `}`):
```rust
            AppEvent::BatchItemFinished { image_id: _, ok, message } => {
                if let Some(b) = self.batch.as_mut() {
                    b.completed += 1;
                    if !ok {
                        b.failed += 1;
                        b.warnings.push(message);
                    }
                    if b.is_done() {
                        b.handles.clear();
                    }
                }
                None
            }
```
Add a test to `events.rs`'s `mod tests`:
```rust
    #[test]
    fn batch_item_finished_folds_into_aggregate() {
        let mut s = AppState::for_test();
        s.batch = Some(crate::export::batch::BatchExportState::new(2));
        s.apply(AppEvent::BatchItemFinished { image_id: 1, ok: true, message: "ok".into() });
        s.apply(AppEvent::BatchItemFinished { image_id: 2, ok: false, message: "disk full".into() });
        let b = s.batch.as_ref().unwrap();
        assert_eq!(b.completed, 2);
        assert_eq!(b.failed, 1);
        assert!(b.is_done());
        assert_eq!(b.warnings, vec!["disk full".to_string()]);
    }
```
(This will not compile until Steps 2–3 add `AppState.batch` and `BatchExportState::new`. That is the expected RED — proceed to add them, then run.)

- [ ] **Step 2: Create the batch orchestration module.** Create `ferrolite-app/src/export/batch.rs`:
```rust
//! Batch export orchestration (spec §8.4). One `ferrolite-export` Background job
//! per queued image. Each job decodes its image ON THE WORKER THREAD (never the
//! UI thread), builds the GPU pyramid inside the job (reusing the single-file
//! `ExportSource::FullResCpu` rationale), computes camera→working from the decoded
//! ColorProfile, and renders with `OpStack::default()` — per-image edits are not
//! persisted, so batch export is color-managed but unedited (spec §2 non-goal).

use std::path::PathBuf;
use std::sync::Arc;

use ferrolite_color::WorkingSpace;
use ferrolite_decode::{ColorProfile, QuadBin};
use ferrolite_export::{run_export, ExportOptions, ExportRequest};
use ferrolite_gpu::GpuContext;
use ferrolite_image::FileKind;
use ferrolite_jobs::{JobHandle, Priority};
use ferrolite_pipeline::{GpuPyramidSource, OpStack};

use crate::events::AppEvent;
use crate::state::AppState;

/// One image to export in a batch. `dest` is the final, collision-resolved path.
#[derive(Debug, Clone)]
pub struct BatchItem {
    pub image_id: i64,
    pub path: PathBuf,
    pub kind: FileKind,
    pub dest: PathBuf,
}

/// Aggregate progress + cancellation handles for a running batch.
#[derive(Default)]
pub struct BatchExportState {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub handles: Vec<JobHandle>,
    pub warnings: Vec<String>,
}

impl BatchExportState {
    pub fn new(total: usize) -> Self {
        Self { total, ..Default::default() }
    }
    pub fn is_done(&self) -> bool {
        self.completed >= self.total
    }
    pub fn cancel_all(&self) {
        for h in &self.handles {
            h.cancel();
        }
    }
}

/// Submit one Background job per item. Returns the job handles (for cancellation).
pub fn spawn_batch(
    state: &AppState,
    egui_ctx: &egui::Context,
    gpu: Arc<GpuContext>,
    items: Vec<BatchItem>,
    working_space: WorkingSpace,
    options: ExportOptions,
) -> Vec<JobHandle> {
    let mut handles = Vec::with_capacity(items.len());
    for item in items {
        let tx = state.tx.clone();
        let egui_ctx = egui_ctx.clone();
        let gpu = Arc::clone(&gpu);
        let handle = state.jobs.submit(Priority::Background, move |cancel| {
            let (ok, message) = run_one(&gpu, &item, working_space, &options, cancel);
            let _ = tx.send(AppEvent::BatchItemFinished {
                image_id: item.image_id,
                ok,
                message,
            });
            egui_ctx.request_repaint();
        });
        handles.push(handle);
    }
    handles
}

fn run_one(
    gpu: &Arc<GpuContext>,
    item: &BatchItem,
    working_space: WorkingSpace,
    options: &ExportOptions,
    cancel: &ferrolite_jobs::CancelToken,
) -> (bool, String) {
    if cancel.is_cancelled() {
        return (false, "Export cancelled".to_string());
    }
    // Decode full-res on the worker thread → (linear image, color profile).
    let (linear, profile) = match item.kind {
        FileKind::Raw => match ferrolite_decode::decode_full(&item.path) {
            Ok(raw) => {
                let profile = raw.color_profile.clone();
                (QuadBin.to_linear_rgba_f32(&raw), profile)
            }
            Err(e) => return (false, format!("Decode failed: {e}")),
        },
        _ => match ferrolite_decode::decode_preview(&item.path, item.kind) {
            Ok(buf) => (
                crate::viewer::load::preview_to_linear(&buf),
                ColorProfile::srgb_fallback(),
            ),
            Err(e) => return (false, format!("Decode failed: {e}")),
        },
    };
    if cancel.is_cancelled() {
        return (false, "Export cancelled".to_string());
    }
    let camera_to_working = ferrolite_color::camera_to_working(
        profile.xyz_to_cam,
        ferrolite_color::Xy { x: profile.white_xy[0], y: profile.white_xy[1] },
        working_space,
    );
    let pyramid = Arc::new(GpuPyramidSource::new(gpu, &linear));
    let stack = OpStack::default();
    let mut noop = |_done: u32, _total: u32| {};
    let req = ExportRequest {
        ctx: gpu,
        pyramid: &pyramid,
        stack: &stack,
        camera_to_working,
        working_space,
        options,
        dest: &item.dest,
        source_path: &item.path,
    };
    match run_export(req, cancel, &mut noop) {
        Ok(outcome) => {
            let base = format!("Exported {}", outcome.dest.display());
            let msg = if outcome.warnings.is_empty() {
                base
            } else {
                format!("{base} ({})", outcome.warnings.join("; "))
            };
            (true, msg)
        }
        Err(ferrolite_export::ExportError::Cancelled) => (false, "Export cancelled".to_string()),
        Err(e) => (false, format!("Export failed: {e}")),
    }
}
```
In `ferrolite-app/src/export/mod.rs`, add near the top: `pub mod batch;`.

Note: verify the actual field names on the decoded profile match `source_to_working` in `app.rs` (`profile.xyz_to_cam`, `profile.white_xy: [f32; 2]`) and that `decode_full` returns a struct with `.color_profile`. If `preview_to_linear` is not `pub` in `crate::viewer::load`, make it `pub` (it is already `pub fn preview_to_linear`).

- [ ] **Step 3: Add `AppState.batch` field.** In `ferrolite-app/src/state.rs`, add to the `AppState` struct (near `export_dialog`):
```rust
    /// Aggregate progress + cancel handles for a running batch export (spec §8.4).
    /// `None` when no batch is active.
    pub batch: Option<crate::export::batch::BatchExportState>,
```
Initialize it to `None` in `AppState::new` and in `AppState::for_test` (find both constructors and add `batch: None,`).

- [ ] **Step 4: Run the event test.**

Run: `cargo test -p ferrolite-app events::tests::batch_item_finished_folds_into_aggregate`
Expected: PASS.

- [ ] **Step 5: Build + clippy.**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit.**
```bash
git add ferrolite-app/src/events.rs ferrolite-app/src/export/batch.rs ferrolite-app/src/export/mod.rs ferrolite-app/src/state.rs
git commit -m "feat(app): batch export orchestration + aggregate-progress event fold"
```

---

## Task 5: AppState export-module fields, startup queue load, queue mutation helpers

**Files:**
- Modify: `ferrolite-app/src/state.rs`

**Interfaces:**
- Produces (on `AppState`):
  - Fields: `pub export_queue: Vec<i64>`, `pub export_settings: ferrolite_export::ExportOptions`, `pub export_dest: Option<std::path::PathBuf>`, `pub export_template: String`.
  - `pub fn load_export_queue(&mut self)` — reads the persisted queue via `self.reads.list_export_queue()`; on error logs + sets `warning`, leaves the in-memory queue empty (cache contract).
  - `pub fn queue_add(&mut self, image_id: i64)` — append if absent (in-memory authoritative) + persist via writer; DB error → warning only.
  - `pub fn queue_add_many(&mut self, ids: &[i64])`.
  - `pub fn queue_remove(&mut self, image_id: i64)`.
  - `pub fn queue_clear(&mut self)`.
  - `pub fn queue_move(&mut self, index: usize, delta: isize)` — reorder one row up/down; persist.

- [ ] **Step 1: Add the fields.** In `AppState` (after the `batch` field from Task 4):
```rust
    /// Persisted export queue: ordered image_ids. Authoritative in-memory copy
    /// (the DB table is a cache — its loss never loses photos). Loaded at startup.
    pub export_queue: Vec<i64>,
    /// Shared batch export settings (spec §8.2).
    pub export_settings: ferrolite_export::ExportOptions,
    /// Batch destination folder (spec §8.4). `None` until picked.
    pub export_dest: Option<std::path::PathBuf>,
    /// Filename token template (spec §8.4). Default "{name}".
    pub export_template: String,
```
Initialize in `AppState::new` and `AppState::for_test`:
```rust
            export_queue: Vec::new(),
            export_settings: ferrolite_export::ExportOptions::default(),
            export_dest: None,
            export_template: "{name}".to_string(),
```

- [ ] **Step 2: Add the helpers.** In `impl AppState`, add:
```rust
    /// Load the persisted export queue (spec §8.4). Cache contract: on DB error
    /// keep an empty in-memory queue and surface a warning; never panic.
    pub fn load_export_queue(&mut self) {
        match self.reads.list_export_queue() {
            Ok(ids) => self.export_queue = ids,
            Err(e) => {
                eprintln!("ferrolite: export queue load failed: {e}");
                self.export_queue = Vec::new();
                self.warning = Some("Could not load export queue.".to_string());
            }
        }
    }

    fn now_unix() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Persist a queue write; on error surface a warning but keep the in-memory
    /// queue authoritative (cache contract §5).
    fn persist_queue<F>(&mut self, op: F)
    where
        F: FnOnce(&ferrolite_catalog::Catalog) -> Result<(), ferrolite_catalog::CatalogError>,
    {
        if let Ok(cat) = self.writer.lock() {
            if let Err(e) = op(&cat) {
                eprintln!("ferrolite: export queue persist failed: {e}");
                self.warning = Some("Export queue not saved (kept for this session).".to_string());
            }
        }
    }

    pub fn queue_add(&mut self, image_id: i64) {
        if self.export_queue.contains(&image_id) {
            return;
        }
        self.export_queue.push(image_id);
        let at = Self::now_unix();
        self.persist_queue(|cat| cat.add_to_export_queue(image_id, at));
    }

    pub fn queue_add_many(&mut self, ids: &[i64]) {
        for &id in ids {
            self.queue_add(id);
        }
    }

    pub fn queue_remove(&mut self, image_id: i64) {
        self.export_queue.retain(|&id| id != image_id);
        self.persist_queue(|cat| cat.remove_from_export_queue(image_id));
    }

    pub fn queue_clear(&mut self) {
        self.export_queue.clear();
        self.persist_queue(|cat| cat.clear_export_queue());
    }

    /// Move the row at `index` by `delta` (clamped), then persist the new order.
    pub fn queue_move(&mut self, index: usize, delta: isize) {
        let len = self.export_queue.len();
        if index >= len {
            return;
        }
        let target = (index as isize + delta).clamp(0, len as isize - 1) as usize;
        if target == index {
            return;
        }
        let id = self.export_queue.remove(index);
        self.export_queue.insert(target, id);
        let ordered = self.export_queue.clone();
        self.persist_queue(|cat| cat.reorder_export_queue(&ordered));
    }
```
Note: use the crate's real lock/accessor names. `self.writer` is `Arc<Mutex<Catalog>>` (confirmed in `state.rs`); `self.reads` is `Arc<ReadPool>`.

- [ ] **Step 3: Write failing unit tests for the in-memory queue logic.** In `state.rs`'s test module (there is an existing `#[cfg(test)]` using `AppState::for_test`), add:
```rust
    #[test]
    fn queue_add_dedups_and_preserves_order() {
        let mut s = AppState::for_test();
        s.queue_add(1);
        s.queue_add(2);
        s.queue_add(1); // dup ignored
        assert_eq!(s.export_queue, vec![1, 2]);
    }

    #[test]
    fn queue_move_reorders_and_clamps() {
        let mut s = AppState::for_test();
        s.export_queue = vec![10, 20, 30];
        s.queue_move(0, 1); // 10 down one
        assert_eq!(s.export_queue, vec![20, 10, 30]);
        s.queue_move(0, -5); // clamp: no-op at top
        assert_eq!(s.export_queue, vec![20, 10, 30]);
        s.queue_move(2, 1); // clamp: no-op at bottom
        assert_eq!(s.export_queue, vec![20, 10, 30]);
    }

    #[test]
    fn queue_remove_and_clear() {
        let mut s = AppState::for_test();
        s.export_queue = vec![1, 2, 3];
        s.queue_remove(2);
        assert_eq!(s.export_queue, vec![1, 3]);
        s.queue_clear();
        assert!(s.export_queue.is_empty());
    }
```
Note: `AppState::for_test` must build a working `writer`/`reads` (in-memory or temp). It already exists and is used across the suite; these helpers call `self.writer.lock()` and will persist harmlessly. If `for_test` uses an in-memory catalog whose schema is migrated to v5 (it will be, after Task 1), persistence succeeds silently. If `for_test` has no real DB, `persist_queue` still won't panic (lock/err are handled), so the in-memory assertions hold regardless.

- [ ] **Step 4: Run the tests.**

Run: `cargo test -p ferrolite-app state::`
Expected: PASS (three new queue tests + existing).

- [ ] **Step 5: Commit.**
```bash
git add ferrolite-app/src/state.rs
git commit -m "feat(app): export-module AppState fields + cache-safe queue mutation helpers"
```

---

## Task 6: `Module::Export` variant + third segmented control + dispatch refactor + module shell

**Files:**
- Modify: `ferrolite-app/src/module.rs`
- Modify: `ferrolite-app/src/chrome/mod.rs`
- Create: `ferrolite-app/src/export_module/mod.rs`
- Modify: `ferrolite-app/src/lib.rs` (module declaration)
- Modify: `ferrolite-app/src/app.rs`

**Interfaces:**
- Produces: `Module::Export`; `crate::export_module::show(app-level wiring)` (see below); Export toolbar + empty content shell (queue list + panels filled in Task 7).

- [ ] **Step 1: Add the enum variant.** In `ferrolite-app/src/module.rs`:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Module {
    #[default]
    Library,
    Develop,
    Export,
}

impl Module {
    pub fn is_library(self) -> bool {
        matches!(self, Module::Library)
    }
}
```

- [ ] **Step 2: Add the third segmented-control entry.** In `ferrolite-app/src/chrome/mod.rs`, replace the two-tab block inside the center-group `allocate_new_ui` closure with three explicit tabs, and widen the tabs rect to include "Export".

Change the width calc (`tabs_w`):
```rust
    let tabs_w = text_w("Library")
        + text_w("Develop")
        + text_w("Export")
        + btn_pad * 3.0
        + ui.spacing().item_spacing.x * 2.0;
```
Change the closure body:
```rust
            if ui.selectable_label(*module == Module::Library, "Library").clicked() {
                *module = Module::Library;
            }
            if ui.selectable_label(*module == Module::Develop, "Develop").clicked() {
                *module = Module::Develop;
            }
            if ui.selectable_label(*module == Module::Export, "Export").clicked() {
                *module = Module::Export;
            }
```

- [ ] **Step 3: Create the module shell.** Create `ferrolite-app/src/export_module/mod.rs`:
```rust
//! The Export module (spec §8.4): a third top-level module. Chrome grammar —
//! toolbar (queue summary + Clear) → content row (queue list · settings panel ·
//! bottom bar). Panels are filled in Task 7; this file owns the toolbar + the
//! outbound action enum.

pub mod bottom_bar;
pub mod queue_list;

use crate::state::AppState;

/// Actions the Export module surfaces up to `app.rs` (which owns GPU state).
pub enum ExportModuleAction {
    /// The user hit Start with a chosen destination — run the batch.
    Start,
    /// Cancel the running batch.
    Cancel,
}

/// The 40px Export toolbar: queue count + "Clear queue".
pub fn toolbar(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.label(format!("Export queue — {} image(s)", state.export_queue.len()));
        let running = state.batch.as_ref().is_some_and(|b| !b.is_done());
        ui.add_enabled_ui(!state.export_queue.is_empty() && !running, |ui| {
            if ui.button("Clear queue").clicked() {
                state.queue_clear();
            }
        });
    });
}
```

- [ ] **Step 4: Declare the module.** In `ferrolite-app/src/lib.rs` (or wherever `mod export;`, `mod develop;` etc. are declared), add:
```rust
pub mod export_module;
```

- [ ] **Step 5: Refactor the app dispatch to three-way.** In `ferrolite-app/src/app.rs`:

(a) **Toolbar dispatch** (currently `if self.module.is_library() { toolbar } else { develop filter+filmstrip }` at ~1243): change to a `match`:
```rust
        let mut film_clicked: Option<i64> = None;
        match self.module {
            crate::module::Module::Library => {
                egui::TopBottomPanel::top("toolbar")
                    .exact_height(40.0)
                    .frame(egui::Frame::none().fill(theme::BG_TOOLBAR).inner_margin(egui::Margin::symmetric(10.0, 0.0)))
                    .show(ctx, |ui| {
                        let changed = crate::library::toolbar::show(ui, &mut self.thumb_size, &mut self.state);
                        if changed { self.state.dirty = true; }
                    });
            }
            crate::module::Module::Develop => {
                // ... unchanged develop_filter + develop_filmstrip panels ...
            }
            crate::module::Module::Export => {
                egui::TopBottomPanel::top("export_toolbar")
                    .exact_height(40.0)
                    .frame(egui::Frame::none().fill(theme::BG_TOOLBAR).inner_margin(egui::Margin::symmetric(10.0, 0.0)))
                    .show(ctx, |ui| {
                        crate::export_module::toolbar(ui, &mut self.state);
                    });
            }
        }
```
Move the existing Develop `develop_filter`/`develop_filmstrip` panel code verbatim into the `Develop` arm.

(b) **Left panel guard** (~1316 `if self.module.is_library()`): leave as-is (`is_library()` still returns the right thing — the left catalog panel is Library-only).

(c) **`develop_meta` bottom panel** (~1296 `if self.module == Module::Develop`): unchanged.

(d) **Central panel** (~1602): the current `if self.module.is_library() { grid } else if viewer { develop } else { canvas }` becomes:
```rust
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG_CANVAS))
            .show(ctx, |ui| {
                match self.module {
                    crate::module::Module::Library => {
                        opened = crate::library::grid::show(ui, &mut self.state, self.thumb_size + 60.0);
                    }
                    crate::module::Module::Develop => {
                        if self.state.viewer.is_some() {
                            self.drive_viewer(ui, frame);
                            // ... crop overlay / context menu unchanged ...
                        } else {
                            let rect = ui.available_rect_before_wrap();
                            canvas::paint(ui, rect);
                        }
                    }
                    crate::module::Module::Export => {
                        // Filled in Task 7. For now, an empty canvas placeholder.
                        let rect = ui.available_rect_before_wrap();
                        canvas::paint(ui, rect);
                    }
                }
            });
```
IMPORTANT: The Develop right adjustment `SidePanel::right("develop_adjust")` and the Export right settings panel must be declared BEFORE the `CentralPanel` (egui panel ordering). Keep the existing Develop right-panel block where it is; add the Export right/bottom panels in Task 7 in the same pre-central region, each guarded by `self.module == Module::Export`.

- [ ] **Step 6: Load the queue at startup.** In `app.rs`, find the one-time startup hook (there is a `startup_rescan_done` guard used for the first-frame rescan). Alongside it (or in `AppState::new`'s caller), call `self.state.load_export_queue();` exactly once on the first update frame. Simplest: in the same `if !self.state.startup_rescan_done { ... }` block, add `self.state.load_export_queue();` before setting the flag.

- [ ] **Step 7: Build + clippy.**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean. (Exhaustive `match self.module` means the compiler enforces all three arms everywhere the enum is matched — fix any non-exhaustive match it flags.)

- [ ] **Step 8: Commit.**
```bash
git add ferrolite-app/src/module.rs ferrolite-app/src/chrome/mod.rs ferrolite-app/src/export_module/mod.rs ferrolite-app/src/lib.rs ferrolite-app/src/app.rs
git commit -m "feat(app): Module::Export variant, third segmented-control tab, dispatch refactor + shell"
```

---

## Task 7: Export module content — queue list, settings panel, bottom bar, Start

**Files:**
- Create: `ferrolite-app/src/export_module/queue_list.rs`
- Create: `ferrolite-app/src/export_module/bottom_bar.rs`
- Modify: `ferrolite-app/src/app.rs`

**Interfaces:**
- Produces:
  - `queue_list::show(ui: &mut egui::Ui, state: &mut AppState)` — renders one row per queued image (filename + up/down reorder + remove ×); reorder/remove call the Task-5 helpers.
  - `bottom_bar::show(ui: &mut egui::Ui, state: &mut AppState) -> Option<ExportModuleAction>` — destination-folder picker (rfd), filename-template `TextEdit`, Start/Cancel + aggregate-progress readout.
  - In `app.rs`: `fn start_batch(&mut self, ctx, frame)` — resolves filenames (expand + collision), builds `Vec<BatchItem>`, builds `gpu`, calls `spawn_batch`, stores `BatchExportState`.

- [ ] **Step 1: Queue list widget.** Create `ferrolite-app/src/export_module/queue_list.rs`:
```rust
//! The Export queue list (spec §8.4): filename rows with reorder + remove.
//! Read-only image metadata is fetched via the read pool by id.

use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    if state.export_queue.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(egui::RichText::new("Queue is empty.\nAdd images from Library or Develop.").color(crate::theme::TEXT_FAINT));
        });
        return;
    }
    // Resolve filenames for display (id → basename). Missing ids show the id.
    let ids = state.export_queue.clone();
    let recs = state.reads.images_by_ids(&ids).unwrap_or_default();
    let name_of = |id: i64| -> String {
        recs.iter()
            .find(|r| r.id == id)
            .map(|r| r.filename.clone())
            .unwrap_or_else(|| format!("#{id}"))
    };

    let running = state.batch.as_ref().is_some_and(|b| !b.is_done());
    let mut do_move: Option<(usize, isize)> = None;
    let mut do_remove: Option<i64> = None;

    egui::ScrollArea::vertical().show(ui, |ui| {
        for (idx, &id) in ids.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("{:>3}.", idx + 1));
                ui.label(name_of(id));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!running, |ui| {
                        if ui.small_button("✕").clicked() {
                            do_remove = Some(id);
                        }
                        if ui.small_button("▼").clicked() {
                            do_move = Some((idx, 1));
                        }
                        if ui.small_button("▲").clicked() {
                            do_move = Some((idx, -1));
                        }
                    });
                });
            });
        }
    });

    if let Some((idx, delta)) = do_move {
        state.queue_move(idx, delta);
    }
    if let Some(id) = do_remove {
        state.queue_remove(id);
    }
}
```

- [ ] **Step 2: Bottom bar.** Create `ferrolite-app/src/export_module/bottom_bar.rs`:
```rust
//! Export module bottom bar (spec §8.4): destination folder + filename template
//! + Start/Cancel + aggregate progress.

use crate::export_module::ExportModuleAction;
use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) -> Option<ExportModuleAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        if ui.button("Destination folder…").clicked() {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                state.export_dest = Some(dir);
            }
        }
        match &state.export_dest {
            Some(d) => ui.monospace(d.display().to_string()),
            None => ui.colored_label(crate::theme::TEXT_FAINT, "(no folder chosen)"),
        };
    });
    ui.horizontal(|ui| {
        ui.label("Filename");
        ui.add(
            egui::TextEdit::singleline(&mut state.export_template)
                .hint_text("{name}")
                .desired_width(220.0),
        );
        ui.colored_label(crate::theme::TEXT_FAINT, "tokens: {name} {seq:03} {date} {camera}");
    });
    ui.horizontal(|ui| {
        let running = state.batch.as_ref().is_some_and(|b| !b.is_done());
        let can_start = !running
            && !state.export_queue.is_empty()
            && state.export_dest.is_some()
            && !state.export_template.trim().is_empty();
        ui.add_enabled_ui(can_start, |ui| {
            if ui.button("Start export").clicked() {
                action = Some(ExportModuleAction::Start);
            }
        });
        if running && ui.button("Cancel").clicked() {
            action = Some(ExportModuleAction::Cancel);
        }
        if let Some(b) = state.batch.as_ref() {
            let msg = if b.is_done() {
                format!("Done — {} exported, {} failed", b.completed - b.failed, b.failed)
            } else {
                format!("Exporting {}/{} ({} failed)", b.completed, b.total, b.failed)
            };
            ui.label(msg);
        }
    });
    action
}
```

- [ ] **Step 3: Wire the panels + Start into `app.rs`.** In the pre-`CentralPanel` region (where the Develop right panel is declared), add Export-guarded panels:
```rust
        if self.module == crate::module::Module::Export {
            egui::TopBottomPanel::bottom("export_bottom")
                .frame(egui::Frame::none().fill(theme::BG_TOOLBAR).inner_margin(egui::Margin::symmetric(12.0, 8.0)))
                .show(ctx, |ui| {
                    if let Some(a) = crate::export_module::bottom_bar::show(ui, &mut self.state) {
                        match a {
                            crate::export_module::ExportModuleAction::Start => self.start_batch(ctx, frame),
                            crate::export_module::ExportModuleAction::Cancel => {
                                if let Some(b) = self.state.batch.as_ref() { b.cancel_all(); }
                            }
                        }
                    }
                });
            egui::SidePanel::right("export_settings")
                .resizable(true)
                .default_width(296.0)
                .width_range(250.0..=400.0)
                .frame(egui::Frame::none().fill(theme::BG_APP).inner_margin(egui::Margin::symmetric(12.0, 8.0)))
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new("EXPORT SETTINGS").small().color(theme::TEXT_FAINT));
                    ui.add_space(6.0);
                    crate::export::settings_form::settings_form(ui, &mut self.state.export_settings);
                });
        }
```
And extend the central `match self.module` `Module::Export` arm (from Task 6) to render the queue list:
```rust
                    crate::module::Module::Export => {
                        crate::export_module::queue_list::show(ui, &mut self.state);
                    }
```

- [ ] **Step 4: Implement `start_batch`.** Add to the app `impl` (near `confirm_export`):
```rust
    /// Resolve output filenames and spawn one Background export job per queued
    /// image (spec §8.4). Filenames are expanded + collision-resolved up front on
    /// the UI thread so {seq} is deterministic and disk collisions are avoided.
    fn start_batch(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(dest_dir) = self.state.export_dest.clone() else {
            self.state.warning = Some("Choose a destination folder first.".to_string());
            return;
        };
        let ids = self.state.export_queue.clone();
        if ids.is_empty() {
            return;
        }
        // Metadata for {name}/{date}/{camera}.
        let recs = self.state.reads.images_by_ids(&ids).unwrap_or_default();
        let options = self.state.export_settings;
        let template = self.state.export_template.clone();
        let ext = options.format.extension();

        // Seed collision set with files already on disk in the destination.
        let mut taken: std::collections::HashSet<String> = std::fs::read_dir(&dest_dir)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string())).collect())
            .unwrap_or_default();

        let mut items: Vec<crate::export_module::_batch_item::BatchItem> = Vec::new(); // see note
        let mut seq = 0usize;
        for &id in &ids {
            let Some(rec) = recs.iter().find(|r| r.id == id) else { continue };
            seq += 1;
            let path = self.state.image_path(rec); // resolve absolute path (see note)
            let stem = std::path::Path::new(&rec.filename)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| rec.filename.clone());
            let camera = format!("{} {}", rec.make.clone().unwrap_or_default(), rec.model.clone().unwrap_or_default())
                .trim()
                .to_string();
            let fctx = ferrolite_export::FilenameCtx {
                name: stem,
                seq,
                date: ferrolite_export::format_capture_date(rec.capture_time.as_deref()),
                camera,
            };
            let expanded = ferrolite_export::expand_filename(&template, &fctx);
            let filename = ferrolite_export::resolve_collision(&expanded, ext, &mut taken);
            items.push(ferrolite_export::batch_item_placeholder()); // replaced below
            let _ = (path, filename); // see note
        }

        let Some(rs) = frame.wgpu_render_state() else {
            self.state.warning = Some("No GPU render state; cannot export.".to_string());
            return;
        };
        let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let working_space = self.state.working_space;

        let handles = crate::export::batch::spawn_batch(&self.state, ctx, gpu, items, working_space, options);
        let total = handles.len();
        let mut bs = crate::export::batch::BatchExportState::new(total);
        bs.handles = handles;
        self.state.batch = Some(bs);
        self.state.warning = Some(format!("Exporting {total} image(s)…"));
    }
```
NOTE (resolve two real details when implementing):
1. **`BatchItem` construction** — the placeholder lines above are illustrative. Build real `crate::export::batch::BatchItem { image_id: id, path, kind: rec.kind, dest: dest_dir.join(&filename) }` inside the loop and push that. Remove the `_placeholder`/`_batch_item` references.
2. **Absolute path** — `ImageRecord` stores `folder_id` + `filename`, not an absolute path. Reuse the exact same path-resolution the viewer-open path uses (`state.rs` `open_image_in_viewer` builds `path` from the record — grep it and reuse that helper; expose it as `pub fn image_path(&self, rec: &ImageRecord) -> PathBuf` on `AppState` if not already present). `kind` is `rec.kind` (`FileKind`).

- [ ] **Step 5: Build + clippy.**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean after resolving the two NOTE items (real `BatchItem` + path helper).

- [ ] **Step 6: Workspace test + fmt.**

Run: `cargo fmt --all && cargo test --workspace`
Expected: PASS across the workspace.

- [ ] **Step 7: Commit.**
```bash
git add ferrolite-app/src/export_module ferrolite-app/src/app.rs
git commit -m "feat(app): Export module UI — queue list, settings panel, destination + template + Start"
```

---

## Task 8: Add-to-queue actions (Library multi-select + Develop current image)

**Files:**
- Modify: `ferrolite-app/src/library/image_context_menu.rs`
- Modify: `ferrolite-app/src/chrome/mod.rs`
- Modify: `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `AppState::queue_add`, `AppState::queue_add_many` (Task 5); `MenuAction` (chrome).
- Produces: `MenuAction::AddToQueue`.

- [ ] **Step 1: Library context-menu item.** In `ferrolite-app/src/library/image_context_menu.rs`, add near the end of `show` (after the "Add to collection" block):
```rust
    ui.separator();
    if ui.button("Add to export queue").clicked() {
        if use_selection {
            let ids: Vec<i64> = state.selection.iter().copied().collect();
            state.queue_add_many(&ids);
        } else {
            state.queue_add(image_id);
        }
        ui.close_menu();
    }
```
(`use_selection` and `image_id` are already in scope from the top of `show`.)

- [ ] **Step 2: Add the menu action variant.** In `ferrolite-app/src/chrome/mod.rs`, extend `MenuAction`:
```rust
pub enum MenuAction {
    ExportImage,
    AddToQueue,
}
```
Add a Photo-menu item (below "Export…"), enabled when a viewer image is open — reuse the existing `export_enabled` flag (it is already gated on a viewer image having a full-res source; for "add to queue" we only need a viewer image, but reusing `export_enabled` is acceptable and keeps the signature unchanged):
```rust
            ui.menu_button("Photo", |ui| {
                if ui.add_enabled(export_enabled, egui::Button::new("Export…")).clicked() {
                    action = Some(MenuAction::ExportImage);
                    ui.close_menu();
                }
                if ui.add_enabled(export_enabled, egui::Button::new("Add to export queue")).clicked() {
                    action = Some(MenuAction::AddToQueue);
                    ui.close_menu();
                }
            });
```

- [ ] **Step 3: Handle the action in `app.rs`.** Where `MenuAction::ExportImage` is handled (~1237), extend:
```rust
                match menu_action {
                    Some(crate::chrome::MenuAction::ExportImage) => self.open_export_dialog(),
                    Some(crate::chrome::MenuAction::AddToQueue) => {
                        if let Some(id) = self.state.viewer.as_ref().map(|v| v.image_id) {
                            self.state.queue_add(id);
                            self.state.warning = Some("Added to export queue.".to_string());
                        }
                    }
                    None => {}
                }
```
(Adjust from the existing `if menu_action == Some(...)` form to a `match` — the current code compares with `==`; replace it with the `match` above.)

- [ ] **Step 4: Build + clippy + test.**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app`
Expected: clean + PASS.

- [ ] **Step 5: Commit.**
```bash
git add ferrolite-app/src/library/image_context_menu.rs ferrolite-app/src/chrome/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(app): add-to-export-queue from Library multi-select + Develop Photo menu"
```

---

## Task 9: Design-system doc — two modules → three

**Files:**
- Modify: `docs/design/ferrolite-design-system.md`

- [ ] **Step 1: §1 Visual direction.** Change "Two top-level modules switched by a segmented control in the title bar:" to "Three top-level modules …" and add a third bullet after Develop:
```markdown
- **Export** — batch queue: collected images, a shared export-settings panel, a
  destination folder, a filename token template, and Start. → **Spec 3**.
```

- [ ] **Step 2: §4 Layout & metrics.** Under the App-shell description, add an Export content-row line after the Develop one:
```markdown
- Export content row: `center queue list (flex)` | `right (296: shared export settings)`; a bottom bar (destination folder · filename token template · Start). Panels resizable (296px default, 250–400 clamp), matching Develop.
```

- [ ] **Step 3: §6 Component inventory.** Add rows to the mapping table:
```markdown
| Export segmented tab | third `SelectableLabel`, accent bg | 3 |
| Export queue list | rows (filename + reorder ▲▼ + remove ✕) in `ScrollArea` | 3 |
| Export settings panel | resizable `SidePanel::right`; shared `settings_form` | 3 |
| Export bottom bar | folder picker + template `TextEdit` + Start | 3 |
```

- [ ] **Step 4: §7 Binding.** Add a sentence: "The **Export module** is a Spec 3 UI target: its queue is the `export_queue` catalog cache and Start dispatches one `ferrolite-export` Background job per image."

- [ ] **Step 5: Commit.**
```bash
git add docs/design/ferrolite-design-system.md
git commit -m "docs(design-system): document the third (Export) module"
```

---

## Final gate (before holding for the author's visual test)

- [ ] **Step 1: Format check.** Run: `cargo fmt --check` — Expected: no diff.
- [ ] **Step 2: Clippy.** Run: `cargo clippy --workspace --all-targets -- -D warnings` — Expected: clean.
- [ ] **Step 3: Tests.** Run: `cargo test --workspace` — Expected: green (GPU goldens auto-skip headless).
- [ ] **Step 4: STOP and hold for Jann.** Per CLAUDE.md, the green gate is necessary but **not sufficient**. Present the finish options, then **hold** for Jann's hands-on visual test of the running app:
  - Export tab appears in the title bar; switching to it shows the queue/settings/bottom-bar layout.
  - Library right-click "Add to export queue" adds single + multi-selection; Develop Photo → "Add to export queue" adds the open image; queue persists across an app restart.
  - Pick a destination, set a template (e.g. `{name}_{seq:03}`), Start → files land in the folder with expanded names + collision suffixes; aggregate progress advances; Cancel stops remaining jobs; the single-file Photo → Export dialog is unchanged.
  Address any issues Jann finds before finishing the branch.

---

## Self-Review (checked against the spec)

**Spec coverage (§8.4 + §12 item 5):**
- `Module::Export` + third segmented-control entry → Task 6. ✓
- Chrome grammar toolbar → content → Task 6 (toolbar) + Task 7 (content panels). ✓
- `export_queue` table + repository (add/remove/list/clear/reorder, persisted, cache) → Task 1. ✓
- Add-to-queue from Library multi-select + Develop → Task 8. ✓
- Export module UI: queue list + shared resizable settings panel (§8.2) + bottom destination picker + filename template + Start → Task 7 (+ shared form in Task 3). ✓
- Pure tested filename expander ({name},{seq:03},{date},{camera} + literals, collision auto-suffix) → Task 2. ✓
- Batch orchestration: one Background job per image, aggregate progress + cancellation → Tasks 4 + 7. ✓
- Design-system doc two → three → Task 9. ✓
- §5 cache contract (queue loss never loses photos) → Task 1 (FK cascade, table-is-cache comment) + Task 5 (in-memory authoritative, DB errors → warning only). ✓
- §9 resizable side panels — **already delivered in Plan 2** (both existing panels are `.resizable(true)` with width ranges); the new Export settings panel ships resizable (Task 7). Not re-done here. ✓

**Placeholder scan:** the only intentional "resolve on implementation" markers are the two NOTE items in Task 7 Step 4 (real `BatchItem` construction + absolute-path helper), which are explicit and bounded — reuse the viewer-open path helper. No "TODO/handle edge cases" left.

**Type consistency:** `AppState.export_queue: Vec<i64>` used consistently across Tasks 5–8; `BatchExportState`/`BatchItem` signatures match between Task 4 (definition) and Task 7 (use); `ExportModuleAction` defined in Task 6, used in Task 7; `MenuAction::AddToQueue` defined + handled in Task 8; `settings_form` defined in Task 3, reused in Task 7. `expand_filename`/`resolve_collision`/`format_capture_date`/`FilenameCtx` re-export names (Task 2) match their call sites (Task 7).
