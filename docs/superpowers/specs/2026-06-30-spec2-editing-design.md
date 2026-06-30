# ferrolite — Spec 2: Editing (design)

> **Status:** Design — approved by user (2026-06-30); pending writing-plans.
> **Date:** 2026-06-30
> **Parent:** `2026-06-28-ferrolite-v1-architecture-map.md` (§4 Spec 2, §5 cross-cutting
> contracts — read first for the settled seams). Builds directly on
> `2026-06-29-viewer-and-vt-ladder-design.md` (the viewer, VT rungs 1–4, `TileSource`,
> `QuadBin` demosaic, `DisplayPipelines`) and the Spec 1.5/1.6 metadata model
> (`2026-06-30-tags-and-filters-design.md`, `2026-06-30-develop-metadata-and-filters-design.md`).
> **Proves:** **G3 (non-destructive editing)** — a GPU edit pipeline, sidecar-persisted
> op stack, and the real Develop adjustment panel.
> **UI target:** the **Develop module** of the design system
> (`docs/design/ferrolite-design-system.md` §6 — 296px right adjustment panel).
> **Branch:** `feat/editing-pipeline` (off `main`).

---

## 1. Goal & validation

Stand up non-destructive editing end-to-end:

> open an image → adjust exposure/WB/contrast/tone-curve/HSL/sharpening/crop-rotate with
> sub-frame slider response on the preview → inspect the edit at 1:1 (full-res, tiled, on the
> GPU) → undo/redo and before/after → the op stack persists to the `.xmp` sidecar and reloads
> on the next open.

Editing is **non-destructive**: the original file is never written; edits are an ordered op
stack stored in the sidecar and replayed on a GPU pipeline. Image quality is secondary to
speed/architecture per the settled tradeoffs (architecture map §2) — these are pragmatic,
fast ops, not a darktable-parity color science effort.

---

## 2. Scope

**In:**
- `ferrolite-pipeline` (**new**, photo tier) — the `OpStack` document model + a retained edit
  DAG built on `ferrolite-gpu::Graph`, with WGSL **compute** edit passes.
- WGSL edit passes: **exposure, white balance, contrast, tone curve, HSL (8-band), sharpening
  (unsharp mask — the neighborhood op), crop/rotate (geometry)**.
- `ferrolite-vt` — **tile overlap/halo** support + a **GPU tile-producer seam** so the full-res
  view can be produced by the edit pipeline per-tile (no CPU readback). Spec 1 shipped
  display-only tiling with no halos; this adds them.
- `ferrolite-catalog::xmp` — persist the op stack as a ferrolite `frl:` namespace alongside the
  existing `xmp:Rating`, merge-preserving.
- `ferrolite-app` — the Develop **296px right adjustment panel**, undo/redo + reset,
  before/after toggle, interactive crop overlay; wiring slider → op stack → DAG → repaint.

**Out (later specs / non-goals):**
- Color management, working-space selection, ICC, histogram, before/after **split** view → Spec 3.
- Multi-format export → Spec 3.
- Lens correction, masking/healing/gradients (the Develop toolbar's Heal/Mask/Grad), broader
  camera coverage, performance tuning → Spec 4.
- Adobe `crs:` interop (a non-goal; foreign `crs:` nodes are merely *preserved*, never authored).
- Batch/copy-paste edits across images, edit presets.

---

## 3. Architecture of the slice

```
ferrolite-app (Develop module: adjustment panel, crop overlay, undo/redo, before/after)
   │  slider → new OpStack → mark node dirty → repaint
   ├── ferrolite-pipeline (NEW, photo tier) ── OpStack + edit DAG on ferrolite-gpu::Graph
   │        │ WGSL compute passes (exposure, WB, contrast, curve, HSL, sharpen, geometry)
   │        ▼ produces PipelineImage (Arc<wgpu::Texture> + dims)
   ├── ferrolite-gpu ── Graph<O> retained executor (UNCHANGED, photo/wgpu-agnostic) + GpuContext
   ├── ferrolite-vt ── + tile halo + GPU tile-producer seam (full-res edited tiles, no readback)
   ├── ferrolite-decode ── QuadBin demosaic → LinearRgbaF32 (unchanged; the pipeline source)
   └── ferrolite-catalog::xmp ── frl:ops persistence in the .xmp sidecar (merge-preserving)
```

**Licensing tiers (architecture map §3) are preserved.** `ferrolite-gpu`/`ferrolite-vt`/
`ferrolite-image` stay engine-transferable (permissive deps only) — the halo + tile-producer
seam additions carry **no photo concepts**. `ferrolite-pipeline` is photo-domain (it owns the
edit ops) and keeps the binary GPL-3.0. The generic executor is **not modified**: it is used
with a concrete photo output type `O = PipelineImage`, honoring cross-cutting contract §4
(the executor supplies graph machinery; the pipeline supplies the edit nodes).

---

## 4. `ferrolite-pipeline` — model & DAG

### 4.1 The document model: `OpStack`
Pure data, `Clone`, (de)serializable; **no GPU coupling** — this is the persisted document and
the unit of undo/redo. Fully unit-testable without a GPU.

- `enum Op` with one variant per adjustment, each a small param struct:
  - `Exposure { ev: f32 }` (stops)
  - `WhiteBalance { temp: f32, tint: f32 }` (UI units → RGB multipliers, see §4.4)
  - `Contrast { amount: f32 }` (bipolar, pivot at mid-grey in linear)
  - `ToneCurve { points: Vec<(f32, f32)> }` (control points in [0,1]; baked to a 256-entry LUT)
  - `Hsl { bands: [HslBand; 8] }` (per-band hue/sat/lum deltas; bands = red…magenta)
  - `Sharpen { amount: f32, radius: u32 }` (unsharp mask; the neighborhood op)
  - `Geometry { crop: CropRect, angle_deg: f32, aspect: Aspect }`
- `struct OpStack { version: u32, ops: Vec<Op> }` — a **fixed canonical op order** (the variant
  order above); the stack stores the *parameters*, the order is implied. An absent op = identity.
- Helpers (tested): `OpStack::default()` (identity/unedited), `is_identity()`, `set_op(Op)`
  (immutable — returns a new stack), per-op reset, full reset.

### 4.2 The edit DAG (on the existing `Graph<O>`)
- `O = PipelineImage` — an `Arc`-wrapped `wgpu::Texture` (+ `width`/`height`/format). Cheap to
  clone (Arc handle), so cached node outputs are cheap to hold.
- One `Node<PipelineImage>` impl **per op kind**, each owning `Arc<GpuContext>` + its
  **once-built** compute pipeline + a small per-eval uniform buffer for its params. `evaluate`
  binds the input texture + params, dispatches the compute pass into a fresh output texture,
  returns it. A `SourceNode` wraps the demosaiced `LinearRgbaF32` upload as the graph root.
- **Per-op invalidation is free:** changing one op's params updates that node's uniform + calls
  `Graph::mark_dirty(node)`; the executor re-runs that node + downstream only, reusing cached
  upstream textures. (Answers the architecture-map open question "DAG granularity: per-op".)
- The DAG is **rebuilt only when the op-set/order changes** (it never does — the order is fixed),
  so in practice the graph is built once per opened image and only **dirtied** on edits. Pipelines
  are built once and reused (CLAUDE.md GPU rule).

### 4.3 Color space
All passes operate in **display-linear RGB** — exactly the space `QuadBin` outputs and the
display shader expects (it applies the sRGB OETF at present; Spec 3 substitutes a color-managed
path). Contrast pivots and curve/LUT domains are documented as display-linear placeholders so
Spec 3 can refine without reworking the pass structure.

### 4.4 Param → GPU conversions (pure, tested)
The CPU-side math that turns UI params into shader uniforms is isolated and unit-tested,
independent of the GPU:
- WB temp/tint → per-channel RGB multipliers.
- Tone curve control points → a 256-entry monotone LUT (uploaded as a small texture/buffer).
- Contrast amount → gain/pivot.
- HSL bands → the 8×(h,s,l) uniform array.
- Sharpen amount/radius → unsharp weight + kernel radius (drives the **halo size**, §5.1).

---

## 5. `ferrolite-vt` — halo + GPU tile producer

### 5.1 Tile halo/overlap (engine tier, no photo concepts)
- `TileSource` gains `tile_with_halo(coord, halo: u32) -> LinearRgbaF32` returning a
  `(TILE_SIZE + 2·halo)²` tile, edge-clamped where it overhangs the level. The existing
  `tile(coord)` is defined as `tile_with_halo(coord, 0)` (no behavior change for Spec 1 paths).
- `ferrolite-image` gains the pure helpers the haloed fetch needs (haloed origin/extent math),
  unit-tested without a GPU.
- The halo width is supplied by the consumer (the edit pipeline picks it from the active op set —
  e.g. `Sharpen.radius`, rotate resampling footprint). Point-only stacks request `halo = 0`.

### 5.2 GPU tile-producer seam (source-agnostic per contract §5)
The VT today fills a physical pool slot by **uploading a CPU `LinearRgbaF32`**. Spec 2 adds an
alternative: a slot can be filled by a **producer that renders directly into it on the GPU**,
with **no CPU readback**. The VT exposes a seam such as:

```
trait TileProducer {
    /// Fill the pool slot for `coord` (writing the interior TILE_SIZE² region).
    fn produce(&self, ctx: &GpuContext, coord: TileCoord, slot: PoolSlot);
}
```

- Spec 1's CPU-upload path is retained as the default producer (unchanged).
- The app supplies an **edit producer** (defined in `ferrolite-app`/`ferrolite-pipeline`, not in
  the VT — the VT stays photo-agnostic) that: samples the **haloed** source region from the
  GPU-resident full-res source, runs the OpStack compute chain over it, and writes the interior
  256² into the slot. The VT only knows "ask the producer to fill this slot for this coord".
- The full-res source pyramid is uploaded to the GPU once on full-decode (reuses the existing
  pyramid build); per-tile editing samples it — no per-tile CPU work, no readback.

### 5.3 Edited-tile caching & invalidation (per-region recompute)
- Edited tiles in the pool are tagged with an `opstack_version`. Any edit **bumps the version**;
  the resident edited tiles for the old version are invalidated and **re-streamed lazily for the
  current view** via `ferrolite-jobs` at **Visible** priority. Off-screen tiles are never
  produced until viewed. (Answers "per-region invalidation" + "full-res recompute scheduling".)
- Navigation or a new edit **cancels** superseded tile-production jobs (cross-cutting contract §1),
  reusing the existing cancel-token plumbing.

---

## 6. Two-tier recompute (preview-res vs full-res)

The interactive surface is the **preview tier**; the full-res tier is for 1:1 inspection.

1. **Preview tier (interactive).** The OpStack DAG runs on a **single fit/preview-res texture** —
   the QuadBin ≈6 MP image fits one texture comfortably (≈48 MB RGBA16F). A slider drag marks one
   node dirty → `Graph::evaluate(output)` re-runs only the changed op + downstream → the result is
   shown via the existing **rung-1 single-texture** display path. This is a handful of ~6 MP
   compute passes — inside frame budget; profiled per CLAUDE.md. No tiling, no halo needed here.
2. **Full-res tier (1:1 zoom).** When the user zooms past the preview's effective resolution, the
   VT streams **edited** full-res tiles via the GPU tile producer (§5.2) at Visible priority, with
   the coarse-LOD fallback (blurry→sharp, never blocks) inherited from Spec 1. Halo applies here
   (§5.1) so `Sharpen`/rotate have no tile seams.

This keeps slider response instant (you only ever *see* screen-res while dragging) and defers
full-res work to when it is actually inspected — and it is the design that makes halos
load-bearing and fits the VRAM budget at 45 MP (no whole-image RGBA16F intermediates).

---

## 7. Persistence — `frl:ops` in the `.xmp` sidecar

- **One sidecar per image** (the existing `<file>.xmp`), extended. The op stack serializes under a
  ferrolite namespace into the same `rdf:Description` that carries `xmp:Rating`:
  `frl:version` + an `frl:ops` payload (a compact structured/JSON-in-attribute encoding of the
  `OpStack`). The exact on-disk encoding is an implementation detail chosen during the plan; the
  **contract** is: a pure `serialize(&OpStack) -> String` / `deserialize(&str) -> Option<OpStack>`
  round-trip, version-tolerant (unknown/older version or absent payload → `OpStack::default()`,
  i.e. unedited).
- **Merge-preserving**, exactly like the rating writer: foreign nodes (including any `crs:` from
  other editors) survive verbatim; a malformed sidecar is backed up to `.xmp.bak` then rewritten
  fresh (reuses the existing `xmp.rs` machinery, extended to also carry `frl:ops`).
- **Read on open:** parse `frl:ops` → hydrate the OpStack → build the DAG dirtied to those params.
- **Write off the UI thread** via the job system, same optimistic pattern as ratings
  (`spawn_metadata_write`-style): in-memory edit is immediate; the persist job follows; a
  `MetadataResult`-style event reports success/warning. Nothing slow on the UI thread (CLAUDE.md §1).
- **Catalog stays a cache (contract §2):** the source of truth is the sidecar. A small
  `images.has_edits BOOLEAN` cache column lets the grid/filmstrip show an "edited" badge without
  parsing every sidecar; it is rebuildable by re-reading sidecars (cache invariant), so a missing
  DB never loses edits.

---

## 8. Develop UI (design-system Develop module)

The Develop module already has (Spec 1.6) the top filmstrip + filter row and the bottom
current-image metadata bar; the **left panel stays hidden in Develop**. Spec 2 adds the **right
296px adjustment panel** and the canvas-level editing affordances.

### 8.1 Right adjustment panel (296px)
Restyled `CollapsingHeader` sections (design-system §6), each `EguiSlider` bound to one op's
params; editing emits a new `OpStack` → marks the node dirty → repaints:
- **Basic** — Exposure (EV, bipolar), Contrast (bipolar), White Balance Temp + Tint.
- **Tone Curve** — a custom painted **interactive** curve widget (drag control points); its
  point-editing math (add/move/clamp/sort, monotone enforcement) is a **pure tested unit**.
- **HSL** — an 8-band swatch row + Hue/Sat/Lum `EguiSlider`s per selected band.
- **Detail** — Sharpening (Amount + Radius).
- **Geometry** — crop/rotate (paired with the canvas overlay, §8.4): Angle slider, Aspect preset
  dropdown, plus the overlay handles.
- Each section has a **Reset** affordance; a global reset clears the whole stack.

### 8.2 Undo/redo
A **bounded history ring of `OpStack` snapshots** (immutable stacks make this trivial and cheap).
`Ctrl+Z` / `Ctrl+Y` (when no text field has focus) move through history; applying a history entry
re-points the DAG and dirties changed nodes. The history logic (push/coalesce rapid same-op
edits/undo/redo/bounds) is a **pure tested unit** independent of egui. History is per-open-image
and not persisted (only the resulting OpStack persists).

### 8.3 Before/After
`\` (and a toolbar button) toggles rendering the **empty stack** vs the **current stack** on the
preview (a full swap, not a split — split is Spec 3). Cheap: evaluate `OpStack::default()` vs the
live stack through the same DAG output.

### 8.4 Interactive crop overlay
A canvas overlay (shown when the Geometry section is active): draggable crop rectangle with
8 handles, a rule-of-thirds grid, a rotate handle, and aspect-ratio presets
(Original/Free/1:1/3:2/4:3/16:9). All **hit-testing + handle-drag + aspect-constraint math is a
pure tested unit** independent of egui; the egui layer only routes pointer events into it. Crop
defines the displayed extent; rotate is applied as a sampling transform at the head of the
per-tile/preview pass (rotate resampling is a halo consumer, §5.1).

---

## 9. Error handling

- **GPU pass / device-surface loss** → wgpu error scopes → recreate `GpuContext` + pipelines +
  VT pools, re-run the preview and re-request visible tiles (reuses Spec 1 recovery). Pipelines
  are rebuilt once on recovery, not per edit.
- **Tile-producer OOM** → shrink the pool budget + backpressure pending production (as Spec 1).
- **Malformed / unknown-version `frl:ops`** → treated as `OpStack::default()` (unedited); the
  malformed sidecar is backed up to `.xmp.bak`, as the rating path does. Never panics.
- **Sidecar write failure** → status-bar warning; the in-memory OpStack is kept (the catalog
  `has_edits` flag still reflects intent), consistent with Spec 1.5's sidecar-failure behavior.
- **Fallback (rung-1 / non-RGGB) images** → edit at **preview-res only**; the full-res tiled edit
  needs the pyramid source, so 1:1 shows the preview-res edit (logged), never panics.
- **Job panics** are caught at the existing worker boundary; one bad edit/tile never downs the pool.

---

## 10. Testing (TDD; CLAUDE.md gate, then hold for the author's visual test)

**Pure CPU logic (run on every OS in CI — the 80%+ target):**
- `OpStack` model: `set_op` immutability, identity/reset, per-op reset, fixed canonical order.
- Serialization: `serialize`/`deserialize` round-trip; version tolerance (unknown/absent → default).
- `frl:ops` XMP merge: write preserves foreign nodes **and** `xmp:Rating`; read recovers the stack;
  malformed → backup + fresh (extends the existing `xmp.rs` tests).
- `tile_with_halo`: size `(TILE_SIZE+2·halo)²`, edge-clamping, `halo = 0` ≡ `tile`.
- Edited-tile cache: version keying, invalidation on edit, lazy re-stream selection for a viewport.
- Param→uniform conversions: WB temp/tint→multipliers, curve→256-LUT (monotone), contrast
  gain/pivot, HSL band packing, sharpen amount/radius→weight+halo.
- Undo/redo history: push/coalesce/undo/redo/bounds.
- Crop overlay: handle hit-testing, drag with aspect constraint, rotate-handle math.
- Develop nav unchanged (`neighbor_in_set` already tested in Spec 1.6).

**Golden-image GPU diffs (auto-skip when `GpuContext::headless()` is `None`, per Spec 1's CI
constraint — `cargo test --workspace` must stay green headless):**
- Each WGSL op vs a committed reference at a fixed param (exposure, WB, contrast, curve, HSL,
  sharpen, geometry).
- A **full-stack** render golden (several ops composed).
- A **tile-seam** golden: `Sharpen` applied via the per-tile haloed producer matches the
  whole-image result within tolerance at tile borders — the halo-correctness proof.
- Goldens authored/verified locally on the dev GPU (RTX 3060/3070 class).

**egui UI** (adjustment panel, crop overlay rendering, undo/redo wiring, before/after): `cargo
build` + clippy + the author's hands-on visual test. No golden tests for egui rendering.

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → **then STOP and hold for the author's (Jann's) visual test of the
running app** before finishing the branch (CLAUDE.md "Finishing a branch" rule).

---

## 11. Decomposition into implementation plans

Build order = dependency order; each plan is its own writing-plans → TDD cycle but all on the one
`feat/editing-pipeline` branch.

1. **Pipeline foundation.** `ferrolite-pipeline` crate: `OpStack` model + serialization (pure,
   tested), the edit DAG on `Graph<PipelineImage>` + `SourceNode`, and the **point-op** WGSL
   compute passes (exposure, WB, contrast) with golden tests. Preview-tier render of a
   point-op stack into the existing rung-1 display.
2. **Remaining ops.** Tone curve (+LUT), HSL (8-band), Sharpening (unsharp), geometry
   (crop/rotate) compute passes + their param→uniform units + goldens.
3. **VT halo + GPU tile producer.** `tile_with_halo` + `ferrolite-image` halo math; the
   `TileProducer` seam; the edit producer (haloed source sampling, no readback); edited-tile
   versioned caching + lazy re-stream; the tile-seam golden. Wires the full-res 1:1 edited view.
4. **Develop UI + persistence.** The 296px adjustment panel + all sections, interactive
   tone-curve + HSL + crop-overlay widgets, undo/redo + reset, before/after, and `frl:ops`
   sidecar read/write wired through the off-thread persist + read-on-open path + `has_edits` badge.

---

## 12. Decisions recorded (resolved during brainstorming, 2026-06-30)

| Question | Decision | Rationale |
|---|---|---|
| Interactive recompute model | **Preview-res interactive + per-tile full-res via the VT** | Instant slider response (only screen-res is ever seen while dragging); full-res deferred to 1:1 inspection; matches the existing VT + two-tier load; makes halo a per-tile concern. |
| VT halo scope | **Build halo infra + ship one real neighborhood op (unsharp-mask Sharpening)** | Honors the architecture-map directive to add halos this phase and exercises the path end-to-end in the real UI (none of the other listed ops is a neighborhood op). |
| Full-res edit path | **Tiled on the GPU with halo, no CPU readback** (not one whole-image pass) | Makes halo load-bearing, fits the 45 MP VRAM budget (no whole-image RGBA16F intermediates), maximizes engine-learning transfer — consistent with the full-4-rung-VT rationale in Spec 1. |
| Edit-stack persistence | **Custom `frl:` namespace in the existing `.xmp` sidecar** (merge-preserving) | One sidecar per image, co-located with `xmp:Rating`, full control over our schema; Adobe `crs:` interop is a non-goal (foreign nodes merely preserved). |
| GPU pass type | **WGSL compute passes** | The proposal's stated primary learning surface. |
| Develop UI extras | **Undo/redo + per-section reset, Before/After toggle, interactive crop overlay** (all in) | Editor-grade UX expected for a real Develop module; the immutable OpStack makes history cheap. |
| Executor changes | **None** — use the existing `Graph<O>` with `O = PipelineImage` | Honors contract §4: the executor stays photo/wgpu-agnostic; the pipeline supplies the edit nodes. |
| Scope of the spec | **One spec, ~4 implementation plans** on one branch | Mirrors Spec 1's plan decomposition; keeps each plan reviewable. |
```
