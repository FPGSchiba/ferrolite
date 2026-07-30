# ferrolite — Unified Maskable Adjustments: layer document, adjustment registry & fused GPU layer engine (design)

> **Status:** Design — approved in brainstorming (2026-07-28); pending user final review of this spec, then writing-plans.
> **Date:** 2026-07-28
> **Branch:** `feat/ui-v2-rewrite`
> **Context:** The V2 UI rewrite consolidated the Develop panel into a modular tool/tab system, but
> global edits (typed `Op`s, one shader pass each) and per-mask edits (a flat Light+Color
> `AdjustmentSet` applied by one `local-adjust` pass) remain two divergent implementations. The V2
> design (`docs/design/V2/README.md`) specifies that Adjust and Mask share the exact same options
> library — this design closes that gap for good.
> **Proves:** Every current and future pixel adjustment is implemented ONCE, registers as
> optionally maskable, and appears in both Adjust (global) and Mask (per-mask) modes from that
> single implementation — one place to change, one dispatch path to test. As a by-product, the
> fused GPU layer engine reduces point-op full-texture passes from ~7 per re-render to 1 per layer.

---

## 1. Goal & requirements

1. **Single implementation per adjustment.** A new tool (e.g. Clarity) is: parameter fields on the
   shared `AdjustmentSet`, one registry descriptor + render function, one shader hook. It then
   works globally AND per-mask with no second implementation.
2. **All pixel adjustments are ultimately maskable.** Light (exposure/contrast/HL/shadows/whites/
   blacks, temp/tint), tone curve, HSL, color grading, vibrance/saturation, color swatch, sharpen,
   noise reduction, dehaze, and future neighborhood ops (clarity/texture). Geometric tools stay
   global-only: Crop/Geometry, Lens correction. Heal is inherently spatial and stays its own tool.
3. **Shader wiring is phased.** Point ops get per-mask shaders in this effort; neighborhood ops
   (sharpen/NR/dehaze/clarity) appear in Mask scope greyed with a hover reason
   (`mask_shader_ready: false`) until their per-layer pass lands in a follow-up phase.
4. **Breaking persistence change is acceptable** (user decision 2026-07-28). Old `frl:ops`
   payloads are treated as "no edits"; stored bytes are left untouched.
5. **Performance must improve, provably.** The fused layer engine is gated on a golden-render
   parity suite and a before/after benchmark (see §6/§7); the bar is no regression anywhere and
   ≥2× on early-op slider drags.

### Non-goals

* No new adjustment tools in this effort (the registry makes them cheap later).
* No changes to mask *definition* machinery (`ferrolite-mask` brush/gradient/range/composite,
  `MaskCompositor`, the definition-keyed mask cache) — the engine consumes the same `MaskBuffer`s.
* No re-generation of the `docs/design/V2/*.dc.html` mockups (README carries deltas — see §8).

## 2. Document model (`ferrolite-pipeline`, breaking)

The persisted edit document (`frl:ops`, new `STACK_VERSION`) becomes:

```text
EditDoc
├── geometry ops (global-only, shape unchanged): CropRect/Geometry, LensCorrection
├── global: AdjustmentSet                        ← "the layer with no mask"
└── layers: Vec<MaskLayer { name, visible, mask: MaskDefinition, adjustments: AdjustmentSet }>
```

* `AdjustmentSet` grows from today's flat Light+Color subset into the single full parameter block:
  exposure, contrast, highlights, shadows, whites, blacks, temp, tint, `ToneCurve` (point +
  parametric), `Hsl` (8 bands), `ColorGrade` (4 wheels + blending/balance), vibrance, saturation,
  color swatch (now also available globally), sharpen (amount/radius), noise reduction
  (lum/detail/color/color-detail), dehaze (amount/radius), reserved clarity/texture.
* Every field is zero-identity with `#[serde(default)]` — the struct stays schema-stable forward;
  this is the LAST breaking bump this area needs.
* The old `Op::Exposure`/`Op::ToneCurve`/… enum entries disappear as document entries.
* **Ordering semantics (fixed):** the geometric/lens passes keep their current positions in the
  execution chain (lens vignette early, geometry/crop last — see §4); among the adjustment
  layers, the global set applies first, then mask layers in stack order. Application order INSIDE
  an `AdjustmentSet` is fixed and canonical (today's chain order). No per-layer op reordering.
* **Old payloads:** version mismatch ⇒ identity doc ("no edits"); stored bytes untouched (nothing
  destroyed; a converter remains possible later). Catalog `has_edits` recomputes on next
  open/save.

## 3. Adjustment registry & scoped UI (`ferrolite-app`)

A small scope enum threads through the options library:

```rust
enum EditScope { Global, Mask(usize) } // index into EditDoc::layers
```

One level below the existing `DevelopToolRegistry` (tools stay as they are), each adjustment
registers once:

```rust
struct AdjustmentDescriptor {
    id: AdjustmentId,          // stable, e.g. "tone_curve"
    tab: TabId,                // "light" | "color" | "effects"
    section: SectionId,        // e.g. BASIC, TONE_CURVE, SHARPENING, DEHAZE
    maskable: bool,            // false ⇒ Global scope only
    mask_shader_ready: bool,   // false ⇒ greyed in Mask scope with hover reason
    show: fn(&mut egui::Ui, ScopedEdit<'_>) -> Option<EditOutcome>,
}
```

`ScopedEdit` resolves the scope to the right `AdjustmentSet`, and edits produce a new `EditDoc`
immutably (today's `EditOutcome` pattern, doc-shaped).

* **Adjust tool** renders the registry with `EditScope::Global` — all entries.
* **Mask tool** renders the mask-management header (create/AI chip/visibility, mask list,
  invert/rename/delete, components + brush controls — today's `mask_panel` top block), then the
  accent "Editing: Mask N — adjustments apply only inside this mask" line, then the SAME tab row
  rendering the SAME registry with `EditScope::Mask(i)`. Entries with `maskable: false` don't
  appear; `maskable && !mask_shader_ready` render greyed with a hover reason. The separate "Mask"
  tab and its duplicate compact slider set are deleted.
* **Load-bearing conventions preserved:** per-control reset stays inside each render function via
  the shared `EguiSlider` reset column / `draw_reset_arrow` — reset writes identity into the
  scoped set, so global and per-mask reset are the same code. Collapsible open/closed state is
  tracked separately for Adjust vs. Mask scope (V2 README). Icons via `icons.rs`; keybind hints
  via `Keymap::hint`.
* The bespoke widgets (ToneCurveWidget, HSL, ColorGradingWheel) re-point from `OpStack` accessors
  to scoped `AdjustmentSet` fields — same widgets, no visual change. The `ops_edit` setter family
  collapses into scoped setters on `EditDoc`.

## 4. GPU layer engine (`ferrolite-pipeline`, the perf win)

Current preview DAG (post 2026-07-28 merge): source → color-matrix → vignette → exposure → WB →
contrast → tone-curve → HSL → color-grade → local-adjust → dehaze → sharpen → geometry — each
point op a separate full-texture pass. `LocalAdjustmentsNode` already fuses Light+Color per layer
in one pass (ping-pong textures). Target:

```text
source → color-matrix → [lens/vignette] →
  layer-engine(global, coverage ≡ 1) →            ← ONE fused point-op pass
  [neighborhood passes: dehaze, sharpen, NR]      ← global, between segments
  layer-engine(mask₁) → layer-engine(mask₂) → …   ← one fused pass per visible mask
  geometry → display tail
```

* The fused pass evaluates the full `AdjustmentSet` in registers per pixel (exposure → temp/tint →
  contrast → tone regions → curve LUT → HSL → grade → vibrance/saturation → swatch), blended by
  mask coverage. Global binds a 1×1 white mask or a specialization constant. This is today's
  `local-adjust` shader grown to the full set.
* **Perf rationale:** an early-op drag (Exposure) currently re-evaluates 7+ downstream
  full-texture passes; after fusion it re-runs ~3 (fused + neighborhood + geometry). Point-op
  passes are bandwidth-bound, so fewer passes ≈ proportionally less memory traffic. Each mask
  layer costs one cheap pass. The retained-DAG dirty tracking stays — fewer, fatter nodes.
* **LUTs:** tone-curve (3×256) and HSL tables per layer in one shared storage buffer with
  per-layer offsets; only the dirty layer's slice is rebuilt (existing `curve_lut`/
  `tone_curve_luts` helpers).
* **Neighborhood ops** stay discrete nodes; global ones run between the global layer pass and the
  mask passes (today's order). Per-mask versions phase in later, each as "heavy map shared, cheap
  masked apply" — dehaze's whole-image transmission texture is the template. Until then those
  controls are greyed in Mask scope.
* **Mask compositing untouched:** `MaskCompositor` + definition-keyed mask cache carry over.
* **Both tiers, one implementation:** `EditPipeline` (preview) and `TileEditPipeline` (full-res
  tiles/export) share the layer-engine node and uniform packing, as they share passes today. The
  `produce_full` drag deferral and warm/tile caching are orthogonal and survive unchanged.
* **Fallback:** if the fused pass regresses on constrained GPUs (register pressure), split into
  two fused segments — still far fewer than 7 passes.

### 4.1 Phase 4 amendment (2026-07-29) — per-mask neighborhood passes landed

`.superpowers/sdd/2026-07-29-unified-engine-phase4-mask-neighborhood/` implemented the
"Per-mask versions phase in later" line above for dehaze and sharpen (NR/clarity remain
deferred — no algorithm exists in any scope yet). The chain above is updated as follows,
without rewriting the history it documents:

* **Dehaze recovery is no longer a discrete node.** The standalone `DehazeRecoveryNode`
  (`(I−A)/t′ + A`, previously its own full-texture pass between the transmission node and
  the color layer-engine pass) was fused INTO the layer-engine's Color-stage dispatch as
  its first per-pixel step — one fewer full-res read+write round trip per evaluate. The
  whole-image transmission map (`DehazeTransmissionNode`) stays its own heavy node (nothing
  about the shared-map cost model changes); only the recovery *apply* step moved. Updated
  chain:

  ```text
  source → color-matrix → [lens/vignette] →
    layer-engine(global, coverage ≡ 1, dehaze-recovery fused in) →
    [dehaze-transmission: shared whole-image map, still discrete] →
    layer-engine(mask₁, own dehaze-recovery + own amount) → layer-engine(mask₂) → … →
    sharpen(global apply, then one masked apply per active mask layer) →
    geometry → display tail
  ```

* **Sharpen is separable and layered, not fused-per-mask-pass.** Rather than a discrete
  node per mask layer (as the original bullet's "one fused pass per visible mask" implied
  for neighborhood ops generally), `SharpenNode` computes ONE shared O(r) two-pass
  (horizontal + vertical) box blur per DISTINCT radius among {global ∪ active mask layers},
  then dispatches one global apply followed by one masked apply per active layer — all
  within the node's own single command encoder/submit. This mirrors dehaze's existing
  "heavy map shared, cheap masked apply" template exactly, as the original bullet
  anticipated, but the "cheap apply" is itself now multiple small dispatches (one per
  active layer) rather than a single global one.
* **Per-mask dehaze RADIUS stays global-only** (the shared transmission map has exactly
  one radius); per-mask dehaze AMOUNT and per-mask sharpen (amount + radius) are fully live.
* Parity/perf evidence for both changes: `docs/benchmarks/2026-07-28-phase3-fused-engine.md`'s
  "Phase 4 increments" section; fixture coverage: `mask_dehaze`/`mask_sharpen` in
  `ferrolite-pipeline/tests/common/layer_engine.rs`.

**Cascade semantics (author-accepted 2026-07-29):** overlapping dehaze applications (global +
per-mask, or multiple masks) compound multiplicatively — each layer's recovery runs on the
already-recovered content. Verified visually on a two-mask + global stress case: extreme but
predictable ("one needs to know where and how to apply dehazing; if it overlaps it is applied
two times or more"). Accepted as designed; no clamping added.

## 5. History, undo & persistence plumbing

* `History<OpStack>` → `History<EditDoc>`; same per-gesture sealing (`EditOutcome { doc, commit }`),
  same cap, one entry per committed gesture regardless of scope. Undo restores the whole prior doc.
* `kind: OpKind` generalizes to `(scope, AdjustmentId)` for rebuild/coalescing decisions;
  `needs_full_rebuild` compares the fields that force a producer rebuild (geometry/lens/
  dehaze-radius) on the new struct.
* Mask UI state (selection, overlay-on, in-flight gestures) stays out of history, as today.
* `serialize.rs` writes `EditDoc` under the bumped version; load of an old version returns the
  identity doc without error.

## 6. Testing

1. **Golden-render parity gate (load-bearing).** Before the engine swap, render a fixture set
   through the CURRENT chain — stacks exercising every op (exposure-only, curve+HSL, grading,
   dehaze+sharpen, 2-mask Light+Color combos) — and commit the PNGs. The layer engine must
   reproduce them within a small per-channel tolerance. This is what makes the hot-path rewrite
   safe to attempt.
2. **Registry invariants** (spirit of `every_action_is_in_a_settings_group`): unique
   `AdjustmentId`s; every descriptor's tab exists; every maskable-and-ready control renders in
   both scopes; every descriptor's reset returns identity (mechanically catches a forgotten
   per-control reset).
3. **Doc model:** serde round-trip incl. unknown-field tolerance; `is_identity` across scopes;
   per-control reset immutability; old-version payload → identity doc without error.
4. **Scoped-edit dispatch** (the "one spot with logic to test"): edits via `EditScope::Global` vs
   `Mask(i)` land in exactly the right `AdjustmentSet`; undo restores both scopes.
5. **Perf gate:** `docs/benchmarks/` method, before/after on the same fixture RAW —
   (a) slider-drag re-render latency at fit and 1:1 for an early op (Exposure) and a late op
   (Grading); (b) open-to-first-edit; (c) same with 3 masks active.
   **Bar: no regression anywhere; ≥2× on the early-op drag case.**

## 7. Rollout phases (within `feat/ui-v2-rewrite`)

1. **Doc model + serialization** — `EditDoc`/full `AdjustmentSet`, adapter accessors so the UI
   behaves identically; visual no-op.
2. **Registry + scoped tabs** — Mask mode gains the full options library; point ops work per-mask
   via the extended `local-adjust` pass; neighborhood controls greyed in Mask scope.
3. **Fused layer engine swap** — behind the golden parity gate + perf gate.
4. **Per-mask neighborhood passes** — one op at a time (sharpen → NR → dehaze/clarity), un-greying
   as each lands. May be split into a follow-up effort.

Each phase ends gate-green with a visual test plan for the author (CLAUDE.md).

## 8. Workstream 2 — UX feedback pass (after the big change lands)

Pattern proven by `systematic-ui-fixes-round-2/3`, extended to close the loop into the design docs:

1. **Structured walkthrough** (`docs/superpowers/2026-XX-XX-v2-ux-walkthrough.md`), prepared once
   the unification is merged and gate-green: per screen (Library; Develop = Adjust scope, Mask
   scope with the shared options library, Crop, filmstrip/navigation; Export; chrome/Settings/
   Help). Each item: exact steps, what to JUDGE (discoverability, friction, visual rhythm, feel of
   drag/reset/collapse), blank verdict line, named fixtures where needed (hazy shot for dehaze,
   high-ISO for NR). Includes the fresh surfaces this design creates (greyed-with-reason clarity;
   whether the Editing-Mask banner makes scope unmistakable).
2. **Author annotates** while running the app; freeform notes, structure guarantees coverage.
3. **Feedback → `systematic-ui-fixes-round-4` spec → fixes**, executed like Rounds 2/3 (plan →
   implementation → gate → author re-test of changed items).
4. **Living design frame:** every accepted UX change is reflected into `docs/design/V2/README.md`
   (layout/interaction/token prose). The `.dc.html` mockups are not re-generated; the README
   carries deltas and notes when a change invalidates a mockup.

## 9. Decisions log (brainstorming 2026-07-28)

| Decision | Choice |
| --- | --- |
| Maskable scope | All pixel adjustments; Crop/Geometry/Lens/Heal global-only |
| Duplication bar | Single implementation; registry with `maskable` flag; one dispatch path to test |
| Persistence compat | Breaking is fine; old payloads read as "no edits", bytes untouched |
| Shader wiring | Phased; not-yet-ready mask controls greyed with hover reason |
| Architecture | Approach 2 — unified layer document + registry + fused GPU layer engine (chosen over pragmatic split execution because fusion also improves performance, and over UI-only unification because it fails the duplication bar) |
| Workstream order | Big change first, UX feedback pass after |
