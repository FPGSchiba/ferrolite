# ferrolite — Spec 1: Speed Core (design)

> **Status:** Design — pending user review, then writing-plans.
> **Date:** 2026-06-28
> **Parent:** `2026-06-28-ferrolite-v1-architecture-map.md` (read first for settled decisions
> and cross-cutting interface contracts).
> **Proves:** G1 (browse speed) + G2 (load/preview speed).
> **UI target:** the **Library module** of the design system
> (`docs/design/ferrolite-design-system.md`, mockup `docs/design/Ferrolite.dc.html`).

---

## 1. Goal & validation milestone

Stand up the speed core and prove the premise:

> rawler decode → instant embedded-preview display → SQLite catalog with async thumbnails →
> smooth GPU zoom/pan on a high-MP image via sparse virtual texturing.

If browsing and first-pixel-on-screen feel faster than RawTherapee on the same machine and
dataset, the premise is validated and three reusable engine subsystems exist (`ferrolite-jobs`,
`ferrolite-gpu`/VT, `ferrolite-vt`). Only then do we layer editing (Spec 2).

**Benchmark target resolution: 24MP** for now (e.g. Nikon Z6/Z f, Sony A7 III, Canon R6),
expandable to 45MP later. The VT/tiling design is identical; 24MP simply fits the 6–8GB GPU
more comfortably, so tiling is exercised for architecture rather than raw necessity at first.

---

## 2. Scope

**In:** `ferrolite-image`, `ferrolite-jobs`, `ferrolite-gpu` (minimal), `ferrolite-vt`,
`ferrolite-decode`, `ferrolite-catalog`, `ferrolite-app` (Library module + viewer + theme +
`EguiSlider`).

**Out (later specs):** the Develop module and all edit ops (Spec 2); histogram, color management,
export (Spec 3); lens correction, polish (Spec 4). The retained **edit** DAG is Spec 2 — Spec 1's
`ferrolite-gpu` carries only the minimal device/display/VT plumbing plus the generic executor
*skeleton interface* so Spec 2 slots in without rework.

---

## 3. Architecture of the slice

```
ferrolite-app (egui shell, Library module, theme, EguiSlider, wgpu canvas)
   │  commands / state
   ├── ferrolite-catalog ──(rusqlite)── catalog.db (cache: folders, images, thumbnails)
   │        ▲ ingest jobs, thumbnail jobs
   ├── ferrolite-jobs ── rayon worker pool (priority + cancel + progress)
   │        │ submits
   ├── ferrolite-decode ──(rawler)── {PreviewImage, RawImage, Metadata}
   ├── ferrolite-gpu ── wgpu device/context + display + (executor skeleton)
   └── ferrolite-vt ── sparse virtual texture (page table, feedback, tile cache, LRU)
            depends on ferrolite-gpu + ferrolite-jobs
   ferrolite-image ── shared pixel/tile/buffer/color vocabulary (used by all)
```

Crate tiers and licensing per the architecture map: `image`/`jobs`/`gpu`/`vt` carry only
permissive deps (engine-transferable); `decode`/`catalog`/`app` may pull LGPL (rawler) → the
binary is GPL-3.0.

---

## 4. Catalog & ingest (G1)

### Schema (SQLite via rusqlite; versioned migrations)
- `folders(id, path UNIQUE, parent_id, last_scanned)`
- `images(id, folder_id, filename, mtime, size, camera_make, camera_model, width, height,
  orientation, capture_time, rating, label, decode_status)` — indexed on `folder_id`,
  `capture_time`.
- `thumbnails(image_id PK, level, w, h, format, blob)` — **thumbnail bytes stored as BLOBs in
  SQLite** (decision confirmed). A 256px WebP/JPEG thumb is a few KB–tens of KB; SQLite's small-blob
  read path is faster than per-file storage and is not the browse bottleneck (generation is). A
  `ThumbnailStore` trait wraps this so a memory-mapped mipmap cache can replace it later (when we
  add multiple preview resolutions or hit huge-library DB bloat) with zero call-site change.

### Ingest flow
1. Select folder → high-priority ingest job.
2. Rayon-parallel directory walk; for each RAW, upsert the `images` row, using `(mtime, size)` to
   **skip unchanged files** (incremental rescan).
3. For each new/changed image, enqueue a lower-priority thumbnail job.
4. Thumbnail job: `ferrolite-decode` extracts the embedded preview → `fast_image_resize` to 256px →
   apply EXIF orientation → encode WebP → write BLOB via `ThumbnailStore`.
5. Grid reads an indexed `images ⨝ thumbnails` query for the active folder, **virtualized** (only
   visible cells realized).

**G1 speed sources:** never walk the filesystem on browse (indexed SQLite reads only) ·
precomputed cached thumbnails · virtualized grid · ingest/thumbnail work off the UI thread.

### Catalog-is-a-cache invariant
Source of truth = files on disk. Integrity failure or schema mismatch → rebuild by re-ingesting.
This keeps error handling simple everywhere.

---

## 5. Two-tier load + viewer (G2)

1. **Open image** → decode the **embedded JPEG preview** → upload texture → display fit-to-screen.
   First-pixel target **< ~150ms**.
2. In parallel, enqueue the **full rawler decode** (Interactive priority; cancels on navigation).
3. On completion → feed the full image to `ferrolite-vt` → **crossfade** preview→full once the
   current view's tiles are resident (no pop).
4. Zoom/pan drives the VT (see §6).

**Fallback:** cameras that embed only a tiny thumbnail → first-pixel path uses a fast half/quarter
-res full decode instead of the embedded preview.

---

## 6. Sparse virtual texture (`ferrolite-vt`) + de-risking ladder

**Target system:** 256×256 tiles in **RGBA16F**; a **page-table** indirection texture
(virtual tile+LOD → physical slot / not-resident); a **GPU feedback/visibility pass** emitting
needed tiles for the current view; CPU diffs vs resident set, enqueues missing tile loads as jobs,
evicts **LRU under a VRAM budget** (configurable physical pool ~1–2GB of the 6–8GB); a **display
shader** sampling page-table→pool that **falls back to a coarser resident LOD** when a tile is
missing (blurry-then-sharpens, never blocks). Tiles are produced by downsampling the decoded image
into an LOD pyramid (`fast_image_resize` on CPU → upload).

**Build ladder — each rung independently demoable; G2 is validated at rung 2, not gated on rung 4:**
1. **Single-texture viewer** — whole image as one texture (24MP RGBA16F ≈ 190MB), zoom/pan. Proves
   the egui↔wgpu canvas + interaction. *Also the fallback path.*
2. **Static tile grid + mip pyramid** (all resident) — tiled sampling + LOD selection + smooth
   zoom/pan. **← G2 validated here.**
3. **Residency + LRU eviction under budget** — on-demand load/evict; handles > VRAM; coarse-LOD
   shader fallback.
4. **Page-table indirection + GPU feedback pass** — full engine-style sparse VT.

**Gate VT:** if rung 4 over-runs, the slice **ships at rung 3** (a real streaming tiled viewer that
already beats RawTherapee), and rung 4 continues as a learning track without blocking the milestone.
Display-only tiling: **no tile overlap/halo** in Spec 1 — halos are added in Spec 2 when neighborhood
edit ops need them.

---

## 7. Job system (`ferrolite-jobs`)

Rayon-backed worker pool. A `Job` carries **priority** (`Interactive > Visible > Background`), a
**cancellation token**, and a **progress sink**; results return over a channel to the app. Priority
queue so current-image decode + current-view tiles preempt background thumbnail backfill.
Navigation **cancels** superseded decode/tile/thumbnail work. Dependencies via simple
job-spawns-job chaining (full dependency-graph scheduling deferred — YAGNI for the slice).

---

## 8. UI — the Library module (egui)

Built to the **Library module** of `docs/design/ferrolite-design-system.md`. This is a first-class
part of the slice, not an afterthought — the speed wins must be *felt* through this UI.

**Shell (Phase-0 deliverable, theme establishes the design system):**
- Title bar: logo, File/Edit/Photo/View/Help menus, **Library/Develop segmented tabs** (Develop tab
  present but its module is a Spec-2 stub), version readout.
- Theme: `Theme` struct overriding egui `Visuals` with the §2 tokens; IBM Plex Sans/Mono bundled
  into the binary (no runtime font fetch).
- `EguiSlider` custom widget per design-system §5 (built here; used by thumbnail-size control +
  metadata-filter exposure ranges now, edit sliders later).

**Library module surfaces:**
- Top toolbar: search field, sort combo, star/label filters, metadata-filter popover (with
  `EguiSlider` ranges), thumbnail-size slider.
- Left panel (236px): Catalog (All Photographs / Recently Added / Quick Collection), Folders tree
  (indented, counts), Collections (colored dots).
- Grid: virtualized 3:2 thumbnails with label dot, star/flag overlay, filename; breadcrumb bar above.
- **Status bar (live, not mocked):** EXIF of the selected image ← `ferrolite-decode`; **"N indexed"**
  ← catalog count; **"GPU: idle/busy"** ← job/VT activity. These are real bindings that make the
  subsystem state visible.
- Canvas: opening a photo (double-click) routes into the viewer (§5) — the Develop *module* is Spec 2,
  but the single-image **viewer surface** (preview→VT zoom/pan) is exercised here for G2.

Deviations allowed per design-system §8 (token consolidation, icon substitution, placeholder data).

---

## 9. Error handling

- Unsupported/corrupt RAW → decode job errors → `decode_status = Failed` → grid placeholder; never
  panics.
- No usable embedded preview → fast partial-decode fallback for first pixel.
- GPU device/surface loss → wgpu error scopes → recreate device + VT pools, re-request visible tiles;
  tile-upload OOM → shrink pool budget + backpressure.
- Catalog corruption → rebuild by re-ingest (cache invariant); versioned migrations.
- Job panic → caught at worker boundary, converted to job error, logged with context; one bad file
  never downs the pool.

---

## 10. Testing (answers proposal open-question #8)

- **GPU correctness = golden-image diffs:** render VT/resample output to an offscreen texture for
  fixed `(image, viewport, zoom)` cases → compare to committed reference PNGs with a small per-pixel
  tolerance (absorbs driver float diffs).
- **VT residency = pure CPU logic tests:** "given viewport+LOD, which tiles are needed" + LRU order —
  no GPU required.
- **Catalog integration tests:** temp DB; fixture folder; assert queries, incremental-rescan skips
  unchanged, thumbnail BLOB round-trip.
- **Decode tests:** small fixture RAWs per target camera (raw.pixls.us) → preview/metadata/orientation.
- **Jobs tests:** priority ordering, real cancellation, panic isolation.
- **Widget tests:** `EguiSlider` value math (snap/clamp/bipolar fill/format) as pure functions.
- Coverage: **80%+ on non-GPU logic**; GPU passes covered by golden diffs rather than line coverage.

---

## 11. Benchmark methodology & acceptance (answers proposal open-question #7)

Same machine, same dataset, head-to-head vs RawTherapee.
- **Dataset:** ~2,000 RAWs across 24MP target cameras (committed file *list* for reproducibility;
  files from raw.pixls.us + author's own).
- **Metrics:** M1 cold folder-open → first thumbnails · M2 warm grid-scroll dropped frames/FPS ·
  M3 time-to-first-pixel opening an image · M4 time-to-full-res-interactive · M5 1:1 zoom/pan frame
  time.
- **Acceptance:** beat RawTherapee on **M1 & M3**; smoother (fewer dropped frames / lower frame time)
  on **M2 & M5**; competitive on M4. **Image quality not compared.**

---

## 12. Build order & decision gates

1. Cargo workspace + crate skeletons + CI (fmt/clippy/test on Windows/macOS/Linux).
2. **Gate 0:** egui shell renders a wgpu texture into its canvas on all 3 OSes; theme + `EguiSlider`
   in place (= VT rung 1). Realizes the design-system shell.
3. `ferrolite-decode`: rawler full decode + embedded preview + metadata on target cameras.
   **Gate decode-preview:** usable embedded preview present? else wire the partial-decode fallback.
4. `ferrolite-catalog` + ingest + thumbnail jobs → Library grid + left panel + status bar (G1).
   Benchmark M1/M2 early.
5. `ferrolite-jobs` hardening (priority, cancellation) as load grows.
6. VT ladder rungs 1→4; **G2 validated at rung 2**; **Gate VT** before committing to rung 4.
7. Two-tier load wiring (preview→full crossfade); benchmark M3/M4/M5.
8. Full head-to-head benchmark vs RawTherapee → validation-milestone decision.

---

## 13. Open items explicitly deferred (not Spec 1)

Edit-stack persistence format, edit-DAG granularity, color pipeline/working space, export formats,
lens correction, histogram, Develop module UI. Tracked in the architecture map under Specs 2–4.
