# ferrolite — Mask perf: brush-component merge + adjustment-cache fix (design)

> **Status:** Design — pending user review, then writing-plans.
> **Date:** 2026-07-07
> **Branch:** `fix/brush-mask-perf` (continues the incremental-composite + highlight work).
> **Builds on:** `2026-07-07-mask-overlay-incremental-and-highlight-design.md` (per-component
> coverage cache + batched fold — both shipped and working), `2026-07-05-p1-masking-design.md`
> (§4.3 the brush rasterizer + the `StrokeCursor` incremental-stamping the engine defined but never
> wired), and the GPU-overlay + O(1)-key work earlier on this branch.
> **Goal:** make brush painting AND per-mask adjustment (Exposure/Contrast/…) sliders smooth on
> large masks, by (1) not re-compositing masks when only adjustments change, and (2) stopping the
> unbounded growth in component count caused by one-component-per-stroke.

---

## 1. Measured diagnosis (this is a re-diagnosis with new data)

Profiling a ~200-component mask (`FERROLITE_BRUSH_PROFILE`), interleaved overlay (`composite_cached`)
and preview (`local_node`) lines:

| Observation | Evidence | Meaning |
|---|---|---|
| Overlay per-component cache **works** | brush frame: `composite_cached … evaluated=1 reused=199 eval=4 ms` | Only the growing stroke re-evals — the incremental cache is correct. |
| **Preview re-composites ALL masks on EVERY change, at full res** | exposure drag: `local_node rebuild=true components=199 composite=12751 ms`; brush: `… composite=3409 ms` | **12.7 s** for an adjustment that didn't touch the masks. Dominant lag. |
| Overlay `fold` spikes to ~1.2 s | `fold=1189 ms` under load vs `fold=32 ms` standalone | The fold is ~32 ms; the 1.2 s is **GPU contention** from the concurrent 12.7 s preview composite. |

**Root causes:**
1. **`LocalAdjustmentsNode` invalidates its mask cache on `cm.layers != *layers`** (`local_node.rs`),
   and `layers` bundles the masks *and* the `AdjustmentSet`. So changing Exposure re-composites every
   mask at full resolution, every frame → 3–13 s. (My earlier "preview is cheap / non-goal" call was
   wrong at this scale — it was measured on a small mask.)
2. **Component count grows without bound:** `route_brush` calls `add_component` once per *stroke*, so
   painting N strokes yields N `Brush` components; every composite (overlay and preview) is O(N), and
   N only grows. This is the fundamental scaling wall behind "brush lag got worse."

The overlay eval-cache (shipped) is fine; the fixes target the preview node's cache key and the
component-count growth.

---

## 2. Fix A — preview mask cache keyed on mask definitions only

`LocalAdjustmentsNode`'s composited-mask cache must depend on the **mask definitions**, not the
adjustments. The mask composite is a pure function of the `MaskDefinition`s (+ dims + input); the
`AdjustmentSet` only feeds the *apply* pass, which is O(visible layers) and already cheap.

- Change `CachedMasks` to store the mask definitions used (e.g. `mask_defs: Vec<MaskDefinition>`,
  or a cheap hash of them) instead of comparing the whole `LocalAdjustments`.
- Rebuild the masks only when `mask_defs != current` or `full_dims` changed. An adjustment-only
  change → `rebuild = false` → reuse cached masks → re-run only the apply passes.
- Result: dragging Exposure/Contrast/etc. on a 200-component mask goes from ~12.7 s/frame to the
  apply cost (a handful of passes) — effectively instant. This also removes the GPU contention that
  inflated the overlay fold.

This is behavior-preserving (identical output; only *when* masks recompute changes) and needs a
golden proving an adjustment-only change reuses masks (see §6).

## 3. Fix B — brush strokes merge into one component (bounded N) + explicit "New Brush Layer"

### 3.1 Merge by default
- `route_brush` no longer creates a new component per stroke. Instead a committed stroke **appends
  to the mask's active `Brush` component**; if none exists, the first stroke creates it. The active
  brush component is the mask's most-recent `Brush` component (see §3.3 for how "New Brush Layer"
  changes it). Paint and erase strokes coexist in that one component (`Stroke { erase }`).
- Component count is now bounded by *how many brush layers the user deliberately creates*, not by how
  many strokes they paint. Composites stay O(small).
- **Live stroke (in-progress):** while dragging, the in-progress stroke is previewed by appending it
  to the active brush component's stroke list (as today, but into the existing component rather than
  a throwaway new one); on release it is committed into that component.

### 3.2 Brush evaluation is one pass per erase-run (not one pass per stroke)
Today `MaskCompositor`'s `Brush` eval loops `for st in strokes { stamp_onto(acc, dabs_of_st) }` — one
GPU pass **per stroke**. With merging, a brush component can hold many strokes, so this must not be
O(strokes):
- Evaluate a `Brush` component by walking its strokes in order and **batching consecutive strokes of
  the same `erase` flag into a single `stamp_onto`** (the dab shader already composites a whole dab
  buffer in one dispatch). Order is preserved (paint/erase runs stay ordered), so the result is
  identical to the per-stroke loop; the pass count drops from O(strokes) to O(erase-runs) — typically
  1–2.
- The existing per-component `ComponentCache` still re-evaluates the brush component when it grows
  (its hash changes as dabs are added), but that re-eval is now a couple of passes over all dabs (one
  dispatch each) rather than hundreds — cheap enough for interactive painting.

### 3.3 "New Brush Layer" — the explicit split (button + keybind)
- A **"New Brush Layer"** action creates a fresh empty `Brush` component in the selected mask and
  makes it the active brush target, so subsequent strokes accumulate there and it is independently
  deletable/hoverable in the Components modal. This gives the deliberate, manageable brush-group
  ("layer") model without a full layers panel (explicitly out of scope — a layers UI doesn't help and
  can hurt perf; the masks list already provides mask-level visibility/rename/delete).
- **UI:** a button in the mask/Components panel. **Keybind:** a new rebindable `Action`
  (e.g. `NewBrushLayer`) — per the repo's load-bearing keybind rules: the button tooltip shows the
  key via `Keymap::hint`, the `Action` is added to a Settings keyboard `GROUPS` entry (enforced by
  `every_action_is_in_a_settings_group`) **and** the Help panel shortcut list.

### 3.4 Existing large masks (migration)
- No automatic migration: existing masks keep their components. But because new strokes append to the
  active (last) brush component rather than adding new ones, an existing mask **stops growing**, and
  Fix A already removes its adjustment lag. A "Merge brush components" convenience action is a
  possible future item, not in this scope.

## 4. What stays (already shipped this branch)
- Per-component coverage cache (`composite_cached`) + batched fold — unchanged; with bounded N they
  are cheap, and the overlay hover-highlight keeps working (now highlighting brush *layers*).
- The overlay GPU tint + native-texture path, the O(1) rebuild key, the generation-counter input id.

## 5. Deferred (explicitly out of scope; revisit only if measure-after says so)
- **Incremental `StrokeCursor` stamping** (stamp only *new* dabs onto a persistent brush buffer):
  §3.2's one-pass-per-erase-run makes a full re-stamp a couple of cheap dispatches, so this
  optimization is deferred; wire it only if measure-after shows the active-brush re-eval is still hot.
- **Fold-accumulator caching** (O(1) fold): the fold is ~32 ms at N≈200 once contention is gone, and
  N is now bounded/small, so the O(N) batched fold is fine. Deferred.
- **A full layers panel** (reorder / per-component show-hide) — separate feature, orthogonal to perf.
- **`LocalAdjustmentsNode` per-component incremental composite:** unnecessary once masks are cached
  across adjustment changes (Fix A) and N is bounded (Fix B); a brush-frame mask change re-composites
  the (few) components once, cheaply.

## 6. Testing (TDD; CLAUDE.md gate, then hold for the author's visual test)
**Pure/CPU logic:**
- Brush-merge in `route_brush`'s pure helpers: appending a committed stroke targets the active brush
  component (not a new one); "New Brush Layer" makes a fresh component active; the model transitions
  are unit-tested where egui-free (mirror the existing `mask_affordance`/`mask_edit` unit tests).
- Erase-run batching produces the same dab ordering/grouping as the per-stroke loop (pure grouping
  function tested).
- `NewBrushLayer` action is in a Settings `GROUPS` entry (the existing
  `every_action_is_in_a_settings_group` test enforces it) and has a Help entry.

**Golden GPU diffs (auto-skip headless):**
- **Fix A:** compositing masks, then changing only an `AdjustmentSet`, does NOT recompute masks
  (assert the cached `MaskBuffer`s are reused — e.g. via a rebuild-count hook or pointer identity)
  while the applied output still reflects the new adjustment.
- **Erase-run batched brush eval == per-stroke-loop brush eval** for a multi-stroke, mixed paint/erase
  `Brush` component (correctness of §3.2).
- Merged brush (one component, many strokes) coverage == the same strokes as N separate add-mode
  components (proves merge is visually equivalent to what the user had).

**Measure-after (the proof):** re-run `FERROLITE_BRUSH_PROFILE` on a large mask: (a) dragging an
Exposure slider shows `local_node rebuild=false` (masks reused) and small frame time; (b) painting
shows bounded component count and small `composite_cached`/`local_node` times that do NOT grow with
strokes painted. Then remove the temporary instrumentation.

**egui UI:** build + clippy + the author's hands-on visual test (button, keybind, painting feel,
per-layer hover-highlight, adjustment-slider smoothness).

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → **hold for Jann's visual test**.

## 7. Decisions recorded (2026-07-07)
| Question | Decision | Rationale |
|---|---|---|
| Adjustment-slider lag | **Key preview mask cache on mask-defs only** (Fix A) | Masks are independent of adjustments; re-compositing them on an Exposure change is pure waste (12.7 s measured). |
| Brush component growth | **Merge strokes into one active Brush component; explicit "New Brush Layer" to split** (hybrid) | Bounds N so composites stop scaling with strokes painted; keeps deliberate, deletable brush groups without a layers UI. |
| Brush eval cost after merge | **One `stamp_onto` per erase-run, not per stroke** | A merged brush holds many strokes; per-stroke passes would reintroduce O(strokes). One dispatch composites a whole dab batch. |
| Split affordance | **Button + rebindable keybind** (`NewBrushLayer`), full discoverability | Matches the repo's keybind rules; discoverable + fast. |
| Incremental stamping / fold-accumulator / layers panel | **Deferred / out of scope** | Bounded N + Fix A make them unnecessary now; revisit only if measure-after shows a remaining hot spot. |
| Existing large masks | **No auto-migration; new strokes append (stop growth), Fix A fixes their adjustment lag** | Avoids changing the user's existing intent; growth stops going forward. |
