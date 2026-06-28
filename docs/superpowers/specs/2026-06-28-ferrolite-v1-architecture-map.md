# ferrolite — v1 Architecture Map & Decomposition

> **Status:** Approved decomposition (parent document). Not a single implementation spec.
> **Date:** 2026-06-28
> **Purpose:** Overarching architecture map for ferrolite v1. This document is the
> **handoff artifact** for spec agents. Each "Spec N" below becomes its own
> `spec → plan → implementation` cycle in this same directory. Read this first,
> then write/continue the spec for the phase you are picking up.
> **Source of truth for goals/non-goals:** the original project proposal (see §"Reference").

---

## How a spec agent uses this document

1. Read this whole file — it carries the settled decisions and the cross-cutting
   interface contracts every phase must honor.
2. Find your phase under **§4 Spec → plan decomposition**.
3. Confirm the **§2 Settled decisions** still hold (do not re-litigate them).
4. Honor the **§5 Cross-cutting interface contracts** — these are the seams between
   crates and must not drift between specs.
5. Write your phase's spec to `docs/superpowers/specs/YYYY-MM-DD-<phase>-design.md`,
   then proceed to a writing-plans cycle.

**Next phase to pick up:** Spec 1 (Speed core) is the validation slice and is being
detailed first. Specs 2–4 are open for future agents.

---

## 1. One-paragraph summary

Cross-platform, open-source, multi-threaded **RAW photo editor + digital asset
manager** in Rust. Two reinforcing goals: (a) be **faster than RawTherapee** at
browsing a library and loading/previewing images; (b) serve as a **learning vehicle
for GPU / pipeline / streaming architecture** transferable to game-engine work.
Non-destructive editing, SQLite-backed catalog, multi-format export. Explicitly **not**
chasing darktable/DxO/Adobe image-science quality. RAW decoding is bought (`rawler`),
not written.

---

## 2. Settled decisions (FIXED — do not re-litigate)

### From the proposal (treat as constraints)

| Layer | Decision |
|---|---|
| Language | Rust |
| Build approach | Fresh project; RapidRAW read as reference only (AGPL — no code copied) |
| RAW decode | `rawler` (LGPL-2.1), version-pinned (API not SemVer-stable) |
| GPU pipeline | `wgpu` + custom WGSL compute shaders (primary learning surface) |
| CPU parallelism | `rayon`; SIMD via `wide`/`pulp`/`std::simd` where needed |
| Catalog/DAM | SQLite via `rusqlite` |
| Color management | `moxcms` (pure Rust) preferred; `lcms2` (C bindings) fallback |
| Thumbnails/resize | `fast_image_resize` (SIMD) |
| Export encoders | `image` (JPEG/PNG/TIFF/WebP), `ravif` (AVIF), `jpegxl-rs` (JPEG-XL) |
| Metadata | `kamadak-exif` (read) + `little_exif` (read/write); hand-rolled XMP sidecar |
| Lens correction | Lensfun bindings (pragmatic; secondary-goal, deferrable) |

### Resolved during brainstorming (2026-06-28)

| Question | Decision | Rationale |
|---|---|---|
| **Project license** | **GPL-3.0 (whole project)** | Simplest mentally; fully compatible with rawler/Lensfun/imagepipe. Engine-transferable crates carry **no copyleft deps** (see §3) so the author can relicense their own code for engine reuse regardless. |
| **GUI framework** | **egui** (`eframe` + `egui-wgpu`) | Frictionless, well-trodden wgpu canvas integration. Spend the learning budget on the GPU pipeline, not the UI layer. |
| **Primary dev/test GPU** | **6–8GB mid-tier** (e.g. RTX 3060/3070) | 45MP fits but a multi-pass pipeline with intermediates gets tight → **tiling is genuinely load-bearing**, not just headroom. |
| **Tiling strategy** | **Sparse virtual texturing from day one** (full engine-style: page table, residency, feedback/visibility pass, tile cache, VRAM-budgeted LRU) | Maximizes engine-learning transfer (the author's explicit priority). De-risked via a staged build ladder in the Spec 1 design. |

### Accepted tradeoffs (do not try to "fix")

- Image quality is **secondary** to speed/architecture. No scope creep toward
  color/denoise parity. "RawTherapee speed with weaker image quality" is acceptable
  for a long time.
- Pure-Rust end-to-end is **not** required; C bindings (lcms2, jpegxl-rs, Lensfun)
  are acceptable.
- `rawler` will lack some new cameras → contribute samples upstream, never fork the decoder.
- Parity with darktable's OpenCL pipeline is **not** a target. The target is beating
  RawTherapee on browse/load — an I/O and caching problem, not image science.

---

## 3. Workspace crate decomposition

Organizing principle: **separate engine-transferable machinery from photo-domain logic
at the crate boundary**, so reusable subsystems carry zero copyleft deps and stay
liftable into the author's game engine even though the binary is GPL-3.0.

### Engine-transferable tier (deps: only `wgpu`, `rayon`, `wide`/`std::simd` — permissive; relicensable)

| Crate | Responsibility | Transferable skill |
|---|---|---|
| `ferrolite-jobs` | Threaded job scheduler over rayon: priority queues, cancellation tokens, dependency-aware task graph, progress sinks. | Engine job system |
| `ferrolite-gpu` | wgpu device/context, compute-pass abstraction, **generic retained DAG executor with dirty-flag invalidation** (knows nothing about photos). | Retained-graph recompute + caching |
| `ferrolite-vt` | Sparse virtual texture: page table, residency, feedback/visibility pass, tile cache, VRAM-budgeted LRU eviction. Depends on `ferrolite-gpu` + `ferrolite-jobs`. | Virtual texturing / LOD streaming |
| `ferrolite-image` | Core pixel/buffer/tile/color vocabulary types shared across crates. | Foundation |

### Photo-domain tier (may pull LGPL/GPL deps → keeps binary GPL-3.0)

| Crate | Responsibility | Phase |
|---|---|---|
| `ferrolite-decode` | Wraps `rawler`: full decode + embedded-preview extraction + metadata read. | 1 |
| `ferrolite-catalog` | `rusqlite` DAM: schema/migrations, folder ingest, indexed queries, thumbnail blob store, sidecar I/O. Repository-pattern interface. | 1 |
| `ferrolite-pipeline` | The **photo edit DAG** (exposure, WB, curve, HSL, crop…) built **on top of** `ferrolite-gpu`'s generic executor. WGSL edit passes. | 2 |
| `ferrolite-color` | Color management (moxcms → lcms2 fallback): camera matrix → working space → display. | 3 |
| `ferrolite-export` | Encoders (`image`/`ravif`/`jpegxl-rs`) + metadata write (`little_exif` + hand-rolled XMP). | 3 |
| `ferrolite-app` | egui shell, panels, browser grid, viewer, command wiring. GPL binary. | 1→ |

**Critical boundary:** `ferrolite-gpu` owns the generic retained-DAG / dirty-flag /
caching machinery; `ferrolite-pipeline` owns the photo-specific edit ops that run on
it. This keeps the recompute engine reusable while edits stay domain-specific.

---

## 4. Spec → plan decomposition

Each is an independent `spec → plan → implementation` cycle. Build order = dependency order.

- **Spec 1 — Speed core (validation slice, IN PROGRESS).**
  Crates: `jobs` + `gpu` (minimal) + `vt` + `image` + `decode` + `catalog` + `app`
  (browser/viewer). Proves **G1 (browse speed)** + **G2 (load/preview speed)**.
  Validation milestone: *rawler decode → instant embedded-preview display → SQLite
  catalog with async thumbnails → smooth GPU zoom/pan on 45MP via sparse VT.*
  Detailed design: see the Spec 1 design doc (next to this file).

- **Spec 2 — Editing.**
  Crates: `pipeline` (retained edit DAG on top of `ferrolite-gpu`) + sidecar op-stack
  persistence + WGSL edit passes (exposure, WB, tone curve, contrast, HSL, crop/rotate).
  Proves **G3 (non-destructive editing)**.
  Open questions to resolve here: edit-stack persistence format (XMP vs custom vs both);
  DAG granularity (per-op vs per-region invalidation; preview-res vs full-res recompute
  scheduling). Note: sparse-VT tile **overlap/halo** support for neighborhood ops is
  added in this phase (Spec 1 ships display-only VT, no halos).

- **Spec 3 — Export & color.**
  Crates: `color` + `export`. Proves **G4 (multi-format export)**.
  Open questions to resolve here: exact working color space; camera-matrix → XYZ →
  working → display path; where ICC transforms sit relative to GPU passes.

- **Spec 4 — Secondary & polish.**
  Lensfun lens corrections, performance tuning (pipeline caching, tiling refinement),
  UX polish, broader camera coverage.

---

## 5. Cross-cutting interface contracts (honored by EVERY spec)

These are the seams between crates. Do not let them drift between phases.

1. **Job submission is universal.** Everything slow — folder ingest, thumbnail gen,
   full decode, tile upload, export — submits a `Job` to `ferrolite-jobs` with
   **priority + cancellation token + progress sink**. Navigation cancels superseded
   work (opening image B cancels image A's pending decode/tiles).

2. **The catalog is a cache, never the source of truth.** Source of truth = original
   files + sidecars on disk. A missing/corrupt SQLite DB must always be rebuildable by
   re-walking the filesystem. This single invariant simplifies error handling across
   the whole app.

3. **Decode yields separable products.** `ferrolite-decode` returns
   `{ PreviewImage (embedded JPEG, decoded), RawImage (full), Metadata }` as
   independently consumable products, so the two-tier load path can show the preview
   without waiting on full decode.

4. **The GPU executor is photo-agnostic.** `ferrolite-gpu` exposes a generic
   retained-DAG executor (nodes, dirty-flag invalidation, cached outputs).
   `ferrolite-pipeline` (Spec 2) supplies photo edit nodes; it does not reach into
   executor internals. The executor must be understandable/testable without any
   photo concepts.

5. **The virtual texture is source-agnostic.** `ferrolite-vt` streams tiles for *any*
   large image source (decoded RAW in Spec 1; pipeline output in Spec 2). Its API is
   `request_view(viewport, lod) → resident tiles` backed by a VRAM-budgeted tile cache;
   it does not know what produced the pixels.

---

## 6. Two-tier load path (drives G2, established in Spec 1, reused after)

1. **Open image →** immediately decode the embedded JPEG preview (fast) → upload as a
   texture → display. *First pixel on screen near-instantly.*
2. **In parallel**, enqueue the full `rawler` decode job. On completion, build the
   sparse-VT source and swap the viewer to the full-res VT-backed view.
3. **Zoom/pan →** VT feedback pass computes needed tiles/LOD → missing tiles requested
   via the job system → uploaded to the tile cache → sampled by the display shader →
   LRU eviction under the VRAM budget.

Fallback: if a camera embeds only a tiny thumbnail (not a usable-resolution preview),
the first-pixel path uses a fast half/quarter-res full decode instead.

---

## 7. Reference

- Original proposal: `2026-06-28-ferrolite-proposal.md` (archived in this directory).
  Goals G1–G5, non-goals NG1–NG6, accepted tradeoffs, settled tech stack. Treat its
  "Settled decisions" as fixed and its "Open questions" as resolved per §2/§4 above.
- RapidRAW (AGPL-3.0): read for ideas only; no code copied into this GPL-3.0 project.
