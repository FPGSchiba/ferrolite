# ferrolite — Spec 1 / Plan 3: Jobs & Library Grid (design)

> **Status:** Design — pending user review, then writing-plans.
> **Date:** 2026-06-29
> **Parent specs:** `2026-06-28-ferrolite-v1-architecture-map.md` (settled decisions +
> cross-cutting interface contracts) and `2026-06-28-ferrolite-speed-core-design.md`
> (Spec 1 design; this is its **Plan 3 of 5**).
> **Proves:** G1 (browse speed) — *felt through the Library UI*.
> **UI target:** the **Library module** of the design system
> (`docs/design/ferrolite-design-system.md`, mockup `docs/design/Ferrolite.dc.html`).

---

## 1. Goal & position in the sequence

Spec 1 ("Speed Core") is a five-plan sequence:

1. ✅ Foundation & Gate 0 — workspace, CI, themed egui shell, wgpu canvas, `EguiSlider`.
2. ✅ Decode & Catalog — `ferrolite-image`, `ferrolite-decode` (rawler), `ferrolite-catalog`
   (SQLite schema, **synchronous** ingest, `ThumbnailStore`, queries).
3. ⏭️ **Jobs & Library grid (this plan).**
4. Viewer & VT ladder — `ferrolite-gpu` + `ferrolite-vt`, two-tier preview→full load.
5. Benchmark harness & milestone — head-to-head vs RawTherapee.

This plan adds the threaded **job system** and turns the Plan 2 synchronous data layer into a
**felt-fast Library browser**: select a folder, watch it index instantly, scroll a virtualized
grid whose thumbnails fill wherever you look. The deliverable proves **G1** and is benchmarked
on **M1** (cold folder-open → first thumbnails) and **M2** (warm grid-scroll smoothness).

**Out of scope (later plans):** GPU context, virtual texture, the single-image viewer, and
two-tier load are **Plan 4**. No `ferrolite-gpu` / `ferrolite-vt` work here. The Develop module,
edit ops, color, and export remain Specs 2–3.

---

## 2. Architecture & crate seams

This phase adds one new crate — **`ferrolite-jobs`** (engine-transferable tier) — and wires it
between the existing catalog and the app. No new photo-domain crates.

```
ferrolite-app (egui Library module: grid, left panel, toolbar, status bar)
   │  submits Jobs                  ▲ results / progress over channel, drained per frame
   ▼                                │
ferrolite-jobs ── rayon pool · priority queue · cancellation tokens · progress sinks
   │  ingest & thumbnail jobs call into ↓
ferrolite-catalog ── WAL SQLite: 1 writer Connection (owned) + read-only Connection pool
   │  uses
ferrolite-decode (embedded-preview extraction)      ferrolite-image (vocabulary)
```

**Dependency purity (architecture map §3):** `ferrolite-jobs` carries **zero photo deps** — only
`rayon` and permissive crates — so it stays liftable into the author's game engine. The binary
remains GPL-3.0.

**Refactor of Plan 2 ingest:** the synchronous `ingest_folder` per-file work becomes the body of
**cancellable jobs**, but its staging is preserved (walk → upsert rows → enqueue thumbnail jobs).
The contract that "ingest is structured so jobs can wrap per-file work without API churn" (Plan 2
global constraints) is now cashed in.

---

## 3. `ferrolite-jobs` — the full scheduler

Built complete now (decision: full scheduler this phase), because it is the engine-transferable
learning artifact and a stable seam that Plan 4 only *adds job kinds* to.

- **`Priority { Interactive, Visible, Background }`** — a binary-heap priority queue feeds a rayon
  pool sized to `available_parallelism() − 1` (leave a core for the UI thread). Higher priority
  dequeues first; FIFO within a priority.
- **`CancelToken`** — a cheap `Arc<AtomicBool>`; clones share the flag. Long jobs poll
  `is_cancelled()` at checkpoints (between files in a walk; before each decode). Cancellation is
  **cooperative**, not preemptive — documented explicitly.
- **`ProgressSink`** — jobs report `(done, total)` and/or a short status message; surfaced live in
  the status bar.
- **`JobHandle`** — holds the `CancelToken` and a result receiver. The app drains a results channel
  each frame, then calls `ctx.request_repaint()` so completed work paints promptly.
- **Reprioritization** — a submitted job's priority can be **raised** (`Background → Visible`) and
  lowered again. This is how the grid promotes its visible window (§5). Implementation: the queue
  re-reads a small priority cell associated with the job id; the concrete mechanism (priority cell
  vs supersede-and-resubmit) is chosen in the plan for the simplest correct option.
- **Panic isolation** — the worker boundary catches panics, converts them to a job error, and logs
  with context. One bad file never downs the pool (architecture map §contract, speed-core §9).

This is a single-process, in-memory scheduler. Full dependency-graph scheduling stays deferred
(YAGNI for Spec 1; speed-core §7). Job-spawns-job chaining covers ingest → thumbnail.

---

## 4. Catalog concurrency — WAL + read-pool + single writer

`rusqlite::Connection` is `!Sync`; background jobs write while the UI queries the visible grid each
frame. Chosen model: **WAL mode, one writer, a pool of read-only connections.**

- **On open:** `PRAGMA journal_mode=WAL` + `PRAGMA synchronous=NORMAL` (durable enough for a
  rebuildable cache; faster writes). Added to the catalog's connection-setup path; no schema
  migration (pragmas are per-connection / one-time WAL switch).
- **`CatalogWriter`** owns the single write `Connection`. All `upsert` and thumbnail-BLOB writes go
  through it, serialized (SQLite permits one writer). It lives on the ingest/thumbnail worker side.
- **`ReadPool`** hands out `Connection`s opened with `SQLITE_OPEN_READ_ONLY`. UI queries borrow one,
  query, return it. Under WAL, readers never block the writer and vice-versa — this is the source
  of the "browse while indexing" feel.
- The Plan 2 repository API is split into `&self` **reads** (served from the pool) vs **writes**
  (through the writer). Existing query SQL is preserved unchanged.

**Cache invariant unchanged:** source of truth = files on disk + sidecars; a corrupt/mismatched DB
is rebuilt by re-ingesting (architecture map §5.2).

---

## 5. Ingest & thumbnail pipeline — viewport-driven priority (Approach A)

1. **Select folder → `Interactive` ingest job.** Rayon-parallel `walkdir`; for each RAW, upsert the
   `images` row, using `(mtime, size)` to **skip unchanged files** (incremental rescan). Row count
   drives the live **"N indexed"** readout. The job polls its `CancelToken` between files so
   switching folders abandons the old scan promptly.
2. For each **new/changed** image, enqueue a **`Background`** thumbnail job: `ferrolite-decode`
   extracts the embedded preview → `fast_image_resize` to 256px max edge → apply EXIF orientation →
   encode JPEG q85 → write BLOB via `ThumbnailStore` (writer connection).
3. **Each frame**, the grid computes its visible image-id window and **promotes those thumbnail jobs
   to `Visible`**; jobs scrolled far off-screen drop back to `Background`. Wherever the user looks
   fills first — the priority tiers we built earn their keep, and the speed win is visible to the eye.
4. The app holds an **LRU `egui::TextureHandle` cache** (cap ~a few hundred cells). Per frame: the
   visible-window query returns rows plus any ready thumbnail BLOBs; a missing thumb → placeholder
   cell; a present thumb → decoded once into a `TextureHandle` and cached. Completed thumbnail jobs
   `request_repaint()` so the next frame's query picks them up.

**G1 speed sources (made felt):** never walk the filesystem on browse (indexed SQLite reads only) ·
precomputed cached thumbnails · virtualized grid (only visible cells realized) · all
decode/resize/encode work off the UI thread · visible-first scheduling.

---

## 6. Library UI — egui, to the design-system Library module

Built to the **Library module** of `docs/design/ferrolite-design-system.md`. The speed wins must be
*felt* through this UI; it is a first-class deliverable, not an afterthought.

- **Left panel (236px):** Catalog (All Photographs / Recently Added), Folders tree from the
  `folders` table (indented, with counts), Collections (colored dots — static for now). Selecting a
  folder issues the read query for that `folder_id`.
- **Top toolbar:** search field, sort combo, thumbnail-size `EguiSlider` (drives cell size), star/
  label filter chips. The metadata-filter popover may be **stubbed** if time-boxed (design-system §8
  permits deviation; placeholder data allowed).
- **Grid:** virtualized 3:2 cells with thumbnail, label dot, star/flag overlay, filename; breadcrumb
  bar above. Only visible cells are realized (drives §5 step 3).
- **Status bar (live, real bindings — not mocked):**
  - selected-image EXIF ← `ferrolite-decode` metadata;
  - **"N indexed"** ← catalog count;
  - **jobs activity** ← `ProgressSink` (e.g. "Generating thumbnails 412/2000");
  - **"GPU: idle"** slot is rendered but **static** — Plan 4 wires real GPU/VT activity here.

---

## 7. Error handling

- Unsupported/corrupt RAW → thumbnail (or decode) job errors → `decode_status = Failed` → grid shows
  a distinct "broken" placeholder. Never panics.
- Job panic → caught at the worker boundary → converted to a job error, logged with context. One bad
  file never downs the pool.
- Folder vanished/permission-denied mid-scan → job error surfaced in the status bar; partial results
  already written are kept (cache, not transaction-of-record).
- Catalog corruption / schema mismatch → rebuild by re-ingest (cache invariant; versioned
  migrations from Plan 2).

---

## 8. Testing (continues speed-core §10)

- **`ferrolite-jobs` (pure / CPU, no GPU):** priority ordering; real cooperative cancellation
  (a job observes its token and stops); panic isolation (one panicking job, pool survives);
  reprioritization (`Background → Visible` reorders dequeue); progress reporting.
- **Catalog concurrency:** WAL read-while-write integration test — a writer inserting rows while a
  pooled reader queries returns a consistent snapshot, no `SQLITE_BUSY` stalls; read-only connection
  rejects writes.
- **Ingest:** incremental rescan skips unchanged `(mtime, size)`; cancellation mid-walk leaves a
  consistent partial catalog (no half-written rows).
- **Grid logic (pure functions, no egui):** visible-window computation from scroll offset + cell
  size; LRU eviction order; cell-state mapping (placeholder / ready / failed).
- **Coverage:** 80%+ on non-GPU logic (there is no GPU in this phase).

---

## 9. Benchmark M1 / M2 — acceptance for this phase

Same machine, same dataset, head-to-head vs RawTherapee (methodology from speed-core §11).

- **Dataset:** ~2,000 24MP RAWs; the file *list* is committed for reproducibility (files from
  raw.pixls.us + author's own).
- **M1 — cold folder-open → first thumbnails on screen.** Acceptance: **beat RawTherapee.**
- **M2 — warm grid-scroll smoothness.** Acceptance: **smoother** than RawTherapee (fewer dropped
  frames / lower frame-time distribution).
- A small instrumented harness records M1 (wall-clock to first N thumbnails painted) and M2
  (frame-time distribution while scrolling a fixed path). **Image quality is not compared.**

Full M3–M5 (load/zoom) belong to Plan 4 once the viewer/VT exist.

---

## 10. Deliverable

Browse a real multi-thousand-image folder fast: instant indexing, virtualized grid with
visible-first thumbnail fill, live status bar, all work off the UI thread — measurably beating
RawTherapee on M1/M2. Plus a complete, tested, photo-agnostic `ferrolite-jobs` scheduler that
Plan 4 extends with decode/tile jobs.
