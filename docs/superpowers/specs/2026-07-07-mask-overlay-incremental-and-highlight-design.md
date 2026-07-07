# ferrolite — Mask overlay: incremental composite + component highlight (design)

> **Status:** Design — pending user review, then writing-plans.
> **Date:** 2026-07-07
> **Branch:** `fix/brush-mask-perf` (continues the GPU-overlay work already on this branch).
> **Builds on:** `2026-07-07-brush-mask-overlay-gpu-design.md` (the GPU tint overlay that removed the
> readback) and `2026-07-05-p1-masking-design.md` (the mask engine + `MaskCompositor`).
> **Goal:** make brush-mask painting AND component-slider edits smooth on large masks (100–500+
> components), and let the user identify which component in the Components modal maps to which
> coverage on the canvas.

---

## 1. Problem & measured diagnosis (measure-before-fix)

After the GPU-tint overlay landed, painting on a large mask was still very laggy. Re-profiling with
`FERROLITE_BRUSH_PROFILE` on a **~192-component** mask, per dragged frame:

| Cost/frame | Steady-state | Verdict |
|---|---|---|
| **`overlay_texture`** (composite all components + tint) | **~90–180 ms**, spikes to 450 ms | **DOMINANT — the lag** |
| `preview ep.evaluate()` (preview edit pipeline) | **~0.3–2 ms** (rare cold-start spike) | Not the bottleneck |

**Root cause (confirmed):** `MaskOverlayCompositor` re-composites **all N components from scratch
every frame**. For each component it allocates a mask texture, uploads a zeroed buffer
(`alloc_zeroed` → `write_texture`), runs the shape/brush GPU pass, and issues a **separate
`queue.submit`**; then it folds all N buffers. At N≈192 that is ~0.6 ms/component ≈ 120 ms/frame of
UI-thread encode + upload + submit overhead, scaling linearly with component count → ~8 fps stutter.

**Why the preview pipeline is NOT slow:** `LocalAdjustmentsNode` already caches its composited masks
(`CachedMasks`), so it does not re-composite every frame. Only the overlay path lacks caching. The
fix therefore targets **only the overlay compositor**; the preview node is left untouched.

During a brush stroke, only **one** component changes (the in-progress stroke). During a
component-slider edit, only **one** component changes (the edited one). In both hot-paths, N−1
components are static — yet all N are re-evaluated every frame. That redundancy is the target.

---

## 2. Fix: per-component coverage cache (incremental evaluation)

Cache **each component's evaluated coverage buffer individually**, keyed by a cheap structural hash
of that component's parameters. Each overlay build:

1. For each component in order, compute its cheap params hash. If it matches the cached hash for that
   slot, **reuse** the cached `MaskBuffer` (no eval). Otherwise **re-evaluate** just that component
   and update the slot.
2. **Fold** the (mostly cached) per-component buffers by their `CompositeMode` (+ invert) into the
   final coverage — batched into **one** command encoder + submit.
3. Tint the folded coverage red (the existing GPU tint pass) → the overlay texture.

Result per frame:
- **Painting a new stroke** (hot component = last): re-eval 1 + fold N. **O(1) eval.**
- **Editing an existing component's params** (hot component = any index): re-eval 1 + fold N. **O(1)
  eval**, uniformly — this is why the per-component cache is chosen over a "base prefix + live tail"
  scheme (which would only help the painting/last-component case).

The expensive part (alloc + zeroed upload + shape/brush pass + submit) becomes incremental for both
hot-paths; only the cheaper fold stays O(N).

### 2.1 Eliminating the per-component overhead in the eval + fold
While restructuring, also remove the constant-factor waste the profile exposed, so even the
one-time full recompose (mask switch, first build) and the O(N) fold are cheap:
- **One encoder + one submit** for the whole fold (and, where practical, batch the re-evals of dirty
  components into that encoder) instead of a `queue.submit` per component/pass.
- **Avoid the zeroed-buffer upload:** initialize mask buffers via a clear/`LoadOp::Clear` (or a
  cheap clear pass) rather than `alloc_zeroed`'s full `write_texture` of a CPU zero vector.

### 2.2 Cache invalidation & lifetime
- The cache lives on `MaskOverlayCompositor` (rebuilt-once object on `ViewerState`), as a
  `Vec<CachedComponent { hash: u64, coverage: MaskBuffer }>` plus the last folded result.
- **Slot alignment:** cache slot `i` corresponds to component `i`. If the component count changes
  (add/remove) or a slot's hash differs, that slot (and the fold) recompute; unaffected slots are
  reused. A count change simply grows/shrinks the `Vec` and re-folds.
- **Input dependence:** `LumaRange`/`ColorRange` sample the overlay input image; their cached
  coverage is invalid if the input changes. The input (`mask_overlay_input`, a bounded downscale) is
  already rebuilt only when `preview_source` changes; fold the input's identity/generation into the
  invalidation so a new input clears the whole cache.
- **Component hash:** a cheap, allocation-free structural hash of the component's params (NOT
  `serde_json`). For `Brush`, hash node count + node fields; for shapes, their scalar params; for
  range, samples + thresholds. Hashing is arithmetic over params (O(total params)), microseconds at
  these sizes — negligible vs a GPU eval.
- **`invert`** is applied in the fold (unchanged), not per component.

### 2.3 Scope of change
- All caching lives inside `ferrolite-mask` (`MaskCompositor`) or a thin cache wrapper in
  `ferrolite-pipeline`'s `MaskOverlayCompositor`. Decided in the plan; kept generic/photo-agnostic.
- `MaskCompositor::composite` remains available for the non-cached callers (the preview
  `LocalAdjustmentsNode` keeps its own `CachedMasks` path — unchanged). The overlay uses the new
  incremental/caching entry point.
- **`LocalAdjustmentsNode` is not modified** (measured cheap; non-goal).

---

## 3. Feature: hover-highlight a component

Let the user see which Components-modal row corresponds to which coverage on the canvas.

- **Interaction:** hovering a component row in the Components modal highlights that component's
  coverage **in white** on the canvas overlay, and **bolds that row's text** in the modal. Move the
  pointer away → both clear. Transient, no clicks. Applies to **any** component type (brush,
  linear, radial, luma range, color range) uniformly.
- **Mechanism (reuses §2's per-component cache — free):** the modal reports the hovered component
  index via `MaskUiState` (e.g. `highlight_component: Option<usize>`). When set, the overlay does a
  second, small draw: take that component's **already-cached** coverage buffer, tint it white
  (premultiplied), and draw it over the red overlay (same native-texture path, or a second native
  texture / a second tint into the same target). Because the coverage is cached, highlighting adds
  no composite work — one extra tint of one buffer.
- **Row bold:** in `mask_components_modal.rs`, detect row hover (`ui.rect_contains_pointer` /
  `response.hovered()`), render that row's label with `egui::RichText::strong()`, and set
  `mask.highlight_component = Some(i)` for the frame; clear it when no row is hovered.
- **Overlay-off interaction:** hovering a row draws the white component-coverage highlight
  regardless of the red-overlay toggle (so hovering always answers "which one is this"), but does
  NOT force the full red overlay on. Documented + visual-tested.
- The white tint reuses the tint pass with a white (1,1,1) premultiplied color + a highlight
  strength (e.g. 0.7 for clear contrast over the 50% red). Exact strength tuned in the visual test.

---

## 4. Data flow (per frame, Mask tool active)

```
rebuild_mask_overlay_if_needed:
  key unchanged & cache warm ────────────────► return (no work)         [most frames when idle]
  key changed (stroke/edit):
    for each component i:
      hash_i == cache[i].hash ? reuse cache[i].coverage : re-eval + store  [O(1) evals in hot-paths]
    fold all coverage buffers (batched, 1 submit) → red tint → overlay texture
  highlight_component = Some(h):
    tint cache[h].coverage white → draw over red                          [free; cached]
```

---

## 5. Error handling
- **Cache slot / count mismatch** (add/remove/reorder): treat a non-matching or out-of-range slot as
  dirty → re-eval; never index out of bounds. A full clear on input change or mask switch.
- **`highlight_component` out of range** (component deleted while hovered): bounds-check → no
  highlight, no panic.
- **Empty / degenerate mask:** unchanged from today (identity/zeroed coverage → transparent tint).
- **Device loss:** the tint pipeline + compositor rebuild once on recovery (as today); the
  per-component cache is dropped and rebuilt.
- Nothing here adds a UI-thread readback or `poll(Wait)` (CLAUDE.md §1); pipelines built once
  (CLAUDE.md §2).

## 6. Testing (TDD; CLAUDE.md gate, then hold for the author's visual test)
- **Pure logic:** the per-component params hash — stable for unchanged params, differs when any
  param changes; brush node append changes the hash (so the growing stroke re-evals) while unrelated
  components' hashes are stable. Cache slot dirty/reuse selection given a hash list.
- **Golden GPU diff:** the incrementally-composited coverage equals the from-scratch full composite
  for the same def (correctness: caching must not change the image) — for a multi-component def, and
  after mutating one component (the cache path must match a fresh composite). Auto-skips headless.
- **Highlight:** the white-tint of a single component's coverage vs a reference (premultiplied
  white, alpha = coverage·strength).
- **Measure-after:** re-run `FERROLITE_BRUSH_PROFILE` on the 190+-component mask; confirm
  `overlay_texture` per frame is now flat/small (independent of component count) while painting AND
  while dragging a component slider. Then remove the temporary instrumentation.
- **egui UI** (modal row bold + hover wiring): build + clippy + the author's hands-on visual test.
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
  `cargo test --workspace` green → **hold for Jann's visual test**.

## 7. Non-goals
- Touching `LocalAdjustmentsNode` / the preview pipeline (measured cheap; already cached).
- O(1) fold for editing an early-index component (sequential composite can't reorder); the fold
  stays O(N) but cheap. Intermediate-accumulator caching is a *possible future* follow-up only if the
  batched fold proves too slow at very high N (verified by measure-after).
- Persisting the highlight / any change to mask semantics or the sidecar (highlight is transient UI
  state only).

## 8. Decisions recorded (2026-07-07)
| Question | Decision | Rationale |
|---|---|---|
| What to optimize | The **overlay compositor only** | Profile: overlay 90–180 ms/frame dominant; preview pipeline already cached (~0.3 ms). |
| Caching strategy | **Per-component coverage cache** (re-eval only the changed component) | Makes the expensive eval incremental for BOTH painting and slider edits; also the substrate the highlight needs. |
| Fold | Stays O(N), batched into one submit; zeroed-upload removed | Sequential composite can't reorder; fold is cheap vs eval. Accumulator caching deferred. |
| Highlight interaction | **Hover row → white coverage on canvas + bold row**, any component type | Fastest "which one is this before I delete it"; no clicks; reuses the cached per-component coverage for free. |
| Highlight vs overlay toggle | Highlight draws the white component coverage regardless of the red toggle | Hovering should always answer the question, even with the red overlay off. |
