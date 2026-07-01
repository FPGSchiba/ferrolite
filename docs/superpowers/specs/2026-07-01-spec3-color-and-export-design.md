# ferrolite — Spec 3: Color management & Export (design)

> **Status:** Design — pending user review (2026-07-01); then writing-plans.
> **Date:** 2026-07-01
> **Parent:** `2026-06-28-ferrolite-v1-architecture-map.md` (§4 Spec 3, §5 cross-cutting
> contracts — read first for the settled seams). Extends
> `2026-06-30-spec2-editing-design.md` (the OpStack / edit-DAG / display-linear pipeline;
> esp. §4.3 which documents the display shader's hardcoded sRGB OETF as the placeholder
> **this spec replaces**, and §8.3 the before/after toggle **this spec extends** to a split).
> **Proves:** **G4 (multi-format export)** — plus real color management substituting the
> display path, a live histogram, and a before/after split.
> **UI target:** the **Develop module** (color section, histogram, before/after) and a **new
> third top-level Export module** (batch queue) of the design system.
> **Branch:** `feat/color-and-export` (off `main`).

---

## 1. Goal & validation

Replace the placeholder "treat camera RGB as sRGB" path with real, if pragmatic, color
management, and stand up multi-format export end-to-end:

> open an image → its camera colors are transformed camera→working-space at the head of the
> edit DAG → all edits run in a defined linear working space → the on-screen image is
> transformed working→display (sRGB) at the tail (replacing the hardcoded OETF) → a live GPU
> histogram tracks the edit → a draggable before/after split compares original vs edited →
> **Photo → Export** writes a single file (format + options → destination popup) → the
> **Export module** collects many images into a catalog-persisted queue and mass-exports them
> to a destination folder with a templated filename structure — every output converted to the
> chosen output space with its ICC profile embedded.

Image quality remains **secondary** to speed/architecture (architecture map §2): the
camera→XYZ→working path is a pragmatic single-illuminant transform, not a darktable-parity
color-science effort. The *architecture* (where transforms sit, how they compose, how export
is scheduled) is the deliverable.

---

## 2. Scope

**In:**
- `ferrolite-color` (**new**, photo tier) — pure color math: working-space definitions,
  RGB↔XYZ matrices, camera→working transform, working→display and working→output transforms,
  ICC-profile emit/parse (via `moxcms`, `lcms2` fallback). No GPU/UI coupling.
- `ferrolite-export` (**new**, photo tier) — the encode core: full-res tiled render → output
  conversion → resize → encode (`image`: JPEG/PNG/TIFF/WebP) → metadata + embedded ICC. Runs
  on `ferrolite-jobs` at **Background** priority.
- `ferrolite-decode` — surface the camera color matrix (+ illuminant/white point) from
  `rawler` as a decode product (honoring contract §3), with a graceful fallback for cameras
  lacking a matrix.
- `ferrolite-pipeline` — a GPU `ColorMatrixNode` (camera→working) inserted at the DAG head;
  contrast/curve domains re-documented as working-space linear; a GPU **histogram compute
  pass**.
- `ferrolite-vt` / display + `ferrolite-pipeline`'s blit shaders — the **tail transform**:
  the hardcoded `linear_to_srgb` becomes `working→display` (3×3 uniform) + sRGB OETF,
  structured as a **swappable** transform.
- `ferrolite-catalog` — an `export_queue` table + repository methods (persisted queue).
- `ferrolite-app` — working-space selector, histogram widget, before/after **split** +
  toolbar toggle button, single-file **Photo → Export** flow, the new **Export module**
  (queue list + shared settings + destination + filename template + Start), and a
  **cross-cutting resizable-side-panels** change.
- **Design-system doc** update: "two modules" → three (add the Export module).

**Out (later specs / non-goals):**
- AVIF (`ravif`) + JPEG-XL (`jpegxl-rs` / libjxl) export → Spec 4 (deferred: C-toolchain weight).
- OS **monitor-profile** auto-detection / manual display-profile picker → Spec 4 (the tail is
  built swappable so this drops in cleanly). Spec 3 assumes an **sRGB display**.
- Per-image export overrides (batch uses **shared** settings); export presets.
- Batch **edits** / copy-paste adjustments (Spec 2 non-goal, unchanged — this is batch
  *export* only).
- Soft-proofing, out-of-gamut warnings, dual-illuminant camera-matrix interpolation.

---

## 3. Architecture of the slice

```
ferrolite-app
  Develop: working-space selector · histogram widget · before/after split + toolbar button
  Export module (NEW): queue list · shared settings panel · destination + filename template · Start
   │
   ├── ferrolite-color (NEW, photo tier) ── pure color math (no GPU): WorkingSpace, RGB↔XYZ
   │        matrices, camera→working, working→display, working→output, ICC emit/parse (moxcms)
   │
   ├── ferrolite-pipeline ── + ColorMatrixNode (camera→working) at DAG head
   │        + histogram compute pass; contrast/curve domains = working-space linear
   │        ▼ PipelineImage output is now **working-space linear**
   │
   ├── ferrolite-vt / display.wgsl + blit.wgsl ── tail: working→display (3×3 uniform) + sRGB OETF
   │        (replaces hardcoded linear_to_srgb; swappable for a future monitor-profile path)
   │
   ├── ferrolite-export (NEW, photo tier) ── tiled full-res render (reuses Spec 2 GPU tile
   │        producer, no whole-image RGBA16F) → working→output → resize → encode → EXIF+ICC
   │        (runs on ferrolite-jobs @ Background)
   │
   ├── ferrolite-decode ── surfaces camera color matrix + illuminant (contract §3 product)
   │
   └── ferrolite-catalog ── export_queue table + repository (persisted queue; still a cache)
```

**Licensing tiers (architecture map §3) preserved.** `ferrolite-color` and `ferrolite-export`
are **photo-tier** (they may pull LGPL/GPL color/codec deps → keep the binary GPL-3.0).
`ferrolite-gpu` / `ferrolite-vt` / `ferrolite-image` stay **engine-transferable**: the tail
transform is a generic 3×3-matrix-plus-OETF uniform (no photo concepts), and the histogram
compute pass over a texture is generic — the *photo-specific* choice of working space and
matrices lives in `ferrolite-color`/`ferrolite-pipeline`, never in the engine crates. The
generic `Graph<PipelineImage>` executor is **not modified** (contract §4).

---

## 4. `ferrolite-color` — pure color math

Pure, `Clone`, no GPU/UI/`unsafe`; the whole crate is unit-testable on every OS in CI.

### 4.1 Working spaces
- `enum WorkingSpace { Srgb, AdobeRgb, DisplayP3, Rec2020, ProPhoto }` — the **curated 5**.
- Each maps to **primaries (xy) + white point** → a linear **RGB→XYZ 3×3** (and its inverse).
  White point is D65 for all except ProPhoto (D50); adaptation handles the mismatch.
- **Default working space = `Rec2020`** (linear): wide enough to hold nearly all camera
  colors, real (non-imaginary) primaries.
- All working-space math is **linear**; OETFs are applied only at the display/output tail.

### 4.2 Camera → working
- Input: `rawler`'s camera `ColorMatrix` (camera-native → XYZ-ish, DNG-style) surfaced by
  `ferrolite-decode` (§6). Compose: `camera→XYZ` → **Bradford chromatic adaptation** to the
  working white point → `XYZ→working` = a single **camera→working 3×3**.
- **Pragmatic single-illuminant** (no dual-illuminant interpolation): use the matrix `rawler`
  provides; quality is secondary.
- **Fallback:** camera without a usable matrix → assume the image is already in **sRGB
  primaries** (identity into an sRGB→working conversion), logged. Never panics.

### 4.3 Tail transforms (composed here, applied on the GPU)
- `working→display` = `working→XYZ` → `XYZ→sRGB` (D65) = a **3×3**; the sRGB **OETF** is applied
  in-shader after the matrix. Built as a plain matrix uniform so a future monitor-profile path
  swaps only the matrix/LUT (Spec 4), not the shader structure.
- `working→output(space)` = `working→XYZ` → `XYZ→output` = a **3×3**; the output space's OETF
  is applied at encode. Feeds both display and export from the same composition code.
- **Regression invariant:** with `WorkingSpace::Srgb`, `working→display` is the identity 3×3
  and the tail reduces **exactly** to today's `linear_to_srgb` (proven by a GPU golden, §10).

### 4.4 ICC profiles
- `moxcms` emits **standard ICC profiles** for each `WorkingSpace`/output space for **embedding
  on export**, and parses an embedded ICC if one is ever present. `lcms2` is the fallback per
  the architecture map. Profile *generation* is validated by byte/round-trip sanity tests; it
  is not on any interactive path.

---

## 5. Managed pipeline — DAG head + display tail

### 5.1 `ColorMatrixNode` (camera → working) at the DAG head
- A new `Node<PipelineImage>` inserted as the **first child of `SourceNode`**, before
  `Exposure`. It multiplies each pixel RGB by the **camera→working 3×3** (a small uniform).
- The CPU **demosaic is unchanged** (it still applies as-shot WB coefficients); camera→working
  is a **GPU node** so the matrix is swappable when the working space changes without touching
  decode. The edit `WhiteBalance` op remains a user tweak **in working space**.
- New canonical op order (the DAG, built once per open, only dirtied on edits — Spec 2 §4.2):
  `Source → ColorMatrix → Exposure → WhiteBalance → Contrast → ToneCurve → Hsl → Sharpen →
  Geometry → [output: working-space linear]`.
- Changing the working space updates the `ColorMatrixNode` uniform **and** the tail matrix, then
  `mark_dirty(ColorMatrixNode)` re-runs the chain. Pipelines are built once (CLAUDE.md GPU rule).

### 5.2 Display tail (replaces the hardcoded OETF)
- `display.wgsl` (`fs_main`, `fs_tiled`, `fs_sparse`) and `blit.wgsl` gain a **`working→display`
  3×3 matrix uniform**; each samples the (now working-space) texel, applies the matrix, then the
  sRGB OETF. The hardcoded `linear_to_srgb`-only path is removed.
- The matrix uniform is pushed when the working space changes — **not per frame, not per image**.
  Display pipelines are still built once at startup and pre-warmed (CLAUDE.md).

### 5.3 Op-semantics note
Spec 2 documented the contrast pivot (mid-grey) and tone-curve domain as **display-linear
placeholders**. They are now **defined as working-space linear** — a documentation/constant
change, no structural rework (Spec 2 §4.3 anticipated this).

---

## 6. `ferrolite-decode` — surface the camera color matrix

- Per contract §3 (decode yields separable products), extend the decode products with a
  **`ColorProfile`** (camera→XYZ matrix + reference illuminant/white point) read from `rawler`,
  alongside the existing `{ PreviewImage, RawImage, Metadata }`.
- Purely additive: existing consumers ignore it. `ferrolite-pipeline` consumes it to build the
  `ColorMatrixNode` uniform via `ferrolite-color`.
- **Fallback:** no matrix available → `ColorProfile::srgb_fallback()` (logged), so the pipeline
  always has a defined transform.

---

## 7. Histogram (GPU compute) + before/after (split)

### 7.1 Live histogram
- A **compute shader** over the preview texture fills a `256 × {R,G,B,luma}` bin buffer via
  atomics, in **display-referred** space (apply `working→display` before binning so the
  histogram matches what's on screen). Only the **~4 KB bin buffer** is read back — no
  whole-image GPU→CPU readback (CLAUDE.md §1).
- New compute pipeline built **once** and reused; recompute is **debounced** and triggered on
  preview recompute. Readback is async (`map_async`) → delivered over the app event channel →
  `request_repaint()`.
- Rendered in the adjustment panel's histogram area (design-system §6). Read-only, so no
  per-component reset applies.

### 7.2 Before/after split
- The viewer renders `OpStack::default()` (original) **left** of a **draggable vertical
  divider** and the current stack **right**, reusing Spec 2's before/after evaluation through
  the same DAG output. Implemented by sampling the "before" vs "after" preview texture by
  screen-x relative to the divider.
- A **Develop-toolbar toggle button** turns the split-compare view on/off. The `\` key remains
  the **momentary full-before** toggle from Spec 2 (unchanged).
- Divider position, drag, and hit-test math are a **pure tested unit**; egui only routes pointer
  events into it. Split is a **preview-tier** feature; at 1:1 zoom the after-view is shown
  (logged), never blocks.

---

## 8. Export — encode core, single flow, and Export module

### 8.1 `ferrolite-export` encode core (shared by both flows)
- **Full-res render, tiled:** reuse Spec 2's GPU **tile producer** (halo-correct) to render the
  edited full-res image tile-by-tile, reading back each tile and assembling the CPU buffer —
  **no whole-image RGBA16F on the GPU** (honors CLAUDE.md bounded-GPU + fits 45 MP, the same
  rationale as Spec 2's full-res tier).
- **Output conversion:** `working→output(space)` 3×3 + output OETF (via `ferrolite-color`),
  quantized to **8-bit** (default) or **16-bit** (TIFF/PNG).
- **Resize (optional):** `fast_image_resize` (existing dep) — none / long-edge px / exact W×H /
  percent.
- **Encode:** `image` crate — **JPEG, PNG, TIFF, WebP**. JPEG/WebP take a quality setting;
  PNG/TIFF lossless.
- **Metadata:** copy source **EXIF** (`little_exif`) + **embed the output ICC**; optional XMP
  write; a "strip metadata" toggle.
- Every export runs as a **cancellable `ferrolite-jobs` job at `Background` priority** (below
  Visible tile streaming and Interactive editing — export never contends with the UI), with a
  progress sink → app events.

### 8.2 Export options (shared defaults)
- **Output color space:** default **sRGB** (web-safe), selectable from the 5; embeds matching ICC.
- **Bit depth:** 8-bit default; 16-bit for TIFF + PNG.
- **Resize:** default none.
- **Quality:** JPEG + WebP default ~90.
- **Metadata:** copy EXIF + embed ICC by default; strip toggle; XMP optional.

### 8.3 Single flow — Photo → Export
- Menu action (`Photo → Export`, wiring the placeholder menu) → a **format + options popup** →
  a **destination path popup** (`rfd`, existing dep) → **one** `ferrolite-export` job on the
  currently open image.

### 8.4 Batch flow — the Export module (`Module::Export`)
- New `Module::Export` variant + a **third title-bar segmented-control entry**, following the
  existing chrome grammar (title bar → toolbar → content row). **Design-system doc updated**
  from two modules to three.
- **Layout:** a **queue list** (thumbnails of collected images) · a **right shared-settings
  panel** (the §8.2 options, resizable) · a **bottom bar** (destination-folder picker +
  **filename token template** field + **Start**).
- **Add-to-queue** actions: from **Library** (multi-select context menu) and **Develop**
  (current image).
- **`export_queue`** catalog table (`image_id`, `position`, `added_at`) with repository methods
  (add / remove / list / clear / reorder). It is **persisted UI state**, consistent with
  "catalog is a cache" (contract §2): it is not source-of-truth for images and its loss never
  loses photos or edits.
- **Filename token template:** a pure `expand(template, ctx) -> String` — tokens `{name}`
  (original basename), `{seq}` (counter, `{seq:03}` zero-pad), `{date}`, `{camera}` + literal
  text; collision auto-suffix within the destination. Unit-tested.
- **Start** → one `ferrolite-export` **Background** job per queued image with **aggregate
  progress**; navigation/new-work cancels superseded jobs via the existing cancel plumbing
  (contract §1).

---

## 9. Cross-cutting UI — resizable side panels

- **Audit every `SidePanel`.** The Library **left** panel is already resizable
  (`app.rs` `SidePanel::left("left").resizable(true).default_width(236.0)`); the Develop
  **right adjustment panel** is fixed (`SidePanel::right("develop_adjust").exact_width(296.0)`).
- Make **all side panels resizable**: design-system exact widths (296 / 236 px) become
  **defaults** with sensible **min/max clamps**; widths **persist** via egui memory across
  sessions.
- New **Export-module** panels ship resizable from the start. This is a small, focused sweep,
  not a re-layout.

---

## 10. Error handling

- **Missing/invalid camera matrix** → `ColorProfile::srgb_fallback()` (logged); pipeline always
  has a defined transform. Never panics.
- **GPU pass / device loss** → existing wgpu error-scope recovery recreates `GpuContext`,
  pipelines (incl. the color/histogram passes), and VT pools; the tail matrix + histogram
  pipeline are rebuilt once on recovery, not per edit (reuses Spec 1/2 recovery).
- **Export render OOM** → the tiled producer bounds VRAM; on pressure, shrink the tile
  working-set and backpressure (as Spec 2). A failed tile fails that export with a status
  warning, not a crash.
- **Encode / write failure** (bad path, permissions, disk full) → per-image failure surfaced in
  the export progress UI (single: status-bar warning); the batch continues with remaining
  images; partial outputs are reported. Never panics.
- **ICC emit failure** → export proceeds **without** an embedded profile plus a warning (pixels
  are still converted); the file is valid, just untagged.
- **`export_queue` DB error** → treated like any catalog-cache error: the in-memory queue is
  authoritative for the session; a warning is shown; images are never lost (contract §2).
- **Job panics** are caught at the existing worker boundary; one bad export never downs the pool.

---

## 11. Testing (TDD; CLAUDE.md gate, then hold for the author's visual test)

**Pure CPU logic (every OS in CI — the 80%+ target):**
- `ferrolite-color`: RGB↔XYZ round-trips; known-value primaries checks; Bradford adaptation;
  camera→working for a known camera matrix; `working→display` identity when working = sRGB;
  ICC-emit byte/round-trip sanity.
- `WorkingSpace` enum: default = Rec2020; matrix inverses consistent.
- Camera-matrix fallback selection (matrix present vs absent).
- Filename token expander: token substitution, `{seq:03}` padding, collision auto-suffix,
  literal text, unknown-token handling.
- `export_queue` repository (SQLite): add / remove / list ordering / clear / reorder.
- Export param → encoder-settings mapping; resize math (long-edge / exact / percent).
- Before/after divider math: position clamp, hit-test, drag.

**Golden-image GPU diffs (auto-skip when `GpuContext::headless()` is `None`, per Spec 1's CI
constraint — `cargo test --workspace` stays green headless):**
- `ColorMatrixNode` (camera→working) vs a reference at a fixed matrix.
- Display tail vs reference; **sRGB ≡ old `linear_to_srgb`** regression golden (§4.3 invariant).
- Histogram compute vs a CPU reference over the same image.
- Tiled full-res export render vs a whole-image reference within tolerance (tile-seam
  correctness with halo, reusing Spec 2's tile-seam proof).
- Per-format encode round-trip (encode → decode → compare within tolerance) for JPEG/PNG/TIFF/WebP.
- Goldens authored/verified locally on the dev GPU (RTX 3060/3070 class).

**egui UI** (working-space selector, histogram widget, before/after split + toolbar button,
export dialog, Export module, resizable panels): `cargo build` + clippy + the author's hands-on
visual test. No golden tests for egui rendering.

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → **then STOP and hold for the author's (Jann's) visual test of
the running app** before finishing the branch (CLAUDE.md "Finishing a branch" rule).

---

## 12. Decomposition into implementation plans

Build order = dependency order; each plan is its own writing-plans → TDD cycle, all on the one
`feat/color-and-export` branch.

1. **`ferrolite-color` foundation + decode color profile.** The pure color crate (working
   spaces, matrices, camera→working, tail/output transforms, ICC emit) with full CPU tests;
   `ferrolite-decode` surfaces the camera `ColorProfile` (+ fallback). No UI.
2. **Managed-pipeline wiring.** `ColorMatrixNode` at the DAG head; the display/blit **tail**
   (3×3 uniform + OETF, swappable); the Develop **working-space selector**; the **sRGB ≡ old**
   regression golden. Also the cross-cutting **resizable-side-panels** sweep (§9).
3. **Histogram + before/after split.** GPU histogram compute + async readback + widget; the
   draggable vertical **split** + toolbar toggle button.
4. **`ferrolite-export` + single-file Photo → Export.** Tiled full-res export render, output
   conversion, resize, the four encoders, EXIF + ICC embed; the `Photo → Export` popup flow.
5. **Export module (batch).** `Module::Export` + segmented-control entry, `export_queue` catalog
   table + repository, add-to-queue actions, shared settings panel, destination + filename token
   template, batch orchestration + aggregate progress, and the **design-system doc** update.

---

## 13. Decisions recorded (resolved during brainstorming, 2026-07-01)

| Question | Decision | Rationale |
|---|---|---|
| Color pipeline model | **Full managed pipeline** (camera→working at source, working→display at tail, working→output on export) | The model the v1 map intends ("camera-matrix→XYZ→working→display"); makes edits well-defined; existing Spec 2 op domains become working-space linear. |
| Working space | **Curated 5** (sRGB, Adobe RGB, Display P3, Rec.2020, ProPhoto), **default linear Rec.2020** | Real-editor range without over-engineering; Rec.2020 is wide with real (non-imaginary) primaries. |
| Display CM depth | **Assume sRGB display**; tail built **swappable** | Cross-platform, correct for the common monitor, matches "quality secondary"; monitor-profile path is a clean Spec 4 add. |
| Export formats | **image quartet** (JPEG/PNG/TIFF/WebP); AVIF + JPEG-XL → Spec 4 | Proves G4 with one dep and no new C toolchain; libjxl/ravif weight deferred. |
| Export flows | **Single (Photo→Export) + batch Export module** | User requirement: a first-class batch queue module in addition to single-file export. |
| Filename structure | **Token template** (`{name}`,`{seq}`,`{date}`,`{camera}` + literals) | Flexible, Lightroom-familiar; a pure tested expander. |
| Export queue persistence | **Persisted in catalog** (`export_queue` table) | User choice; queue survives restart; still a cache (not source-of-truth), consistent with contract §2. |
| Batch settings | **Shared for the whole batch** | The standard batch-export model; simple, predictable. |
| Export job priority | **Background** | Lowest priority (Interactive > Visible > Background) so export never contends with editing, navigation, or tile streaming. |
| Histogram compute | **GPU compute pass**, display-referred, ~4 KB readback | No whole-image CPU readback (CLAUDE.md §1); aligns with the GPU-learning goal; pipeline built once. |
| Before/after | **Draggable vertical split + toolbar toggle button** (full-`\` toggle unchanged) | Editor-grade compare; reuses Spec 2's empty-vs-current-stack evaluation. |
| Side panels | **All side panels resizable** (widths → defaults + clamps, persisted) | User requirement; design-system exact widths become defaults. |
| New crates | **`ferrolite-color` + `ferrolite-export`** (photo tier) | Architecture map §3/§4; engine-transferable crates stay copyleft-free (tail = generic matrix+OETF). |
| Executor changes | **None** — reuse `Graph<PipelineImage>` | Honors contract §4: executor stays photo/wgpu-agnostic. |
| Scope | **One spec, 5 implementation plans**, one branch | Mirrors Spec 2's decomposition; keeps each plan reviewable. |
