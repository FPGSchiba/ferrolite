# ferrolite — Curve spline modes + reusable curve component — Design

> **Status:** Approved design (brainstorm complete). Ready for a `writing-plans` cycle.
> **Date:** 2026-07-04
> **Branch:** `feat/spec4-1-ux-polish` (same branch as Spec 4.1 — this rides alongside the
> tone-curve overhaul just completed).
> **Follows:** Spec 4.1 §3.E (tone-curve interaction overhaul) — this extends that work with
> interpolation modes and extracts the widget into a reusable component.

---

## 1. Summary

The tone curve currently interpolates control points **piecewise-linearly** (`ferrolite-pipeline`
`uniforms::curve_interp`, baked to a 256-entry LUT) and the app widget draws a matching straight
polyline. This feature adds a **Smooth** interpolation mode (monotone cubic Hermite) alongside
**Linear**, and refactors the interactive curve widget into a **reusable component** so future
per-channel color curves can reuse it. Two crates change: `ferrolite-pipeline` (the interpolation
+ a persisted `mode`, photo tier) and `ferrolite-app` (the reusable widget + a mode selector).

**Explicitly out of scope (YAGNI):** per-channel RGB/color curves — the component is *made ready*
for them, but none are wired now.

---

## 2. Fixed constraints

- **Tiers:** `ferrolite-pipeline` and `ferrolite-app` are both **photo tier** — the new
  interpolation is pure math with **no new dependencies** and **no copyleft**. No engine-tier crate
  (`ferrolite-image`/`-gpu`/`-vt`) is touched.
- **Cross-cutting contracts:** the tone-curve op stays a `ferrolite-pipeline` node on the unchanged
  `Graph<PipelineImage>`; the `mode` is an **additive** field on the existing `ToneCurve` op,
  persisted in the existing `.xmp` OpStack serde sidecar (merge-preserving). The GPU still applies a
  256-entry LUT — only the CPU-side LUT bake changes.
- **CLAUDE.md:** per-control reset for the mode selector; no UI-thread blocking (LUT bake is the
  same cheap 256-entry CPU pass already done on edit); green gate then hold for the author's visual
  test.
- **Back-compat:** existing sidecars have no `mode` key — they MUST load as **Linear** (today's
  exact behavior). New curves created in-app default to **Smooth**.

---

## 3. Design

### 3.1 `CurveMode` + interpolation (`ferrolite-pipeline`, photo tier)

- New `#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)] pub enum CurveMode { Linear, Smooth }`
  in `op.rs`, with `impl Default for CurveMode { fn default() -> Self { CurveMode::Linear } }`
  (Linear default = legacy back-compat).
- `ToneCurve` gains `pub mode: CurveMode` with `#[serde(default)]` on the field, so pre-feature
  sidecars (no `mode`) deserialize to `Linear`.
- `uniforms.rs`: the private `curve_interp(pts, x)` gains a mode:
  - **Linear** — the existing piecewise-linear branch, unchanged.
  - **Smooth** — **monotone cubic Hermite** interpolation with **Fritsch–Carlson** tangent
    limiting (guarantees monotonic, no overshoot beyond the data range). Endpoints clamp like today
    (flat outside `[pts.first.x, pts.last.x]`).
  - The existing final "force non-decreasing" clamp on the 256-LUT stays as a safety net.
- `curve_lut` becomes `pub fn curve_lut(points: &[(f32,f32)], mode: CurveMode) -> [f32; 256]`.
- **Expose interpolation to the app:** make the interpolation reachable from `ferrolite-app` for
  rendering — `pub mod uniforms;` (or a targeted re-export `pub use uniforms::curve_lut;` /
  `pub fn curve_sample(points, mode, x) -> f32` in `lib.rs`). The app renders the curve by sampling
  this — single source of truth, display == applied.

### 3.2 Reusable curve widget (`ferrolite-app`, photo/app tier)

New `ferrolite-app/src/widgets/curve.rs` — a self-contained interactive curve editor extracted from
`develop/curve_widget.rs`. It owns ALL the interaction from Spec 4.1 §3.E (reliable grab via
`grab_or_insert`, hover highlight, insert-when-far, click-to-select, the three delete gestures,
per-control "Reset" button) plus the new mode selector.

```rust
/// Visual style for a curve editor instance (enables future per-channel reuse).
pub struct CurveStyle {
    pub curve_color: egui::Color32,   // e.g. theme::ACCENT for tone; R/G/B later
    pub point_color: egui::Color32,
    // (label/title handled by the caller's section header)
}

/// One frame's result. `None` when nothing changed.
pub struct CurveEdit {
    pub points: Vec<(f32, f32)>,
    pub mode: ferrolite_pipeline::CurveMode,
    pub reset: bool,   // the Reset button was clicked (caller resets the op)
    pub commit: bool,  // this change should push undo history / persist
}

/// Draw + interact. `id_source` disambiguates multiple curves on one screen
/// (tone vs. future R/G/B). Renders the curve by sampling the pipeline's
/// interpolation for `mode` so the displayed shape matches the applied LUT.
pub fn curve_editor(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    points: &[(f32, f32)],
    mode: ferrolite_pipeline::CurveMode,
    style: &CurveStyle,
) -> Option<CurveEdit>;
```

- The **mode selector** is a small segmented control (Linear | Smooth) rendered under the curve,
  next to the "Reset" button. It has its **own per-control reset** (back to the default mode) —
  reuse the shared reset affordance so it stays consistent (CLAUDE.md per-control-reset rule).
- Pure point math stays in `develop/curve_math.rs` (or moves beside the widget) — unchanged logic
  (`grab_or_insert`, `insert_point`, `move_point`, `delete_point`, `is_identity`).
- `develop/curve_widget.rs` becomes a **thin adapter**: it calls `widgets::curve::curve_editor`
  with the tone curve's points+mode+`CurveStyle{ACCENT}`, maps the returned `CurveEdit` to the
  existing `EditOutcome` (build `Op::ToneCurve(ToneCurve{points, mode})`, or `stack.reset` on reset).

### 3.3 Tone Curve wiring + defaults

- The Tone Curve section reads `stack.tone_curve()` → `(points, mode)`; passes them to the widget;
  writes back `Op::ToneCurve(ToneCurve { points, mode })`.
- **New-curve default = Smooth:** when the widget first turns an identity curve into a real edit
  (first inserted point), the adapter constructs the op with `mode = CurveMode::Smooth`. Legacy
  sidecars (no mode) still load `Linear` via serde default — so existing edits are visually
  unchanged, new edits are smooth by default. The user can switch either way; the choice persists.

---

## 4. Testing

**Pure unit tests (`ferrolite-pipeline`):**
- Monotone cubic: passes through every control point (LUT at a control x equals its y within LUT
  quantization); output is monotonic non-decreasing for monotonic control points; **no overshoot**
  (`0 ≤ lut[i] ≤ 1`, and no value exceeds the local control-point envelope); endpoints flat.
- `curve_lut(pts, Linear)` reproduces today's LUT exactly (regression guard on the existing tests).
- `curve_lut(pts, Smooth)` differs from Linear on a bent curve but matches at control points.
- **Serde back-compat:** a `ToneCurve` JSON without a `mode` field deserializes with
  `mode == CurveMode::Linear`; round-trip with `mode` preserves it.

**Golden test (`ferrolite-pipeline/tests`):** add a Smooth-mode tone-curve golden beside the
existing `tone_curve_darken_midtones_matches_golden` (Linear) so the applied result is pinned.

**Pure/app:** `grab_or_insert` and point-math tests carry over unchanged. The widget interaction +
mode toggle + smooth rendering are **visual-tested** (held for the author).

---

## 5. Files touched

- `ferrolite-pipeline/src/op.rs` — `CurveMode` enum; `ToneCurve.mode` field.
- `ferrolite-pipeline/src/uniforms.rs` — `curve_interp`/`curve_lut` gain the mode; monotone-cubic.
- `ferrolite-pipeline/src/lib.rs` — expose `CurveMode` + the interpolation/LUT publicly.
- `ferrolite-pipeline/src/serialize.rs` + `tests/golden.rs` — mode in fixtures; Smooth golden.
- `ferrolite-app/src/widgets/curve.rs` — **new** reusable `curve_editor`.
- `ferrolite-app/src/develop/curve_widget.rs` — thin adapter over the reusable widget.
- `ferrolite-app/src/develop/curve_math.rs` — unchanged pure math (possibly re-homed).

---

## 6. Risks

- **Monotone-cubic correctness** is the one place to get right — use Fritsch–Carlson so a monotonic
  control set never overshoots (a naive Catmull-Rom would). Covered by the no-overshoot test.
- **Exposing `uniforms`** widens `ferrolite-pipeline`'s public surface slightly; keep it to the
  curve interpolation (a `pub fn curve_sample`/`curve_lut`), not the whole uniforms module, if
  practical.
- **Default-mode split** (new=Smooth, legacy=Linear) is the subtle bit: verify a legacy sidecar
  loads Linear and a freshly-created curve is Smooth, both via the same `mode` field.

---

## 7. Out of scope

- Per-channel RGB/color curves (the component is made ready; none wired now).
- Per-point corner/smooth toggles (single mode per curve).
- Any GPU/LUT-format change (still a 256-entry LUT lookup).
