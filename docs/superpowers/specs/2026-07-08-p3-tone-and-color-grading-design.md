# P3 — Advanced Tone & Color Grading — Design

> **Status:** Approved design (parent of three implementation plans). **Not a single
> writing-plan.** One P3 spec that decomposes into **three independent `plan → sdd →
> branch → merge` cycles**, each on its own branch off `main`.
> **Date:** 2026-07-08
> **Parent lineage:** v2 architecture map (`2026-07-05-ferrolite-v2-architecture-map.md`,
> **§4 P3**) → this design.
> **Builds on (all merged to `main`):** Spec 2 edit DAG + OpStack sidecar + LUT bake
> (`ToneCurve`), Spec 3 working-space color, Spec 4.1 tone-curve widget overhaul, the
> curve-spline-modes work (`CurveMode` + reusable `curve_editor`), the develop tool
> registry (`PanelTab`), and P1 masking (`LocalAdjustments`/`AdjustmentSet`).

---

## 1. Summary

P3 delivers the v2 map's **Advanced tone & color grading** phase: **parametric + point
tone curves (incl. per-channel R/G/B)**, **color-grading wheels (shadow/mid/highlight +
global, hue-sat-lum)**, and **dehaze** (classical Dark Channel Prior). All three are
**global** stack ops for P3. Everything is a `ferrolite-pipeline` node on the unchanged
GPU executor with **no new dependencies** (pure Rust math). Each feature is an independent
plan on its own branch.

**Scope decisions (settled in the 2026-07-08 brainstorm):**

| # | Decision |
|---|---|
| S1 | **Three plans / three branches**: (1) tone curves, (2) color-grading wheels, (3) dehaze. Each is `plan → sdd → merge` off `main`. |
| S2 | **Tone curves = full Lightroom parity**: Master/Red/Green/Blue point curves (channel selector) **plus** a parametric region editor (Highlights/Lights/Darks/Shadows + split points). |
| S3 | **Color grading = full LR Color-Grading parity**: four wheels (Shadows/Midtones/Highlights/Global), each hue+sat + a luminance slider, plus **Blending** and **Balance** sliders. |
| S4 | **Dehaze = Dark Channel Prior** (He et al.): a bipolar halo/neighbourhood op (negative amount = add haze). |
| S5 | **Global-only for P3.** Per-mask curves/grade/dehaze are **deferred to a follow-up spec ("P3-local")** written after P3 merges (see §7). Each plan writes its pixel math as a **pure, reusable function** so the follow-up reuses it with no rework. |

**Non-goals (P3):** per-mask/local variants of these ops (§7); ML-based dehaze (NG track);
LGG (lift/gamma/gain) grading model — the map fixed the **HSL-per-region** model (S3);
clarity/texture/grain (a future "Effects" phase — but see §6.3 for the tab that will host them).

---

## 2. Shared architecture & cross-cutting conventions

These apply to **all three plans** and must not drift between branches.

### 2.1 Final canonical op order (target for all three plans)

New ops slot into the canonical `OpKind` order (the discriminant order = apply order). The
**final** target order after all three plans merge is:

```
Exposure · WhiteBalance · Contrast · Dehaze · ToneCurve · Hsl · ColorGrade
         · LocalAdjustments · Sharpen · LensCorrection · Geometry
```

- **Dehaze** sits after `Contrast`, before `ToneCurve` (an early "basic"-class correction,
  mirroring LR's Basic-panel placement — it conditions the image before tone/curve work).
- **ColorGrade** sits after `Hsl`, before `LocalAdjustments` (grading is a late,
  post-tone/HSL color pass, matching LR's pipeline position).
- `ToneCurve` keeps its existing position (currently `= 3`); its discriminant shifts when
  `Dehaze` is inserted ahead of it.

**Branch-merge coordination (load-bearing):** the three branches are cut off `main`
independently and will each edit `OpKind`. Because `OpKind` is a **sort key that is never
serialized** (guarded by the existing `opkind_renumber_does_not_change_serde_output` test),
inserting a variant and renumbering the tail is a **mechanical, serde-safe rebase**. Rule:
**whichever branch merges first sets its variant at the position above; each later branch
rebases onto `main` and re-numbers to the §2.1 target order.** No sidecar migration is ever
required by a renumber. Every plan MUST keep/extend that guard test.

### 2.2 Persistence & back-compat (contract 2)

All new state is **additive `#[serde(default)]`** on the op structs, so any sidecar written
before P3 deserializes to today's exact behavior (new fields → identity). The catalog stays
a pure cache; op params live only in the OpStack `.xmp` sidecar.

### 2.3 Contracts honored

- **Contract 4 (GPU executor is photo-agnostic):** every new op is supplied by
  `ferrolite-pipeline` as a **node**; `ferrolite-gpu`'s `Graph<PipelineImage>` executor is
  not modified. Curves = LUT-node extension; grade = per-pixel node; dehaze = halo node.
- **Contract 5 (VT is source-agnostic):** **Dehaze** is a **halo consumer** on the
  source-agnostic VT, exactly the class as Spec 2's `Sharpen` (patch radius drives halo).
  Curves and grade are per-pixel (no halo).
- **Contract 1 (jobs):** any incidental disk/heavy work stays off the UI thread; LUT bakes
  are the same cheap 256-entry CPU pass; dehaze's global atmospheric-light estimate runs on
  the already-decoded preview image, not per-frame on the UI thread (§5.3).
- **No engine-tier edits, no copyleft, no new deps** — all three are pure-Rust math in the
  photo tier (`ferrolite-pipeline` + `ferrolite-app`).

### 2.4 UI conventions (CLAUDE.md, load-bearing)

- **Per-control reset** on **every** new control (each curve channel, each parametric
  slider, each wheel, each grade slider, the dehaze slider) — reuse `widgets::draw_reset_arrow`
  + the `EguiSlider` reset column.
- **Icons** for the new tabs come from the `icons` module (Phosphor aliases) only — no raw
  glyphs, no hand-drawn `Painter` icons. Add semantic aliases in `icons.rs`.
- **No UI-thread blocking / build-once GPU:** pipelines/shaders built once and reused; the
  new color-wheel widget is plain egui vector drawing.
- **Keybind/discoverability:** if a new tool tab gets a keybind, honor the keybind-tooltip
  and Settings/Help discoverability rules; if none, no action needed.

### 2.5 Reusable-math constraint (enables the §7 follow-up)

Each plan MUST expose its core transform as a **pure function** in `ferrolite-pipeline`
(e.g. `curve_lut(...)` already exists; add `parametric_curve_lut(...)`, `color_grade_px(...)`,
`dehaze_recover(...)`), independent of the global-op wiring. The global node calls it; the
future per-mask path (§7) calls the same function. No transform logic may live only inside a
node's `apply`/shader-setup.

---

## 3. Plan 1 — Advanced tone curves

**Branch:** `feat/p3-tone-curves` · **Crates:** `ferrolite-pipeline`, `ferrolite-app`.

### 3.1 Op model (extend `ToneCurve`, back-compat)

Keep the legacy `points` + `mode` fields as the **Master** curve so pre-P3 sidecars are
unchanged; add per-channel curves and the parametric region curve as defaulted fields.

```rust
/// A single point-curve channel (control points + interpolation mode).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct PointCurve {
    pub points: Vec<(f32, f32)>,   // identity = [] or [(0,0),(1,1)]
    #[serde(default)]
    pub mode: CurveMode,           // Linear | Smooth (existing enum)
}

/// Lightroom-style parametric region curve (applied to luminance/all channels).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct ParametricCurve {
    pub highlights: f32,      // [-1,1], 0 = identity
    pub lights: f32,          // [-1,1]
    pub darks: f32,           // [-1,1]
    pub shadows: f32,         // [-1,1]
    pub shadow_split: f32,    // [0,1], default 0.25 (darks|shadows boundary)
    pub midtone_split: f32,   // [0,1], default 0.50
    pub highlight_split: f32, // [0,1], default 0.75 (lights|highlights boundary)
}
// Default = all region values 0, splits 0.25/0.50/0.75 → identity.

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ToneCurve {
    // Master (RGB/luminance) curve — legacy field names, unchanged for back-compat.
    pub points: Vec<(f32, f32)>,
    #[serde(default)]
    pub mode: CurveMode,
    // New in P3 — all #[serde(default)] = identity, so pre-P3 sidecars load unchanged.
    #[serde(default)]
    pub red: PointCurve,
    #[serde(default)]
    pub green: PointCurve,
    #[serde(default)]
    pub blue: PointCurve,
    #[serde(default)]
    pub parametric: ParametricCurve,
}
```

### 3.2 Bake & GPU

- **Composite three final per-channel LUTs**: for channel `k ∈ {R,G,B}`,
  `finalₖ(x) = channelₖ( master( parametric(x) ) )` — parametric first (region shaping),
  then the master (all-channel) curve, then the per-channel curve.
- New pure fn `parametric_curve_lut(&ParametricCurve) -> [f32; 256]` (region weighting with
  smooth falloff across the split points; monotonic, no overshoot) — unit-tested.
- **GPU change (the main work):** the tone-curve shader moves from **one shared 256-LUT** to
  **per-channel R/G/B LUTs** (upload as a 3-row LUT texture or three 1D LUTs; sample the
  matching row per channel). Build-once pipeline; only the uploaded LUT data changes per edit.
- When all four curves + parametric are identity, the op is dropped from the stack (existing
  identity-elision behavior).

### 3.3 UI (Curve tab)

- **Channel selector** (Master / R / G / B) above the curve; each channel drives its own
  `PointCurve` through the existing reusable `curve_editor` (already mode-aware). Curve is
  tinted per channel (R/G/B/neutral).
- **Parametric sub-panel** below: Highlights/Lights/Darks/Shadows sliders + three split
  sliders, with the parametric shape drawn as a read-only overlay curve.
- **Per-control reset:** each channel curve (its existing Reset), each parametric slider, and
  the channel's mode selector.

### 3.4 Tests
- Serde back-compat: a pre-P3 `{"points":..,"mode":..}` loads with R/G/B + parametric identity.
- `parametric_curve_lut` identity at zeros; monotonic; correct region response (raise
  shadows lifts low end only, etc.).
- Per-channel composite: a red-only curve changes R LUT, leaves G/B identity.
- Golden: a per-channel + parametric combined edit.

---

## 4. Plan 2 — Color-grading wheels

**Branch:** `feat/p3-color-grading` · **Crates:** `ferrolite-pipeline`, `ferrolite-app`.

### 4.1 Op model (new `ColorGrade`)

```rust
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct GradeWheel {
    pub hue: f32,   // [0,360) degrees, wheel angle
    pub sat: f32,   // [0,1], distance from center (0 = neutral)
    pub lum: f32,   // [-1,1], region luminance offset
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct ColorGrade {
    pub shadows: GradeWheel,
    pub midtones: GradeWheel,
    pub highlights: GradeWheel,
    pub global: GradeWheel,
    pub blending: f32,  // [0,1], region overlap, default 0.5
    pub balance: f32,   // [-1,1], shifts shadow/highlight midpoint, default 0.0
}
// Default = all wheels neutral (sat 0, lum 0), blending 0.5, balance 0.0 → identity.
```

### 4.2 Math & GPU (per-pixel node, no halo)

- Pure fn `color_grade_px(rgb, &ColorGrade) -> rgb` (also the WGSL kernel's model):
  1. Compute pixel luminance `Y`.
  2. Derive **shadow / midtone / highlight weights** from `Y`, shaped by `blending`
     (region overlap width) and `balance` (moves the shadow↔highlight midpoint).
  3. For each region, convert `(hue, sat)` → a tint color and **add it weighted**; apply the
     region's `lum` offset weighted; the **global** wheel applies uniformly across all `Y`.
- Per-pixel only → no VT halo. Build-once WGSL pipeline; params are uniforms.

### 4.3 UI (new **Grade** tab)

- **New `widgets/color_wheel.rs`**: a hue-sat disc with a draggable thumb (hue = angle,
  sat = radius), plain egui vector drawing, salted by an `id_source` so four instances
  coexist. Built once, **reused 4×** (Shadows/Midtones/Highlights/Global).
- A **luminance slider** under each wheel; **Blending** and **Balance** sliders below.
- **New `GradeTab: PanelTab`** registered in `base_tabs()`; new tab icon aliased in `icons.rs`.
- **Per-control reset:** each wheel (a `draw_reset_arrow` that returns the wheel to
  neutral sat/hue), each lum slider, blending, balance.

### 4.4 Tests
- `color_grade_px` identity when all-neutral.
- Region isolation: a shadows-only tint colors darks, leaves highlights ~unchanged; global
  tints everything; `balance` shifts the split; `blending` widens overlap.
- Golden: a full 3-way + global grade.

---

## 5. Plan 3 — Dehaze (Dark Channel Prior)

**Branch:** `feat/p3-dehaze` · **Crates:** `ferrolite-pipeline`, `ferrolite-app`.

### 5.1 Op model (new `Dehaze`)

```rust
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Dehaze {
    pub amount: f32, // [-1,1], 0 = identity; >0 removes haze, <0 adds haze
}
```

### 5.2 Algorithm (Dark Channel Prior, He et al.)

1. **Dark channel:** per-pixel `min(R,G,B)`, then a local **min-filter over a patch**
   (radius `r`, e.g. 7–15 px) → this is the **neighbourhood/halo** step.
2. **Atmospheric light `A`:** the brightest pixels in the dark channel (top ~0.1%) → a small
   RGB constant (§5.3 on where this is computed).
3. **Transmission:** `t = 1 - ω · darkchannel(I / A)` (ω ≈ 0.95).
4. **Recovery:** `J = (I - A) / max(t, t₀) + A` (`t₀` ≈ 0.1 floor).
5. **Blend by amount:** positive `amount` interpolates source→`J`; **negative** interpolates
   source→a *hazier* synthesis (toward `A` with reduced contrast), giving symmetric add-haze.
- Pure fn `dehaze_recover(px, dark, a, amount) -> px` for the per-pixel recovery/blend;
  unit-tested independent of the GPU passes.
- (Guided-filter refinement of `t` is **out of scope** for P3 — a soft matting/guided-filter
  pass is a possible later quality bump, recorded as an open question below.)

### 5.3 Tiling: atmospheric light `A` is a global stat (the key design point)

`A` is a **whole-image** estimate; computing it per VT tile would be inconsistent and wrong.
Design: estimate `A` **once on the preview-resolution image** (already decoded/available),
and pass it to every tile as a **uniform**. Each tile then only needs the **local
min-filter** (halo-bounded, contract 5) + the shared `A`. The `A` estimate is cheap and runs
off the UI thread / at op-setup, not per frame. The halo footprint = the patch radius `r`
(same plumbing class as `Sharpen`'s radius).

### 5.4 UI (new **Effects** tab)

- A single **bipolar Dehaze slider** (−1..1, reset 0) in a **new `EffectsTab: PanelTab`**
  (chosen over the Light tab to leave a home for future clarity/texture/grain — an "Effects"
  phase). New tab icon aliased in `icons.rs`. Per-control reset on the slider.

### 5.5 Tests
- `dehaze_recover` identity at `amount = 0`.
- On a **synthetic hazy image** (known `A`, added haze), positive amount increases contrast /
  reduces the dark-channel haze; negative amount re-adds haze (round-trip-ish).
- Halo: the min-filter patch radius is reported to the halo plumbing correctly (Spec 2 halo).
- Golden: dehaze at a fixed positive amount on a hazy fixture.

---

## 6. UI placement summary

| Feature | Tab | New widget | New icon alias |
|---|---|---|---|
| Tone curves | **Curve** (existing) — add channel selector + parametric sub-panel | reuse `curve_editor` | — |
| Color grading | **Grade** (new `PanelTab`) | **`widgets/color_wheel.rs`** (new) | yes |
| Dehaze | **Effects** (new `PanelTab`) | reuse `EguiSlider` | yes |

The **Color** tab (8-band HSL) is left as-is — grading is a distinct concept and gets its own
tab rather than crowding HSL.

---

## 7. Follow-up: per-mask curves / grade / dehaze ("P3-local", after P3)

P3 is global-only (S5). A **separate spec, written after P3 merges**, will extend P1's
`AdjustmentSet` so mask layers can carry curve / grade / dehaze adjustments, reusing the pure
math functions each P3 plan exposes (§2.5). Recording it here so the intent is not lost:

- New spec: `docs/superpowers/specs/YYYY-MM-DD-p3-local-curves-grade-dehaze-design.md`.
- Depends on: all three P3 plans merged (their `curve_lut`/`parametric_curve_lut`/
  `color_grade_px`/`dehaze_recover` functions) + P1's mask compositing path.
- Because the math is already pure and tested, that spec is mostly *wiring into the mask
  adjustment set + per-mask UI*, not new algorithms.

---

## 8. Plan decomposition (three `plan → sdd → merge` cycles)

| Plan | Branch | Crates | Depends on | Merge-order note |
|---|---|---|---|---|
| **1 — Tone curves** | `feat/p3-tone-curves` | pipeline, app | main | inserts nothing before it; extends `ToneCurve` in place |
| **2 — Color grading** | `feat/p3-color-grading` | pipeline, app | main | inserts `ColorGrade` after `Hsl` |
| **3 — Dehaze** | `feat/p3-dehaze` | pipeline, app | main | inserts `Dehaze` after `Contrast` (renumbers `ToneCurve`+) |

All three are independent off `main`; the only coupling is the **serde-safe `OpKind`
renumber** at merge time (§2.1). Each plan: TDD pipeline math → GPU/UI wiring → workspace
gate green → **hold for the author's visual test** with a per-plan checklist (CLAUDE.md).

Recommended execution order: **Plan 1 → Plan 2 → Plan 3** (independent, but this order
minimizes renumber churn since dehaze — the one that shifts `ToneCurve` — lands last).
