# ferrolite — P1: Masking & local-adjustments engine (design)

> **Status:** Design — approved by user (2026-07-05); pending writing-plans.
> **Date:** 2026-07-05
> **Parent:** `2026-07-05-ferrolite-v2-architecture-map.md` (§2 settled decisions, §3 licensing
> tiers, §5 six cross-cutting contracts, and the **P1** phase entry — read first for the settled
> seams). This is **v2 phase P1**, the masking backbone P5/A2/A3/P7 all build on.
> **Builds directly on:** `2026-06-30-spec2-editing-design.md` (the edit DAG on
> `ferrolite-gpu::Graph`, the `OpStack` + `.xmp` sidecar, the **VT tile halo + GPU tile
> producer**, and the two-tier preview-res/full-res recompute) and
> `2026-07-01-spec3-color-and-export-design.md` (the fixed canonical op order and the working-space
> linear pipeline masked adjustments slot into).
> **Proves:** local adjustments end-to-end — create a mask (brush / linear gradient / radial
> gradient / luminance range / color range), composite components with add/subtract/intersect,
> adjust a per-mask Light+Color set with sub-frame preview response, inspect at 1:1 tiled on the
> GPU, and have every mask + adjustment persist to the `.xmp` sidecar and reload on next open.
> **UI target:** the **Develop module** — the toolbar's **Mask/Grad** placeholders become one
> unified Masking tool (design system `docs/design/ferrolite-design-system.md` §6, 296px right
> panel). **Heal stays inert** (P5's charter).
> **Branch:** `feat/p1-masking-engine` (off `main`).

---

## 1. Goal & validation

Stand up local (masked) adjustments end-to-end:

> enter the Masking tool → create a mask and add components (brush stroke, linear/radial gradient,
> luminance range, color range) → combine components with **add / subtract / intersect** → adjust
> the mask's **Light** (Exposure, Contrast, Highlights, Shadows, Whites, Blacks) and **Color**
> (Temp, Tint, Saturation, Hue, Color) set with sub-frame slider response on the preview → toggle
> a colored mask overlay → inspect the result at 1:1 (full-res, tiled, on the GPU, seam-free) →
> reset any single control on its own → undo/redo → the whole mask stack persists to the `.xmp`
> sidecar and reloads on the next open.

Local adjustments are **non-destructive**: masks are parametric definitions in an ordered stack
stored in the sidecar and replayed on the GPU edit pipeline; rasterized mask buffers are
re-derivable caches, never the source of truth. This phase promotes **image quality** exactly as
far as the local point-ops require (v2 map D1) — the *neighborhood-dependent* local adjustments
(Texture, Clarity, Dehaze, Sharpness, Noise) are explicitly deferred to the phases that own their
machinery (P3/P4) and appear here only as greyed, reserved controls.

---

## 2. Scope

**In:**
- **`ferrolite-mask` (new, engine tier)** — the photo-agnostic mask machinery: the `MaskComponent`
  parametric vocabulary, the composited `MaskDefinition`, WGSL shape evaluators (linear gradient,
  radial gradient, luminance range, color range) + **add/subtract/intersect** compositing compute,
  the **brush-stroke rasterizer** (dab stamping), the single-channel mask buffer vocabulary, and
  the **imported-raster (AI) seam** (defined + serialized, no producer).
- **`ferrolite-pipeline`** — a new `Op::LocalAdjustments` holding an ordered `Vec<MaskLayer>`; the
  `MaskLayer { mask, adjustments }` + `AdjustmentSet` types; the point-op **Light + Color** WGSL
  adjustment passes; the DAG node that applies an `AdjustmentSet` through a composited mask; op-order
  insertion after `Hsl`; source-space coordinate handling; `frl:ops` sidecar encoding.
- **`ferrolite-vt`** — brush-stroke mask buffers stream as a generic large source through the
  existing source-agnostic tile path (reuses the Spec 2 halo + GPU tile-producer seam). No new
  photo concepts.
- **`ferrolite-app`** — the unified **Masking** tool: masks list, per-mask Light+Color adjustment
  section (per-control reset; greyed neighborhood controls), canvas mask overlay, and the brush /
  linear / radial / range tool affordances; undo/redo integration.

**Out (later phases / non-goals):**
- **Heal / clone / spot removal** → P5 (the toolbar Heal placeholder stays inert this phase).
- **AI-generated masks** (SAM 2.1 / SegNext) → A2. P1 defines and serializes the hand-off seam
  only; no `ort`, no weights, no producer.
- **Neighborhood local adjustments** — Texture, Clarity, Dehaze, Sharpness, Noise. Reserved slots
  in `AdjustmentSet`, greyed in the UI; they light up with P3 (dehaze) / P4 (NR + sharpen).
- Per-mask **Tone Curve** and full **8-band HSL** (Lightroom itself has neither locally — the Light
  region sliders + Saturation/Hue/Color cover local tonal/color work).
- **Depth-range masks** (need a depth source) and **luminosity/range auto-masking beyond the two
  range tools** → future.
- Preset/copy-sync of masked edits → P7 (P1 only guarantees the mask data is preset-portable data).

---

## 3. Architecture of the slice

```
ferrolite-app (Develop: unified Masking tool — masks list, per-mask Light+Color sliders,
   │           canvas overlay, brush/linear/radial/range affordances, undo/redo)
   │  pointer (display space) → inverse-map to source coords → edit MaskLayer/AdjustmentSet
   │  → new OpStack → mark LocalAdjustments node dirty → repaint
   │
   ├── ferrolite-pipeline (photo tier)
   │     Op::LocalAdjustments(Vec<MaskLayer>)  [new op, inserted after Hsl]
   │     MaskLayer { mask: ferrolite_mask::MaskDefinition, adjustments: AdjustmentSet }
   │     LocalAdjustmentsNode: for each layer → (engine) composite mask → (photo) apply
   │        Light+Color point-ops through the mask → accumulate; output PipelineImage
   │     frl:ops sidecar encoding of the LocalAdjustments payload
   │
   ├── ferrolite-mask (NEW, engine tier — permissive deps only; NO copyleft, NO weights)
   │     MaskComponent { LinearGradient, RadialGradient, LumaRange, ColorRange,
   │                     Brush{strokes}, Imported{handle, provenance} }
   │     MaskDefinition = ordered Vec<(MaskComponent, CompositeMode)>  (+ invert)
   │     WGSL: shape evaluators · add/subtract/intersect compositing · brush dab rasterizer
   │     Mask buffer vocabulary (single-channel R32F tile/texture)
   │
   ├── ferrolite-gpu ── Graph<PipelineImage> retained executor (UNCHANGED, contract 4) +
   │     GpuContext; the mask compositing/shape/brush passes are supplied as generic nodes
   │
   ├── ferrolite-vt ── brush-stroke buffer streams as a generic large source (contract 5),
   │     reusing the Spec 2 tile halo + GPU tile-producer seam
   │
   └── ferrolite-catalog::xmp ── frl:ops persistence (merge-preserving; catalog stays a cache)
```

**Licensing tiers (map §3) preserved.** `ferrolite-mask` is **engine-transferable**: a gradient,
an ellipse falloff, a range threshold, a brush dab, an add/subtract/intersect composite, and a
single-channel mask buffer are all photo-agnostic — the crate carries **no copyleft deps and no
model weights**, so the masking subsystem lifts into the author's game engine as a unit (D7). The
*photo meaning* — which adjustment applies through which mask, and the working-space point-op math —
lives in `ferrolite-pipeline`. The generic `Graph<PipelineImage>` executor is **not modified**
(contract 4); mask compositing and adjustment application are supplied as **nodes**. Brush buffers
ride the **source-agnostic** VT (contract 5). `ferrolite-ai` is **not** a dependency of
`ferrolite-mask` (D6) — the AI seam is an inert data variant here.

---

## 4. `ferrolite-mask` — the engine crate

### 4.1 Parametric mask vocabulary (source of truth; pure data, `Clone`, (de)serializable)
All shapes are defined in **normalized source coordinates** ([0,1]² over the pre-geometry image),
so masks stay anchored to image content across crop/rotate/aspect (§5.2).

- `enum MaskComponent`
  - `LinearGradient { start: Vec2, end: Vec2 }` — a linear ramp; mask = clamped projection of the
    pixel onto the start→end axis. `start`/`end` carry the gradient extent (feathered band).
  - `RadialGradient { center: Vec2, radius: Vec2, rotation: f32, feather: f32, invert: bool }` —
    an ellipse falloff.
  - `LumaRange { lo: f32, hi: f32, softness: f32 }` — a smooth band over the **post-Hsl** luma.
  - `ColorRange { samples: Vec<Rgb>, tolerance: f32, softness: f32 }` — smooth color-distance
    selection over the post-Hsl color.
  - `Brush { strokes: Vec<Stroke> }` where `Stroke { nodes: Vec<BrushNode>, erase: bool }` and
    `BrushNode { pos: Vec2, radius: f32, hardness: f32, flow: f32 }` (pressure reserved). The
    parametric strokes are the source of truth; the rasterized buffer is a cache (§4.3).
  - `Imported { handle: RasterHandle, provenance: MaskProvenance }` — the AI/external seam (§7);
    inert in P1 (no producer).
- `enum CompositeMode { Add, Subtract, Intersect }`.
- `struct MaskDefinition { components: Vec<(MaskComponent, CompositeMode)>, invert: bool }` — the
  first component seeds the accumulator; later components fold in by their mode. Empty = full
  (identity mask) or empty depending on `invert`; documented and tested.

### 4.2 Shape evaluation & compositing (generic GPU nodes, contract 4)
- One WGSL evaluator per parametric shape, each writing a single-channel `R32F` mask value in
  `[0,1]` **analytically per pixel** from the shape params and the pixel's source-space position
  (and, for range shapes, the sampled input color). No neighborhood → **zero halo** for
  gradient/radial/luma/color.
- A compositing pass folds components by `CompositeMode`: `Add` = `max`/additive-clamped, `Subtract`
  = `a * (1 - b)`, `Intersect` = `min`/multiplicative (exact operators chosen + unit-tested in the
  plan). `invert` = `1 - m` at the end.
- All of this is supplied to the executor as generic nodes; the *math* is photo-agnostic.

### 4.3 Brush-stroke rasterizer + VT streaming (contract 5)
- Strokes rasterize by **dab stamping**: each `BrushNode` contributes a radial falloff
  (`hardness`/`flow`), accumulated along the stroke polyline. The rasterizer is a generic compute
  pass over a single-channel buffer.
- **Preview tier:** a preview-res mask texture. **Full-res tier:** the brush buffer streams as a
  generic large source through `ferrolite-vt`, reusing the Spec 2 halo + GPU tile-producer seam.
  The only halo in the whole masking stage is here: **halo = max dab radius**, so a dab straddling
  a tile border rasterizes completely.
- **Incremental stamping while painting:** only the *new* dabs since the last pointer sample are
  stamped onto the cached buffer — no full re-raster per pointer move (CLAUDE.md §1).

### 4.4 Buffer vocabulary
- A single-channel `R32F` mask tile/texture type in the engine vocabulary (`ferrolite-image` or
  `ferrolite-mask` — decided in the plan; kept generic). Cheap-to-clone `Arc<wgpu::Texture>` handle,
  consistent with `PipelineImage`.

---

## 5. `ferrolite-pipeline` — local adjustments in the DAG

### 5.1 The op & the adjustment set
- `Op::LocalAdjustments(LocalAdjustments)` with `struct LocalAdjustments { layers: Vec<MaskLayer> }`
  and `struct MaskLayer { name: String, visible: bool, mask: MaskDefinition, adjustments: AdjustmentSet }`.
- `struct AdjustmentSet` — **point ops only** in P1, each an `Option`/zero-identity scalar with its
  own reset:
  - **Light:** `exposure`, `contrast`, `highlights`, `shadows`, `whites`, `blacks`.
  - **Color:** `temp`, `tint`, `saturation`, `hue`, `color` (a tint/overlay swatch).
  - **Reserved (greyed in UI, no shader in P1):** `texture`, `clarity`, `dehaze`, `sharpness`,
    `noise` — the type carries the fields so P3/P4 wire them without a schema break.
- Highlights/Shadows/Whites/Blacks are smooth **tonal-region gains** (a parametric tone response),
  point-ops in working-space linear; this is new tonal math P3 can later reuse globally.

### 5.2 Op order & coordinate space (source-anchored)
- New canonical order (the DAG is built once per open, only dirtied on edits — Spec 2 §4.2):
  `Source → ColorMatrix → Exposure → WhiteBalance → Contrast → ToneCurve → Hsl →
  **LocalAdjustments** → Sharpen → LensCorrection → Geometry → [output: working-space linear]`.
- `OpKind` gains `LocalAdjustments` after `Hsl`; the discriminant values renumber (Sharpen/
  LensCorrection/Geometry shift up). **Safe:** `OpKind` is a sort key, never serialized; `Op`
  serializes by serde variant name, so renumbering does not touch the sidecar format.
- **Masks are source-anchored:** shapes and strokes are stored in normalized source coordinates,
  so crop/rotate/aspect/lens (all *after* `LocalAdjustments`) never slide a mask relative to
  content. Range masks read the post-`Hsl` graded image (what the user perceives).
- **Pointer mapping:** brush/gradient/radial input arrives in **display space**; the app
  inverse-maps it to source coords through the active geometry (crop+rotate — reuse Spec 2's crop
  overlay transform) and lens model (Spec 4.4). **Fallback:** if the lens inverse proves heavy,
  placement treats lens as identity (acceptable at typical distortion magnitudes), logged.

### 5.3 The `LocalAdjustmentsNode`
- One `Node<PipelineImage>` for the whole stage. For each **visible** layer, in order: (engine)
  composite the `MaskDefinition` into an effective single-channel mask; (photo) run the Light+Color
  point-ops and blend adjusted-vs-input by the mask value; feed the result forward as the next
  layer's input. Output feeds `Sharpen`.
- **Per-op invalidation stays free (Spec 2):** editing any slider/mask param updates the node's
  uniforms/mask inputs + `Graph::mark_dirty(node)`; the executor re-runs this node + downstream,
  reusing cached upstream textures. Pipelines are built once and reused (CLAUDE.md GPU rule).

---

## 6. Two-tier recompute (reuses Spec 2 §6)

1. **Preview tier (interactive).** `LocalAdjustments` runs on the single preview-res texture. A
   slider or mask-param change marks the node dirty → re-runs it + downstream — a handful of ~6 MP
   passes, inside frame budget (profiled per CLAUDE.md). No tiling needed.
2. **Full-res tier (1:1).** The VT streams **edited** full-res tiles via the GPU tile producer
   (Spec 2 §5.2): masks evaluate per tile in source coords — parametric/range analytic (zero halo),
   brush point-sampled from the VT-streamed brush buffer (halo = max dab radius applies only to
   brush rasterization). Coarse-LOD fallback (blurry→sharp, never blocks) inherited from Spec 1/2.

---

## 7. Persistence — `frl:ops` (contract 2)

- The `LocalAdjustments` payload serializes under the existing `frl:` namespace in the one `.xmp`
  sidecar, nested in the same `frl:ops` structure as the other ops — **merge-preserving** (foreign
  nodes, `crs:`, `xmp:Rating` survive verbatim), version-tolerant (absent/unknown → identity).
- **Parametric is the source of truth; rasters are caches.** Brush stroke nodes, gradient/radial
  params, range thresholds, and AI **prompts** persist; no rasterized mask ever enters the sidecar.
  A missing catalog never loses masks (rebuildable from sidecars); `images.has_edits` already covers
  the "edited" badge.
- Pure `serialize`/`deserialize` round-trip, unit-tested; malformed → identity + `.xmp.bak` backup
  (reuses the Spec 2 `xmp.rs` machinery).

## 8. AI-mask hand-off seam (design now; producer in A2)

- `MaskComponent::Imported { handle: RasterHandle, provenance: MaskProvenance }` is **defined and
  serialized** in `ferrolite-mask` now, with **no producer** in P1.
  - `MaskProvenance` is a serializable, engine-opaque descriptor — for A2 it carries `{ model_id,
    model_version, prompt }` where `prompt` is click points / box / semantic class. The engine
    **stores but never interprets** it.
  - **Re-derivable (contract 2):** the sidecar stores the *prompt*, not the raster; A2's
    `ferrolite-ai::segment` `Job` (contracts 1/6) rebuilds the raster from the prompt — exactly as
    brush strokes are parametric with the raster as cache.
  - **No AI contamination (D6):** `ort`/weights never touch `ferrolite-mask`; the AI tier hands in a
    raster + descriptor, the mask engine only composites and persists it.
  - **Refine/combine is free:** an `Imported` component is just another entry in a
    `MaskDefinition`, so `Subtract` a brush from a SAM mask or `Intersect` it with a luma range with
    zero extra machinery.
- Defining + serializing the variant now keeps A2 **additive** — no enum break, no sidecar schema
  bump.

---

## 9. Develop UI (design-system Develop module)

### 9.1 Unified Masking tool
- The toolbar's **Mask** and **Grad** placeholders fold into **one** Masking tool (matching modern
  Lightroom; linear/radial gradients are mask component types, not separate tools). **Heal** stays
  inert (P5).

### 9.2 Masks panel (296px right panel, resizable per Spec 3 §9)
- A **masks list**: each `MaskLayer` a row with visibility toggle, invert, rename, delete; a
  **Create New Mask** action; within the selected mask, **Add / Subtract / Intersect** a component
  with any tool (Brush / Linear / Radial / Luma range / Color range).
- **Selected-mask adjustments:** the **Light** (Exposure, Contrast, Highlights, Shadows, Whites,
  Blacks) and **Color** (Temp, Tint, Saturation, Hue, Color) `EguiSlider`s, each with the shared
  **per-control reset** affordance (`draw_reset_arrow` + reset column, CLAUDE.md); the reserved
  neighborhood controls (Texture/Clarity/Dehaze/Sharpness/Noise) shown **greyed with a hover
  reason** (design-system pref).

### 9.3 Canvas overlay + tool affordances
- A toggleable colored **mask overlay** (default red), rendered from the composited mask buffer we
  already build.
- **Brush:** cursor with size/feather/flow; incremental stroke capture. **Linear gradient:** drag
  handles (start/end axis + band). **Radial:** ellipse with resize/rotate handles + feather.
  **Range tools:** eyedropper + threshold/softness sliders (with a live selection preview).
- All hit-testing / handle-drag / threshold / inverse-mapping math is a **pure tested unit**
  independent of egui; egui only routes pointer events in (same discipline as Spec 2's crop
  overlay).

### 9.4 Undo/redo
- Masks live in the `OpStack`, so the Spec 2 bounded history ring of immutable `OpStack` snapshots
  covers mask edits for free; rapid same-target edits (e.g. a slider drag, a brush stroke) coalesce
  into one history entry per gesture (stroke = one entry on commit).

---

## 10. Error handling

- **Nothing slow on the UI thread (CLAUDE.md §1, contract 1):** brush rasterization, full-res tile
  production, and sidecar I/O go to `ferrolite-jobs` with priority + cancellation; navigation / new
  edits cancel superseded tile-production and rasterize jobs.
- **GPU pass / device-surface loss** → wgpu error scopes recreate `GpuContext` + pipelines
  (incl. mask shape/composite/brush passes) + VT pools; pipelines rebuilt **once** on recovery, not
  per edit (reuses Spec 1/2 recovery).
- **Tile-producer / brush-buffer OOM** → shrink the pool budget + backpressure pending production
  (as Spec 2); a failed tile fails that tile with a coarse-LOD fallback, never a crash.
- **Malformed / unknown-version `frl:ops`** → treated as identity (unedited); the sidecar is backed
  up to `.xmp.bak` then rewritten fresh. Never panics.
- **Empty / degenerate masks** (no components, zero-area radial, empty stroke) → identity mask;
  documented + tested; never divides by zero.
- **Fallback (rung-1 / non-RGGB) images** → mask edit at **preview-res only** (the full-res tiled
  edit needs the pyramid source), logged, never panics.
- **Job panics** caught at the existing worker boundary; one bad rasterize/tile never downs the pool.

---

## 11. Testing (TDD; CLAUDE.md gate, then hold for the author's visual test)

**Pure CPU logic (every OS in CI — the 80%+ target):**
- `MaskComponent` / `MaskDefinition` model: component ordering, composite-mode folding semantics,
  `invert`, empty/degenerate handling.
- `LocalAdjustments` / `OpStack`: `set_op` immutability, per-layer + per-control reset, canonical
  order with the new op inserted, `OpKind` renumber does not change serde output.
- Serialization: `frl:ops` round-trip incl. layers/components/strokes/AI provenance; version
  tolerance; XMP merge preserves foreign nodes + `xmp:Rating`; malformed → identity + `.bak`.
- Coordinate mapping: display→source inverse through crop+rotate (and lens, with the identity
  fallback); round-trips within tolerance.
- Brush stroke math: dab spacing/accumulation, incremental-stamp selection, halo = max dab radius.
- Adjustment param→uniform conversions for the Light+Color point-ops.
- Overlay/tool hit-testing: gradient/radial handle drag, range eyedropper/threshold, brush cursor.
- Undo/redo: stroke = one entry on commit; slider coalescing; bounds.

**Golden-image GPU diffs (auto-skip when `GpuContext::headless()` is `None` — `cargo test
--workspace` stays green headless):**
- Each shape evaluator (linear / radial / luma-range / color-range) vs a committed reference.
- Each composite mode (add / subtract / intersect) + invert vs reference.
- Brush rasterization vs reference; **tile-seam** golden: a brush mask via the per-tile haloed
  producer matches the whole-image result at tile borders (halo-correctness proof).
- A **full-stack** golden: a two-layer masked adjustment (e.g. radial exposure + luma-range temp)
  composed through the pipeline.
- Goldens authored/verified locally on the dev GPU (RTX 3060/3070 class).

**egui UI** (masks panel, list, per-control reset, overlay, tool affordances): `cargo build` +
clippy + the author's hands-on visual test. No golden tests for egui rendering.

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → **then STOP and hold for the author's (Jann's) visual test of the
running app** before finishing the branch (CLAUDE.md "Finishing a branch" rule).

---

## 12. Decomposition into implementation plans

Build order = dependency order; each plan is its own writing-plans → TDD cycle, all on the one
`feat/p1-masking-engine` branch.

1. **`ferrolite-mask` foundation.** New engine crate: the `MaskComponent`/`MaskDefinition` model
   (pure, tested) + the WGSL **shape evaluators** (linear/radial/luma/color) and
   **add/subtract/intersect + invert** compositing compute + the single-channel mask buffer
   vocabulary, with shape/composite goldens. No pipeline wiring yet.
2. **Brush rasterizer + VT streaming.** The stroke model + **dab rasterizer** WGSL, incremental
   stamping, and brush-buffer streaming through the Spec 2 VT halo + tile-producer seam; the
   **tile-seam brush golden**.
3. **`ferrolite-pipeline` integration.** `Op::LocalAdjustments` + `OpKind` insertion after `Hsl`;
   `MaskLayer` + `AdjustmentSet`; the **Light+Color point-op** WGSL passes + param→uniform units;
   the `LocalAdjustmentsNode` (composite → apply → accumulate) wired at the correct op order;
   source-space coordinate mapping; the `frl:ops` sidecar encoding + read-on-open; preview + full-res
   recompute + invalidation (painting-stays-preview-until-commit, version bump, region-scoped
   optimization); the full-stack golden.
4. **Develop masking UI.** The unified Masking tool + masks list (create / visibility / invert /
   rename / delete / add-subtract-intersect); the per-mask Light+Color section with per-control
   reset + greyed neighborhood controls; the canvas mask overlay + brush/linear/radial/range
   affordances (pure hit-test units); undo/redo integration.
5. **AI-mask seam.** The `MaskComponent::Imported { handle, provenance }` variant +
   `MaskProvenance` descriptor + serialization + compositing path (no producer, no `ort`);
   forward-compatibility tests proving A2 is additive.

---

## 13. Decisions recorded (resolved during brainstorming, 2026-07-05)

| Question | Decision | Rationale |
|---|---|---|
| Masked-adjustment data model | **One `Op::LocalAdjustments` holding an ordered `Vec<MaskLayer>`**, applied as a single stage | Contains the N-mask multiplicity inside one op payload, so `OpStack` stays one-op-per-kind / fixed-order; one document / sidecar / undo history; mirrors global-then-local in pro editors. |
| Coordinate space & op-order slot | **Source-anchored masks; stage after `Hsl`, before `Sharpen`/`LensCorrection`/`Geometry`** | Masks stay pinned to content across crop/rotate/aspect (the "crop first, then mask" workflow); range masks see the graded image; parametric shapes tile cleanly in source space. |
| Per-mask adjustment set | **Full point-op LR set** — Light (Exposure/Contrast/Highlights/Shadows/Whites/Blacks) + Color (Temp/Tint/Saturation/Hue/Color); neighborhood locals (Texture/Clarity/Dehaze/Sharpness/Noise) **deferred to P3/P4**, greyed | Genuine LR-competitive local tonal/color editing now, reusing/point-op math; keeps neighborhood color science in the phases that own its halo machinery; type grows without a schema break. |
| Mask representation & persistence | **Hybrid — parametric source of truth (brush = stroke nodes), GPU-rasterized buffer as a re-derivable cache** | The only shape consistent with contract 2 (no raster in the sidecar); resolution-independent; every component is per-pixel evaluable so it tiles. |
| Rendering & invalidation | **DAG subgraph (engine composites, photo applies); two-tier reuse; painting stays preview-tier until stroke commit; version-bump invalidation + region-scoped optimization** | Masks add no halo except brush rasterization; painting never churns full-res tiles mid-stroke; matches Spec 2's drag=preview / full-res-deferred model. |
| Engine crate placement | **New `ferrolite-mask` crate** (engine tier) | High cohesion for all mask primitives; keeps `ferrolite-gpu`'s executor lean; lifts into the game engine as a unit; matches the v1 generic-vs-photo boundary (D7). |
| AI-mask hand-off | **Define + serialize `MaskComponent::Imported { handle, provenance }` now; producer in A2** | Keeps A2 additive (no enum/schema break); re-derivable via prompt (contract 2); no `ort`/weights in the engine tier (D6); AI mask combines with manual components for free. |
| Develop UI | **One unified Masking tool** (Mask+Grad folded in); Heal deferred to P5 | Modern-LR UX; gradients are mask components; healing belongs to P5's charter. |
| Executor changes | **None** — reuse `Graph<PipelineImage>` | Honors contract 4: the executor stays photo/wgpu-agnostic; the pipeline supplies the mask/adjustment nodes. |
| Scope | **One spec, 5 implementation plans, one branch** | Mirrors Spec 2/3 decomposition; keeps each plan reviewable. |
```
