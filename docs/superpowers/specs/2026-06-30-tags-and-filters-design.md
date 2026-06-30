# ferrolite — Spec 1.5: Tags & Filters (design)

> **Status:** Design — approved by user; pending writing-plans.
> **Date:** 2026-06-30
> **Parent:** `2026-06-28-ferrolite-v1-architecture-map.md` (read first for settled decisions
> and cross-cutting interface contracts) and `2026-06-28-ferrolite-speed-core-design.md`
> (Spec 1, §8 Library module — the toolbar this spec un-stubs).
> **Proves:** completes the **Library module** — a rating/flag/tag + collections model with
> the full toolbar (search, sort, rating/flag/tag filters, metadata-range popover) wired live.
> **Builds on:** Spec 1 (merged) — catalog, ingest, decode, jobs, Library grid, viewer.
> **Out of scope:** all edit ops, the Develop module, the edit op-stack (Spec 2); color
> management, export (Spec 3). No engine-tier crates (`ferrolite-gpu`, `ferrolite-vt`) are
> touched — this is a **Library-only** slice.

---

## 1. Goal

The Library module shipped in Spec 1 with a fully *stubbed* toolbar: the search field, Sort
combo, star filter, and Metadata popover are all `add_enabled(false, …)` visual placeholders,
and there is no rating/flag/tag/collection model behind them. This slice makes the Library you
already browse with **usable for organizing**: rate, flag, and tag photos; group them into
collections; and search/sort/filter across folders.

> rate / flag / tag photos → organize into collections → search, sort, and filter the catalog
> (including across multiple folders) through the now-live Library toolbar.

This is an independent slice with no dependency on Spec 2; it is built first because it is
smaller and completes the surface in daily use.

---

## 2. Scope

**In:**
- `ferrolite-image` — shared value types: `Rating`, `Flag`, `Color`, `TagId` (pure data).
- `ferrolite-catalog` — schema v3 migration; tags/collections/flag persistence (**SQLite is the
  source of truth**); `xmp:Rating` sidecar read/write (the only XMP I/O in this slice); the
  compiled `LibraryQuery` filter/sort/search layer over the existing read pool.
- `ferrolite-app` — live toolbar (search, sort, rating/flag/tag filters, metadata popover),
  tag manager, collections in the left panel, grid overlays (stars/flag/tag dots), and the
  metadata-edit commands (keyboard + multi-select batch apply).

**Out (later specs):** edit ops, the Develop module, the Spec-2 edit op-stack (the
`ferrolite:Edits` XMP block is **reserved but unused** here); color management; export;
exporting tags/collections to disk for interop or backup (a future nicety, see §4).

---

## 3. The metadata axes & their source of truth

| Axis | Values | Source of truth | Queryable mirror | Interop now |
|---|---|---|---|---|
| **Rating** | 0–5 stars | `xmp:Rating` in `<image>.xmp` | `images.rating` column | ✅ Lr/darktable/Bridge |
| **Flag** | None / Pick / Reject | **SQLite** (`images.flag`) | — | — |
| **Tags** | many per image; each has a name + color, global vocabulary | **SQLite** (`tags` + `image_tags`) | — | — |
| **Collections** | named, colored, ordered membership | **SQLite** (`collections` + `collection_images`) | — | — |

The classic single fixed color-label is **dropped** — the global colored-tag vocabulary
replaces it. The unused `images.label` column is retired in the v3 migration.

### §5.2 amendment (explicit, user-approved)

Architecture-map §5.2 declares the catalog a pure cache, rebuildable by re-walking the
filesystem. That holds for **file-derived data** (folders, images, thumbnails, and
**rating**, which is read back from `xmp:Rating`). It does **not** hold for **tags,
collections, and flags**: those are catalog-native and **SQLite is their source of truth**.
This is a deliberate carve-out, accepted with its consequence — the catalog DB holds the only
copy of that organizing work, so it is *precious* rather than disposable. An export/backup
path (e.g. to XMP keywords / JSON) is a future nicety and **out of scope** here. Spec 2 will
extend the XMP layer with the edit op-stack and may, at that point, also mirror tag keywords
to `dc:subject` for interop; this spec does not.

---

## 4. Ratings via XMP sidecar (`ferrolite-catalog`)

Per architecture-map §3, `ferrolite-catalog` owns sidecar I/O. In this slice the sidecar
carries **only `xmp:Rating`**.

- **New module `sidecar/xmp.rs`**, using `quick-xml` (new dependency; permissive MIT/Apache,
  photo-tier crate so no licensing concern).
- **Read (on ingest):** if `<image>.xmp` exists, parse it leniently — extract `xmp:Rating`
  (attribute or element form), ignore everything else; absent/malformed → rating `0`. The XMP
  value is authoritative: on rescan, a changed sidecar updates `images.rating`.
- **Write (on rating edit):** **merge-preserving.** Read the existing `<image>.xmp` if present,
  update/insert the `xmp:Rating` value inside the `rdf:Description`, and **stream-copy every
  other node verbatim** so foreign edits (e.g. Lightroom develop settings) are never clobbered.
  If no sidecar exists, emit a minimal well-formed `x:xmpmeta` template. If the existing file
  fails to parse, back it up to `<image>.xmp.bak` and write a fresh template (logged, never
  panics).
- **Path convention:** sibling `<image>.xmp` (e.g. `DSC_0001.NEF` → `DSC_0001.NEF.xmp`).
  Documented as the chosen convention; the dotted-vs-replaced-extension choice is fixed to
  `<full-name>.xmp` for unambiguous round-trips.

`xmp:Rating` is the single field with dual storage (XMP truth + SQLite mirror) — necessary
because filtering/sorting by rating requires it in SQLite. All other axes are single-store.

---

## 5. SQLite schema (v3 migration)

Bump `SCHEMA_VERSION` to `3`; add a `if version < 3 { … }` block (existing migrations are
preserved untouched).

```sql
-- images: new columns
ALTER TABLE images ADD COLUMN flag     INTEGER NOT NULL DEFAULT 0;  -- 0 none, 1 pick, 2 reject
ALTER TABLE images ADD COLUMN added_at INTEGER;                     -- ingest timestamp (epoch s)
-- `label` column retired: no longer read or written. Abandoned in place (not dropped) to
--  keep the migration trivial and avoid a table rebuild; it costs nothing and is ignored.

-- global tag vocabulary (SQLite = source of truth)
CREATE TABLE tags (
    id    INTEGER PRIMARY KEY,
    name  TEXT NOT NULL UNIQUE,
    color INTEGER NOT NULL            -- 0xRRGGBB packed
);
CREATE TABLE image_tags (
    image_id INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
    tag_id   INTEGER NOT NULL REFERENCES tags(id)   ON DELETE CASCADE,
    PRIMARY KEY (image_id, tag_id)
);
CREATE INDEX idx_image_tags_tag ON image_tags(tag_id);   -- cross-folder filter by tag

-- collections (SQLite = source of truth)
CREATE TABLE collections (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    color      INTEGER NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE collection_images (
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    image_id      INTEGER NOT NULL REFERENCES images(id)      ON DELETE CASCADE,
    position      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (collection_id, image_id)
);
```

`added_at` is set at insert during ingest (powers the "Recently Added" catalog source and
sort-by-added). Foreign keys use `ON DELETE CASCADE` so removing an image cleans its
associations; `PRAGMA foreign_keys = ON` is asserted on each connection.

---

## 6. Value types (`ferrolite-image`)

Pure data + small helpers, fully unit-tested without a GPU or DB (keeps the engine-tier crate
clean of photo/GPU coupling):

- `struct Rating(u8)` — clamped `0..=5`; `new(v) -> Rating` saturates, `as_u8`, `as_i64`.
- `enum Flag { None, Pick, Reject }` — `as_i64`/`from_i64` mirroring `DecodeStatus`'s pattern.
- `struct Color { r: u8, g: u8, b: u8 }` — `from_packed(u32)`/`to_packed() -> u32` for the
  `0xRRGGBB` SQLite column; `from_hex`/`to_hex` for the UI/tests.
- `struct TagId(i64)` — newtype to prevent id mix-ups (per Rust patterns).

`Flag` and `Rating` follow the existing `model.rs` enum/`as_i64` idiom for consistency.

---

## 7. Query layer — cross-folder filter/sort/search (`ferrolite-catalog`)

A compiled `LibraryQuery` replaces the bare `list_images(folder_id)` for the grid. It is a
plain data struct compiled to **one parameterized SQL `SELECT`** (no string interpolation of
user input — SQL-injection-safe per security rules), run on the existing **read pool**, off the
UI thread.

```rust
pub struct LibraryQuery {
    pub scope:   Scope,               // Folder { id, recursive } | AllPhotographs
                                      //  | Collection { id } | RecentlyAdded { limit }
    pub search:  Option<String>,      // substring over filename + tag names (case-insensitive)
    pub sort:    Sort,                 // { key: CaptureTime|Filename|Rating|AddedAt, desc: bool }
    pub rating:  Option<RatingFilter>, // AtLeast(n) | Exactly(n)
    pub flags:   FlagFilter,           // set of accepted flags (empty = all)
    pub tags:    TagFilter,            // { ids: Vec<TagId>, mode: Any | All } (empty = all)
    pub camera:  Option<String>,       // camera_model exact match
    pub iso:     Option<(u32, u32)>,   // inclusive ISO range
    pub date:    Option<(String, String)>, // capture_time range (ISO-8601 lexical compare)
}
```

- **Compilation** builds a `WHERE`/`JOIN`/`ORDER BY` from set predicates only (absent predicates
  add no clause). Tag `All` mode uses `GROUP BY … HAVING COUNT(DISTINCT tag_id) = N`; `Any`
  uses `tag_id IN (…)`. Scope `AllPhotographs`/`Collection` is what makes **cross-folder**
  results possible.
- **Returns** `Vec<ImageRecord>`; `ImageRecord` gains `rating: Rating` and `flag: Flag`.
- **Grid overlays:** tag color dots are fetched **only for visible rows** via a batched
  `tags_for_images(&[ImageId]) -> HashMap<ImageId, Vec<TagRef>>`, preserving the virtualized
  grid (CLAUDE.md — no O(all-images) work per frame).
- **Distinct-values helpers** for the toolbar: `distinct_cameras()`, `iso_bounds()`,
  `date_bounds()` feed the Metadata popover's combo + slider ranges.

The compiled-SQL builder is a **pure, fully-tested function** (`LibraryQuery -> (sql, params)`),
verifiable without a DB; integration tests then run representative queries against a temp DB.

---

## 8. UI — Library module (`ferrolite-app`)

Built to the design-system Library module. Replaces every `add_enabled(false, …)` stub.

**Toolbar (`library/toolbar.rs`):**
- Live **search** field (debounced ~200 ms before issuing a query).
- **Sort** combo: Capture Time / Filename / Rating / Date Added, with asc/desc toggle.
- **Rating** filter: ★ threshold (≥ N).
- **Flag** filter: segmented None/Pick/Reject (multi-select set).
- **Tag** filter: multi-select dropdown over the global vocabulary + Any/All toggle.
- **Metadata** popover: camera combo + ISO range + capture-date range using the existing
  `EguiSlider` for the numeric ranges.
- The Subfolders toggle and thumbnail-size slider (already live) are preserved.

**Left panel (`library/panel.rs`):** Catalog (All Photographs / Recently Added) and
**Collections** (colored dots, click to scope the grid) join the existing Folders tree. A
**Tag manager** affordance (create / rename / recolor / delete tags) lives here too.

**Grid (`library/grid_layout.rs`) overlays:** star rating, flag glyph, and tag color dots per
cell (Spec 1 §8). Selection supports multi-select for batch apply.

**Metadata-edit commands:** with cell(s) selected — keys `0`–`5` set rating, `P` pick / `X`
reject / `U` unflag (Lightroom convention), and tag toggles via the tag manager / context menu.
Edits apply to the whole selection.

**Toolbar-state → `LibraryQuery`** is a pure function (testable without egui); search debounce
state is likewise isolated.

---

## 9. Threading & responsiveness (CLAUDE.md, contract §1)

Nothing touches the filesystem or DB on the UI thread.

- **Metadata mutations are optimistic + a job.** Setting rating/flag/tags updates the in-memory
  grid model *immediately* for instant feedback, and enqueues a `WriteMetadata` job (carrying a
  cancellation token + priority) that: writes the SQLite row(s)/associations, and — **for
  rating only** — writes the `xmp:Rating` sidecar. On completion the job calls
  `ctx.request_repaint()`; on failure it emits an error event, the optimistic change is
  reverted, and a status-bar note is shown.
- **Queries run on the read pool.** Toolbar changes build a `LibraryQuery` → submit a read job
  → results delivered over the app event channel → grid updates. Search input is debounced so
  keystrokes do not spawn a query each.
- **Tag/collection CRUD** (create/rename/recolor/delete tag; create collection; add/remove
  members; reorder) are jobs writing SQLite, results over the event channel.

Priorities reuse the existing `Interactive > Visible > Background` scheme; metadata writes are
user-initiated but not first-pixel-critical (Visible). Navigation/teardown cancels superseded
query jobs.

---

## 10. Error handling

- Malformed/foreign `<image>.xmp` on write → back up `.xmp.bak`, write fresh, log; never panic.
  On read → ignore unknown content, default rating `0`.
- Read-only directory (cannot write the sidecar) → the SQLite rating still updates so the UI
  works; a status-bar warning surfaces the sidecar-write failure.
- Tag name collision (UNIQUE) / deleting a tag in use → handled as typed `CatalogError`s;
  deleting a tag cascades its `image_tags` rows (the global vocabulary entry is removed).
- DB integrity failure → migrations are versioned; file-derived data re-ingests, but per §3
  tags/collections/flags are **not** rebuildable — reinforcing that the DB is precious.
- One bad sidecar/file never downs ingest (existing panic-isolated worker boundary).

---

## 11. Testing (TDD; 80%+ on non-GPU logic)

- **XMP rating round-trip** (`sidecar/xmp.rs`): read → set rating → write → re-read preserves
  the rating *and* foreign nodes (fixture XMP with extra namespaces); element-form and
  attribute-form `xmp:Rating`; missing-file template; malformed-file `.bak` fallback.
- **`LibraryQuery` compilation** (pure): each scope/predicate/sort emits the expected SQL +
  params; Any vs All tag modes; absent predicates add no clause; injection-safety (params only).
- **Catalog integration** (temp DB + fixtures): v3 migration from a v2 DB; cross-folder tag
  filter returns images from multiple folders; rating filter; collection scoping;
  `tags_for_images` batching; `added_at` set on ingest; incremental rescan picks up a changed
  `.xmp` rating.
- **Value types** (`ferrolite-image`): `Rating` clamp, `Flag` round-trip, `Color`
  packed/hex round-trip.
- **Tag/collection CRUD**: create/rename/recolor/delete; member add/remove/reorder; UNIQUE
  collision error path.
- **Pure UI logic**: toolbar-state → `LibraryQuery`; search-debounce state machine.

GPU crates are untouched, so no golden-image work is needed in this slice.

---

## 12. Build order (TDD throughout)

1. `ferrolite-image` value types (tests first).
2. `ferrolite-catalog` schema v3 migration + model fields (`flag`, `added_at`, retire `label`).
3. `sidecar/xmp.rs` — rating read/write with merge-preserve (round-trip tests).
4. tags + collections tables + CRUD (integration tests).
5. `LibraryQuery` compile + read-pool query functions + `tags_for_images` (pure + integration).
6. Ingest: set `added_at`, read `xmp:Rating` back on scan.
7. `ferrolite-app`: toolbar wiring → grid overlays → metadata-edit commands → tag manager →
   collections in the left panel. Optimistic-update + job threading.
8. **Workspace gate green:** `cargo fmt --check` + `cargo clippy --workspace --all-targets
   -- -D warnings` + `cargo test --workspace`. Then finish the branch.

---

## 13. Decisions recorded (resolved during brainstorming, 2026-06-30)

| Question | Decision | Rationale |
|---|---|---|
| Sidecar format | **XMP for rating now; SQLite for everything else** | User wants ratings portable across PCs immediately; tags/collections/flags need no interop yet, so a single SQLite store is simpler than file-backed truth + mirror. |
| §5.2 (catalog-is-a-cache) | **Amended: SQLite is source of truth for tags/collections/flags** | Those cannot be reconstructed from RAW files; honoring §5.2 would force sidecar files whose only benefit is rebuildability the user has chosen to forgo for now. DB treated as precious. |
| Color label vs tags | **Drop fixed color-label; global colored-tag vocabulary replaces it** | One flexible, many-per-image, app-wide coloring concept instead of two overlapping ones. |
| Tag vocabulary scope | **Global (app-wide), name + color, SQLite** | User wants to filter across multiple folders by tag; a per-folder vocabulary cannot. |
| Flag storage | **SQLite (`images.flag`)** | No standard XMP field; kept in the single SQLite store with tags/collections. |
| Filter scope | **Full set** (search, sort, rating/flag/tag filters, metadata popover) | Completes the Library toolbar designed in Spec 1 §8. |
| XMP write strategy | **Merge-preserving** | Never clobber foreign (e.g. Lightroom) edits in an existing sidecar. |
