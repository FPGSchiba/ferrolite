# ferrolite — Spec 1 / Plan 3.5: Library Ingest & Folder Management (design)

> **Status:** Design — pending user review, then writing-plans.
> **Date:** 2026-06-29
> **Parent specs:** `2026-06-28-ferrolite-v1-architecture-map.md` (settled decisions +
> cross-cutting interface contracts), `2026-06-28-ferrolite-speed-core-design.md`
> (Spec 1 design), and `2026-06-29-jobs-and-library-grid-design.md` (Plan 3).
> **Position:** an **interim plan ("Plan 3.5")** between Plan 3 (Jobs & Library grid)
> and Plan 4 (Viewer & VT). It makes the Library usable on a real, mixed, nested library.
> **Proves:** extends G1 (browse) to real-world libraries — nested folders + mixed formats.
> **UI target:** the **Library module** of the design system
> (`docs/design/ferrolite-design-system.md`).

---

## 1. Goal & position in the sequence

Plan 3 delivered a felt-fast Library browser over a **single, flat, RAW-only** folder. A real
photo library is **nested** and **mixed** (RAW alongside JPEG/PNG/TIFF/…). This interim plan closes
that gap with four scoped items so the next agent picking up **Plan 4 (Viewer & VT)** inherits a
Library that browses an actual on-disk tree:

1. **More RAW formats** — extend the `RAW_EXTS` list in `scan.rs`.
2. **Standard raster ingest** — JPEG/PNG/TIFF/WebP/BMP/GIF via the `image` crate (already a dep),
   EXIF via `kamadak-exif` (new permissive dep). A `FileKind {Raw, Standard}` classifier and a
   non-RAW decode route, reusing `apply_orientation`.
3. **Recursive subfolders (Model B — a proper folder tree)** — walk the whole subtree; every real
   directory becomes a `folders` row with `parent_id` wired; each image keyed to its **actual**
   directory. Left panel becomes an indented, expandable tree with roll-up counts; an
   "include subfolders" view via a `WITH RECURSIVE` CTE over `parent_id`.
4. **Remove folder** — `Catalog::remove_folder` (thumbnails → images → folder rows, recursively
   over a subtree) plus a remove affordance on left-panel rows, with a ReadPool-side refresh.

**Out of scope (deferred):** HEIC/AVIF/JPEG-XL **input** (C-library deps — note `ravif`/`jpegxl-rs`
remain *export* encoders in Spec 3, not input here). GPU context, virtual texture, single-image
viewer, two-tier load (Plan 4). Edit/color/export (Specs 2–3). RAW-only / standard-only grid
**filters** (would justify a persisted `kind` column; YAGNI now).

---

## 2. Cross-cutting contracts honored (architecture map §5)

- **Jobs stay photo-agnostic.** All recursion/format logic lives in `scan`/`decode`/`catalog`/`app`;
  `ferrolite-jobs` is untouched. Ingest remains an Interactive cancellable walk job fanning out
  Background thumbnail jobs (Plan 3 §3, §5).
- **WAL writer + ReadPool unchanged.** New writes (`upsert_folder` with parent, `remove_folder`) go
  through the single `Catalog` writer; new reads (`list_images_recursive`, `list_folders` with
  `parent_id`) are served from the `ReadPool`.
- **Catalog is a rebuildable cache.** `remove_folder` deletes **catalog rows only — never files on
  disk**. A removed folder is re-ingested by re-opening it. No schema migration is required (§3).
- **Decode yields format-agnostic products.** Both routes return the same `ImageBuffer` / `Metadata`
  vocabulary, so the catalog, jobs, grid, thumbnail store, and (future) viewer are unchanged by the
  addition of standard-raster input.

---

## 3. Schema: no migration required

The Plan 2 schema (`schema.rs`, `SCHEMA_VERSION = 1`) already provides everything needed:

- `folders.parent_id INTEGER` — exists, currently always `NULL`. This plan finally **wires it**.
- `images … UNIQUE(folder_id, filename)` — already the correct key for **per-directory** images, so
  the same filename in two sibling subfolders (e.g. `2024/IMG_001.JPG` and `2025/IMG_001.JPG`) keys
  to two different `folder_id`s and never collides.

**`FileKind` is derived from the file extension, not persisted.** A `kind` column would only pay off
once we add RAW-only/standard-only grid filters, which is out of scope. Decode routing re-derives the
kind from the extension at decode time (free). Therefore **`SCHEMA_VERSION` stays `1`; this plan adds
no migration block.** (When a future plan adds format filters, bump to 2 and backfill `kind`.)

---

## 4. Formats & the non-RAW decode route (items 1 & 2)

### 4.1 `FileKind` placement (avoids a dependency cycle)

`scan.rs` lives in `ferrolite-catalog`; the decode route also needs the kind; and `catalog` depends
on `decode` (never the reverse). So:

- **`FileKind { Raw, Standard }` is defined in `ferrolite-decode`** (it selects the decode route).
- **The extension lists + `classify(path) -> Option<FileKind>` live in `scan.rs`**, returning
  `ferrolite_decode::FileKind`. This keeps the format classifier in `scan` (item 2) with no cycle.

### 4.2 `scan.rs` changes

- **Extend `RAW_EXTS`** (item 1). Current list:
  `nef nrw cr2 cr3 crw arw sr2 srf raf rw2 orf pef dng raw rwl iiq 3fr erf mef mos kdc dcr`.
  Add commonly-seen RAW extensions still missing, e.g.: `srw` (Samsung), `x3f` (Sigma Foveon),
  `gpr` (GoPro), `dcs` `dcr` (Kodak — `dcr` present), `cap` `iiq` (Phase One — `iiq` present),
  `fff` `3fr` (Hasselblad — `3fr` present), `rwz` (Rawzor), `bay` (Casio), `cs1` `ia` (Sinar),
  `ari` (Arri), `braw`/`r3d` **excluded** (video). The exact final set is finalized in the plan;
  failed decodes degrade to a `Failed` row, so over-inclusion is low-risk.
- **Add `STANDARD_EXTS`**: `jpg jpeg png tif tiff webp bmp gif`.
- **`classify(path) -> Option<FileKind>`**: RAW first, then standard, else `None` (file skipped).
  Case-insensitive (lowercased extension), mirroring `is_raw`. `is_raw` is retained for back-compat.
- The scanned-file struct carries `kind: FileKind`. Recursive walk: see §5.1.

### 4.3 `ferrolite-decode` changes

- **`orient` module:** move `apply_orientation` out of `preview.rs` into `orient.rs` as `pub(crate)`,
  reused by both the RAW and standard preview paths (no behavior change).
- **`standard` module:**
  - `decode_preview_standard(path) -> Result<ImageBuffer, DecodeError>`: `image::open` →
    read EXIF orientation via `kamadak-exif` → `apply_orientation` → `to_rgb8` → `ImageBuffer`.
    Decoding full-size then letting `fast_image_resize` downscale to the 256px thumbnail is
    acceptable (no partial-decode optimization this plan).
  - `read_metadata_standard(path) -> Result<Metadata, DecodeError>`: dimensions via the cheap
    header read (`image::image_dimensions`); `make`/`model`/`iso`/`aperture`/`shutter`/
    `focal_length`/`capture_time`/`lens`/`orientation` from `kamadak-exif` when present. Formats
    without EXIF (PNG/BMP/GIF) yield `None` for optional fields and **empty `String`** for
    `make`/`model` (the `Metadata` struct keeps those as `String`). Dimensions are always present.
- **Kind-dispatching entry points:** `decode_preview(path, kind)` and `read_metadata(path, kind)`
  match on `FileKind` and delegate to the RAW (rawler) or standard route. Existing RAW logic moves
  behind the `Raw` arm unchanged.

### 4.4 Cargo

- Add **`kamadak-exif`** (imported as `exif`; MIT/BSD-2 — permissive, GPL-compatible) to
  `[workspace.dependencies]` and to `ferrolite-decode`.
- Enable `image` decoders on `ferrolite-decode`'s dependency (workspace pins
  `default-features = false`): `features = ["jpeg", "png", "tiff", "webp", "bmp", "gif"]`.
- No new deps in `catalog`/`app`/`jobs`.

---

## 5. Recursive folder tree in the catalog (item 3)

### 5.1 Recursive scan

`scan_tree(root) -> Vec<ScannedDir>` (or an equivalent flat list tagging each file with its parent
directory) walks the entire subtree (no `max_depth`). For each entry it records the directory and,
for each supported file, a `ScannedFile { path, filename, mtime, size, kind }`. The existing depth-1
`scan_raw_files` may remain for back-compat/tests or be re-expressed in terms of the new walk.
Cancellation is polled **between directories and between files** so a folder switch abandons the walk
promptly (Plan 3 §3 cooperative cancellation).

### 5.2 Folder-row creation — every real directory becomes a row

Walking top-down, maintain a `path -> folder_id` map:

- For each directory `d`: `folder_id = upsert_folder(d, parent_id = map.get(d.parent()))`; the opened
  root's parent is `NULL` (unless it is already a descendant of a known folder). Insert into the map.
- For each image: `folder_id = map[file.parent()]`, then `upsert_image` keyed `(folder_id, filename)`.

This mirrors the filesystem exactly (no "compressed" tree that skips image-less intermediates), so
`parent_id` always points to the immediate parent and roll-up counts aggregate naturally.

### 5.3 Catalog API

- **`upsert_folder(path, parent_id: Option<i64>)`** — gains the parent argument; on conflict it keeps
  the existing row (and may update `parent_id`). All callers updated (app ingest, catalog
  `ingest_folder`).
- **`remove_folder(folder_id) -> Result<(), CatalogError>`** (item 4): gather the descendant folder
  set via `WITH RECURSIVE`, then in **one transaction** delete in FK-safe order:
  `thumbnails` (by `image_id` in the subtree's images) → `images` (by `folder_id` in subtree) →
  `folders` (the subtree). FK enforcement stays off (as today); the ordered delete is sufficient and
  needs no schema change.
- **`ingest_folder`** (the catalog's synchronous path) becomes recursive too, for parity with the
  app's job-driven ingest; its staging (walk → upsert rows → thumbnails) is preserved.

### 5.4 Queries

- **`list_images_recursive(conn, folder_id)`**:
  ```sql
  WITH RECURSIVE subtree(id) AS (
      SELECT id FROM folders WHERE id = ?1
      UNION ALL
      SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id
  )
  SELECT <IMAGE_COLS> FROM images
  WHERE folder_id IN (SELECT id FROM subtree)
  ORDER BY filename;
  ```
- **`list_folders`** additionally selects `parent_id`; it keeps returning **direct** image counts
  (the existing `GROUP BY` + `LEFT JOIN`). `FolderRecord` gains `parent_id: Option<i64>`.
- **Roll-up counts are computed in-app** from the flat folder list (children map + post-order sum) —
  a few hundred folders at most, trivial, and it avoids a second recursive query each frame.

---

## 6. App: tree UI, subfolder toggle, remove dialog (item 4 + UX)

### 6.1 Left-panel folder tree

`panel.rs` replaces the flat list with an indented, expandable tree built from the flat
`Vec<FolderRecord>`:

- Build a children map and roots (rows whose `parent_id` is `NULL` or points outside the set);
  compute roll-up counts by post-order sum of direct counts.
- Each row renders: an expand/collapse triangle for nodes with children (the expanded set is
  persisted in `AppState`, e.g. `expanded_folders: HashSet<i64>`); a `selectable_label`
  `"{leaf_name}  ({rollup})"` — **roll-up count only** (resolved UX decision); the full path as a
  hover tooltip; and a remove affordance (✕ on hover **and** a right-click "Remove" context menu).
- Selecting a folder calls `select_folder(id)` (existing reset + set current).

### 6.2 Include-subfolders toggle

- `AppState.include_subfolders: bool`, **default `true`** (resolved UX decision).
- A toggle in the top toolbar flips it. `refresh_images` selects `list_images_recursive` when `true`,
  else `list_images`. Flipping the toggle or selecting a folder sets `dirty` so the grid reloads on
  the next frame (idle frames still issue zero queries — Plan 3 dirty-flag discipline).

### 6.3 Remove flow (resolved UX: confirm dialog for subtrees)

- A **leaf** folder (no children) removes **immediately**.
- A folder **with subfolders** opens a confirmation modal naming the folder and its subtree image
  count before deleting. State: `AppState.pending_remove: Option<{ id, name, subtree_count }>`;
  the modal is an `egui::Window` rendered from `app.rs`/panel, gated on `pending_remove.is_some()`.
- On confirm: if `current_folder` is inside the removed subtree, call `reset_for_new_folder()` first
  (cancels that folder's ingest + thumbnail jobs, clears selection); then `writer.remove_folder(id)`
  (a fast write on the UI thread under the writer lock); then reload folders and set `dirty`.
- Removal deletes **catalog rows only — never disk files**. Re-open the folder to re-ingest.
- *Known benign edge:* a Background thumbnail job for an unrelated concurrently-ingesting folder is
  unaffected; a pending thumbnail for a just-removed image simply has nowhere consistent to land and
  is harmless against a rebuildable cache. The common case (removing the folder you're viewing) is
  handled by the `reset_for_new_folder()` cancel above.

### 6.4 Status bar

"N indexed" (global `image_count`), jobs activity (`ProgressSink`), and the static "GPU: idle" slot
are unchanged.

---

## 7. Error handling (continues Plan 3 §7)

- Unsupported/corrupt input (RAW or standard) → decode/thumbnail job errors → `decode_status =
  Failed` → grid shows the broken placeholder. Never panics. (Standard decode failures route through
  the same `Failed` path as RAW.)
- Folder vanished / permission denied mid-walk → job error surfaced in the status bar; partial rows
  already written are kept (cache, not transaction-of-record).
- `remove_folder` runs in a transaction: a failure rolls back, leaving the catalog consistent.
- Catalog corruption / schema mismatch → rebuild by re-ingest (cache invariant; Plan 2 migrations).

---

## 8. Testing (continues Plan 3 §8; 80%+ on non-GPU logic — no GPU here)

- **scan:** `classify()` returns Raw/Standard/None correctly and case-insensitively; `scan_tree`
  finds nested files, records every directory, tags each file's `kind`, and ignores unsupported
  extensions.
- **decode (standard):** generate a tiny PNG and JPEG in-test (via the `image` crate) →
  `read_metadata_standard` reports correct dimensions and empty `make`; `decode_preview_standard`
  returns an upright `ImageBuffer`; `apply_orientation` shared path exercised.
- **catalog:** recursive ingest wires `parent_id` and keys images to their actual directories;
  **duplicate filenames in sibling subfolders both ingest** (no `UNIQUE` collision);
  `list_images_recursive` returns the subtree union while `list_images` returns direct-only;
  `remove_folder` deletes the subtree (thumbnails + images + folders) and leaves siblings intact —
  tested for both a leaf and a multi-level subtree; transaction rollback on injected failure.
- **app (pure functions, no egui):** tree build + roll-up sum from a flat folder list;
  `include_subfolders` selects the correct query path; remove/confirm state machine
  (`pending_remove` set → confirm → cleared); selection reset when the current folder is removed.

---

## 9. Plan shape (for writing-plans)

One cohesive plan, ~5 phased, independently-testable tasks (suits subagent-driven development):

1. **Formats & scan** — extend `RAW_EXTS`, add `STANDARD_EXTS` + `classify`, `FileKind` in decode,
   recursive `scan_tree`; unit tests.
2. **Standard decode route** — `orient` module, `standard` module, kind-dispatching entry points,
   Cargo (`kamadak-exif` + `image` features); unit tests.
3. **Catalog tree** — `upsert_folder(parent_id)`, recursive ingest, `list_images_recursive`,
   `list_folders` + `parent_id`, `remove_folder`; integration tests.
4. **App tree UI** — left-panel tree + roll-up, expand/collapse state, include-subfolders toggle,
   remove affordance + confirm dialog, ReadPool-side refresh; pure-function tests.
5. **Wiring & gate** — end-to-end ingest of a nested mixed fixture, `reset_for_new_folder` on remove,
   `cargo fmt` + `cargo clippy -D warnings` workspace gate, coverage check.

---

## 10. Deliverable

Open a real, deeply-nested, mixed-format folder and browse it: every directory appears as an
indented tree node with roll-up counts; selecting a folder shows its subtree (toggle to direct-only);
RAW and standard rasters thumbnail through the same format-agnostic pipeline; remove a folder
(catalog-only, with a confirm for subtrees) and watch the tree refresh — all off the UI thread,
honoring the architecture-map contracts, with **no schema migration**.
