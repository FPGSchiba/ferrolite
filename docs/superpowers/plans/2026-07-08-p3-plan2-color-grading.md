# P3 Plan 2 — Color-Grading Wheels Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a global `ColorGrade` op — four hue/sat/lum wheels (Shadows/Midtones/Highlights/Global) plus Blending and Balance — applied as a per-pixel GPU node, with a new **Grade** tab driven by a reusable hue-sat color-wheel widget.

**Architecture:** `ColorGrade` is a new `ferrolite-pipeline` op inserted at `OpKind::ColorGrade` **after `Hsl`** (serde-safe tail renumber — `OpKind` is a never-serialized sort key). A pure `color_grade_px(rgb, &ColorGrade)` is the reusable transform (§2.5) and the GPU kernel's reference; the shader is a per-pixel `PointOpNode` (no halo) fed a uniform whose per-wheel tint vectors are **precomputed on the CPU** so the WGSL carries no magic constants and stays bit-for-bit aligned with the CPU fn. A new reusable `widgets/color_wheel.rs` (hue = angle, sat = radius, id-salted, plain egui `Mesh` drawing) is reused 4×; a new `GradeTab: PanelTab` hosts it with per-control reset on every wheel and slider.

**Tech Stack:** Rust, `ferrolite-pipeline` (op model + CPU grade math + WGSL per-pixel node), `ferrolite-app` (egui Grade tab + color-wheel widget), `wgpu`, `bytemuck`, `serde`/`serde_json`, `egui-phosphor` (icon).

## Global Constraints

Copied verbatim from the P3 design (`docs/superpowers/specs/2026-07-08-p3-tone-and-color-grading-design.md`) §2/§4 and the v2 architecture map §5; every task's requirements implicitly include these.

- **Global-only op.** No per-mask / `LocalAdjustments` work — deferred to the "P3-local" spec. `ColorGrade` is a global stack op.
- **Op-order + serde-safe renumber (§2.1, load-bearing).** Insert `OpKind::ColorGrade` **after `Hsl`, before `LocalAdjustments`**, and renumber the tail. `OpKind` is a **sort key that is never serialized** (guarded by `opkind_renumber_does_not_change_serde_output`). The renumber is mechanical and requires no sidecar migration. **Keep/extend that guard test.** Target order after this plan: `Exposure · WhiteBalance · Contrast · ToneCurve · Hsl · ColorGrade · LocalAdjustments · Sharpen · LensCorrection · Geometry` (Dehaze from Plan 3 is not in this branch).
- **Back-compat (contract 2).** `ColorGrade` is a new op variant; a sidecar written before it simply has no `ColorGrade` entry (`color_grade()` returns `None` → identity). New op struct fields default to identity. Catalog stays a pure cache.
- **Contract 4 (GPU executor is photo-agnostic).** `ferrolite-gpu`'s executor is NOT modified. `ColorGrade` is a `ferrolite-pipeline` **node** supplied via the existing generic `PointOpNode<U>` (per-pixel, no halo).
- **Build-once GPU (CLAUDE.md).** Build the pipeline/shader ONCE and reuse it; only the uploaded uniform changes per edit. Never rebuild per image/open/interaction. No UI-thread blocking. The color-wheel widget is plain egui vector drawing (an `egui::Mesh`), built inline each frame like every other egui widget (bounded, no GPU pipeline).
- **Reusable-math constraint (§2.5).** `pub fn color_grade_px(rgb: [f32; 3], cg: &ColorGrade) -> [f32; 3]` must be a pure, public fn in `ferrolite-pipeline`, independent of node/shader wiring, so the future per-mask path reuses it. The GPU kernel mirrors it. No transform logic may live only in a shader.
- **Per-control reset (CLAUDE.md, load-bearing).** Every new control resets to its default on its own: each wheel (reset → neutral, `sat = 0`), each Lum slider, Blending, Balance — reuse `widgets::draw_reset_arrow` + the `EguiSlider` reset column.
- **UI icons (CLAUDE.md, load-bearing).** The Grade feature's icon is a semantic alias added to `ferrolite-app/src/icons.rs` sourced from the Phosphor catalog. No raw glyphs, no hand-drawn `Painter` icons.
- **No new dependencies, no engine-tier edits, no copyleft.** Pure-Rust math in the photo tier (`ferrolite-pipeline` + `ferrolite-app`) only.
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean; 100-col; no `unwrap()` outside tests; immutable-by-default.
- **Identity elision:** a `ColorGrade` whose four wheels are all neutral (`sat == 0 && lum == 0`) is dropped from the stack (Blending/Balance are no-ops when nothing is tinted), mirroring every other `set_*` helper in `ops_edit`.

**Branch:** `feat/p3-color-grading` off `main` (create it before Task 1 if not already on it).

---

### Task 1: `ColorGrade` op model + serde-safe `OpKind` renumber

Add `GradeWheel` + `ColorGrade`, the `Op::ColorGrade` variant, and `OpKind::ColorGrade` inserted after `Hsl` (renumbering the tail). Add the accessor + `is_identity`. Keep the serde guard intact.

**Files:**
- Modify: `ferrolite-pipeline/src/op.rs` (structs, `Op`/`OpKind`/`Op::kind`/`OpStack::color_grade`, tests)
- Modify: `ferrolite-pipeline/src/lib.rs` (export `ColorGrade`, `GradeWheel`)
- Modify: `ferrolite-pipeline/src/serialize.rs` (round-trip test only)
- Test: `ferrolite-pipeline/src/op.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `pub struct GradeWheel { pub hue: f32, pub sat: f32, pub lum: f32 }` — derives `Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize` (`Default` = all 0 = neutral).
  - `pub struct ColorGrade { pub shadows: GradeWheel, pub midtones: GradeWheel, pub highlights: GradeWheel, pub global: GradeWheel, pub blending: f32, pub balance: f32 }` — derives `Clone, Copy, PartialEq, Debug, Serialize, Deserialize`; **manual** `Default` (`blending: 0.5`, `balance: 0.0`, wheels neutral).
  - `Op::ColorGrade(ColorGrade)`; `OpKind::ColorGrade = 5`; `OpStack::color_grade(&self) -> Option<ColorGrade>`.
  - `impl GradeWheel { pub fn is_neutral(&self) -> bool }`, `impl ColorGrade { pub fn is_identity(&self) -> bool }`.
- Consumes: nothing from later tasks.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `ferrolite-pipeline/src/op.rs`:

```rust
#[test]
fn grade_wheel_default_is_neutral() {
    let w = GradeWheel::default();
    assert_eq!((w.hue, w.sat, w.lum), (0.0, 0.0, 0.0));
    assert!(w.is_neutral());
}

#[test]
fn color_grade_default_is_identity_with_half_blending() {
    let cg = ColorGrade::default();
    assert_eq!(cg.blending, 0.5);
    assert_eq!(cg.balance, 0.0);
    assert!(cg.shadows.is_neutral() && cg.global.is_neutral());
    assert!(cg.is_identity(), "all-neutral wheels = identity regardless of blending/balance");
}

#[test]
fn color_grade_tinted_wheel_is_non_identity() {
    let cg = ColorGrade {
        shadows: GradeWheel { hue: 210.0, sat: 0.4, lum: 0.0 },
        ..Default::default()
    };
    assert!(!cg.is_identity());
    // A lum-only wheel is also non-identity.
    let cg2 = ColorGrade {
        highlights: GradeWheel { hue: 0.0, sat: 0.0, lum: 0.3 },
        ..Default::default()
    };
    assert!(!cg2.is_identity());
    // Blending/balance alone (no tint, no lum) stay identity.
    let cg3 = ColorGrade { blending: 0.9, balance: -0.5, ..Default::default() };
    assert!(cg3.is_identity());
}

#[test]
fn color_grade_sorts_between_hsl_and_local_adjustments() {
    let cg = Op::ColorGrade(ColorGrade {
        midtones: GradeWheel { hue: 120.0, sat: 0.2, lum: 0.0 },
        ..Default::default()
    });
    let s = OpStack::default()
        .set_op(Op::Sharpen(Sharpen { amount: 0.3, radius: 1 }))
        .set_op(cg.clone())
        .set_op(Op::Hsl(Hsl {
            bands: [HslBand { hue: 0.0, sat: 0.0, lum: 0.0 }; 8],
        }));
    let kinds: Vec<OpKind> = s.ops.iter().map(|o| o.kind()).collect();
    assert_eq!(kinds, vec![OpKind::Hsl, OpKind::ColorGrade, OpKind::Sharpen]);
    assert_eq!(s.color_grade().unwrap().midtones.hue, 120.0);
}

#[test]
fn opkind_discriminants_after_colorgrade_insert() {
    assert_eq!(OpKind::Hsl as u8, 4);
    assert_eq!(OpKind::ColorGrade as u8, 5);
    assert_eq!(OpKind::LocalAdjustments as u8, 6);
    assert_eq!(OpKind::Sharpen as u8, 7);
    assert_eq!(OpKind::LensCorrection as u8, 8);
    assert_eq!(OpKind::Geometry as u8, 9);
}

#[test]
fn color_grade_roundtrips() {
    let cg = ColorGrade {
        shadows: GradeWheel { hue: 210.0, sat: 0.5, lum: -0.2 },
        midtones: GradeWheel { hue: 90.0, sat: 0.1, lum: 0.0 },
        highlights: GradeWheel { hue: 40.0, sat: 0.3, lum: 0.15 },
        global: GradeWheel { hue: 0.0, sat: 0.0, lum: 0.05 },
        blending: 0.7,
        balance: -0.3,
    };
    let s = serde_json::to_string(&cg).unwrap();
    assert_eq!(serde_json::from_str::<ColorGrade>(&s).unwrap(), cg);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline --lib op::tests`
Expected: FAIL to compile (`GradeWheel`/`ColorGrade` unknown, no `Op::ColorGrade`/`OpKind::ColorGrade`/`color_grade`).

- [ ] **Step 3: Add the structs**

In `ferrolite-pipeline/src/op.rs`, add just above the `Op` enum:

```rust
/// One color-grading wheel: a hue-sat tint direction plus a luminance offset.
/// `hue` in [0,360) degrees (wheel angle), `sat` in [0,1] (distance from center,
/// 0 = no tint), `lum` in [-1,1] (region luminance offset). Default = neutral.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct GradeWheel {
    pub hue: f32,
    pub sat: f32,
    pub lum: f32,
}

impl GradeWheel {
    /// True when this wheel applies no tint and no luminance shift.
    pub fn is_neutral(&self) -> bool {
        self.sat == 0.0 && self.lum == 0.0
    }
}

/// Lightroom-style color grading: four wheels (Shadows/Midtones/Highlights/
/// Global) plus region `blending` (overlap width, [0,1]) and `balance` (shifts
/// the shadow↔highlight midpoint, [-1,1]). Default = identity.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct ColorGrade {
    pub shadows: GradeWheel,
    pub midtones: GradeWheel,
    pub highlights: GradeWheel,
    pub global: GradeWheel,
    pub blending: f32,
    pub balance: f32,
}

impl Default for ColorGrade {
    fn default() -> Self {
        // Neutral wheels, mid blending, centered balance → identity.
        Self {
            shadows: GradeWheel::default(),
            midtones: GradeWheel::default(),
            highlights: GradeWheel::default(),
            global: GradeWheel::default(),
            blending: 0.5,
            balance: 0.0,
        }
    }
}

impl ColorGrade {
    /// True when every wheel is neutral (no tint, no lum). Blending/balance are
    /// no-ops when nothing is tinted, so they do not affect identity.
    pub fn is_identity(&self) -> bool {
        self.shadows.is_neutral()
            && self.midtones.is_neutral()
            && self.highlights.is_neutral()
            && self.global.is_neutral()
    }
}
```

- [ ] **Step 4: Add the `Op` variant, renumber `OpKind`, add `kind()` arm + accessor**

In `ferrolite-pipeline/src/op.rs`:

- Add `ColorGrade(ColorGrade),` to the `Op` enum between `Hsl(Hsl),` and `LocalAdjustments(LocalAdjustments),`.
- In the `OpKind` enum, insert `ColorGrade = 5,` after `Hsl = 4,` and renumber the tail:
```rust
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Exposure = 0,
    WhiteBalance = 1,
    Contrast = 2,
    ToneCurve = 3,
    Hsl = 4,
    ColorGrade = 5,
    LocalAdjustments = 6,
    Sharpen = 7,
    LensCorrection = 8,
    Geometry = 9,
}
```
- Add the match arm to `Op::kind()`: `Op::ColorGrade(_) => OpKind::ColorGrade,` (between the `Hsl` and `LocalAdjustments` arms).
- Add the accessor to `impl OpStack` (next to `hsl`):
```rust
pub fn color_grade(&self) -> Option<ColorGrade> {
    self.ops.iter().find_map(|o| match o {
        Op::ColorGrade(c) => Some(*c),
        _ => None,
    })
}
```

- [ ] **Step 5: Update any existing OpKind-discriminant assertion**

The existing test `opkind_discriminants_place_local_adjustments_after_hsl` asserts the OLD numbers (`LocalAdjustments = 5`, etc.). Replace its body with the new numbering (or delete it in favor of the new `opkind_discriminants_after_colorgrade_insert` added in Step 1 — pick one; do not leave a test asserting the pre-renumber numbers). Leave `opkind_renumber_does_not_change_serde_output` **unchanged** (it uses Exposure+Sharpen JSON; `Sharpen` serializes by variant name regardless of discriminant, so its expected JSON is still correct).

- [ ] **Step 6: Export the types**

In `ferrolite-pipeline/src/lib.rs`, add `ColorGrade` and `GradeWheel` to the `pub use op::{ .. }` list (alphabetical-ish):
```rust
pub use op::{
    Aspect, ColorGrade, Contrast, Correction, CropRect, CurveMode, Exposure, Geometry, GradeWheel,
    Hsl, HslBand, LensCorrection, Op, OpKind, OpStack, ParametricCurve, PointCurve, Sharpen,
    ToneCurve, WhiteBalance, STACK_VERSION,
};
```

- [ ] **Step 7: Add a serialize round-trip test**

In `ferrolite-pipeline/src/serialize.rs` tests, add (import `ColorGrade`, `GradeWheel` in the test `use`):
```rust
#[test]
fn round_trips_color_grade() {
    let s = OpStack::default().set_op(Op::ColorGrade(ColorGrade {
        shadows: GradeWheel { hue: 210.0, sat: 0.4, lum: -0.1 },
        ..Default::default()
    }));
    assert_eq!(deserialize(&serialize(&s)), Some(s));
}
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --lib` and `cargo test -p ferrolite-pipeline --test '*'` (serialize/golden compile)
Expected: PASS. Then `cargo build --workspace` — the new `OpKind` variant makes `Op::kind()` matches exhaustive; if any `match op.kind()` elsewhere in the workspace became non-exhaustive it will fail to compile — fix by adding a `ColorGrade` arm mirroring the adjacent `Hsl`/`LocalAdjustments` handling (report any such site in your report).

- [ ] **Step 9: Commit**

```bash
git add ferrolite-pipeline/src/op.rs ferrolite-pipeline/src/lib.rs ferrolite-pipeline/src/serialize.rs
git commit -m "feat(pipeline): add ColorGrade op (OpKind after Hsl, serde-safe renumber)"
```

---

### Task 2: `color_grade_px` pure transform

The reusable per-pixel grade math (§2.5) and the GPU kernel's reference: luminance-based region weights (shaped by blending + balance), per-wheel hue-sat tint vectors, weighted add + luminance offset.

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs` (constants, `hsv_to_rgb`, `grade_tint`, `grade_region_weights`, `color_grade_px`; tests)
- Modify: `ferrolite-pipeline/src/lib.rs` (export `color_grade_px`)
- Test: `ferrolite-pipeline/src/uniforms.rs`

**Interfaces:**
- Consumes: `crate::op::{ColorGrade, GradeWheel}` (Task 1); the existing private `luma709` and `smoothstep` in `uniforms.rs`.
- Produces:
  - `pub const GRADE_TINT_STRENGTH: f32 = 0.5;` `pub const GRADE_LUM_STRENGTH: f32 = 0.5;`
  - `pub fn color_grade_px(rgb: [f32; 3], cg: &ColorGrade) -> [f32; 3]` — identity when all wheels neutral; adds tint + lum weighted by region.
  - crate-internal `fn hsv_to_rgb(h_deg: f32, s: f32, v: f32) -> [f32; 3]`, `fn grade_tint(hue: f32, sat: f32) -> [f32; 3]` (zero-luma chroma vector), `fn grade_region_weights(y: f32, blending: f32, balance: f32) -> (f32, f32, f32)` (shadow, midtone, highlight).

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `ferrolite-pipeline/src/uniforms.rs`:

```rust
#[test]
fn color_grade_identity_when_neutral() {
    use crate::op::ColorGrade;
    let c = color_grade_px([0.3, 0.5, 0.7], &ColorGrade::default());
    assert!((c[0] - 0.3).abs() < 1e-6 && (c[1] - 0.5).abs() < 1e-6 && (c[2] - 0.7).abs() < 1e-6);
}

#[test]
fn shadows_tint_colors_darks_not_highlights() {
    use crate::op::{ColorGrade, GradeWheel};
    // A blue (hue 240) shadow tint.
    let cg = ColorGrade {
        shadows: GradeWheel { hue: 240.0, sat: 1.0, lum: 0.0 },
        ..Default::default()
    };
    let dark = color_grade_px([0.1, 0.1, 0.1], &cg);
    let light = color_grade_px([0.9, 0.9, 0.9], &cg);
    // Darks gain blue (B rises above R). Highlights are ~unchanged.
    assert!(dark[2] > dark[0] + 0.02, "shadow tint bluened the darks: {dark:?}");
    assert!(
        (light[0] - 0.9).abs() < 0.03 && (light[2] - 0.9).abs() < 0.03,
        "highlights ~unchanged by a shadows-only tint: {light:?}"
    );
}

#[test]
fn global_tint_affects_all_luminances() {
    use crate::op::{ColorGrade, GradeWheel};
    let cg = ColorGrade {
        global: GradeWheel { hue: 120.0, sat: 1.0, lum: 0.0 }, // green
        ..Default::default()
    };
    let dark = color_grade_px([0.1, 0.1, 0.1], &cg);
    let light = color_grade_px([0.8, 0.8, 0.8], &cg);
    assert!(dark[1] > dark[0] + 0.02, "global greened the darks");
    assert!(light[1] > light[0] + 0.02, "global greened the highlights too");
}

#[test]
fn balance_shifts_the_region_split() {
    // With balance negative, the shadow region shrinks (pivot moves down), so a
    // mid-dark pixel leans more highlight; with balance positive it leans shadow.
    let (sh_lo, _, _) = grade_region_weights(0.4, 0.5, -0.6);
    let (sh_hi, _, _) = grade_region_weights(0.4, 0.5, 0.6);
    assert!(sh_hi > sh_lo, "positive balance raises the shadow weight at a fixed Y");
}

#[test]
fn blending_widens_region_overlap() {
    // At the extremes, wider blending pulls the shadow/highlight weights toward
    // 0.5 (more overlap); narrow blending pushes them apart.
    let (sh_wide, _, _) = grade_region_weights(0.25, 1.0, 0.0);
    let (sh_narrow, _, _) = grade_region_weights(0.25, 0.0, 0.0);
    assert!(sh_narrow > sh_wide, "narrow blending keeps low-Y firmly in shadows");
}

#[test]
fn grade_tint_is_zero_at_zero_sat() {
    assert_eq!(grade_tint(123.0, 0.0), [0.0, 0.0, 0.0]);
}

#[test]
fn lum_only_wheel_shifts_brightness_without_tint() {
    use crate::op::{ColorGrade, GradeWheel};
    let cg = ColorGrade {
        global: GradeWheel { hue: 0.0, sat: 0.0, lum: 0.5 },
        ..Default::default()
    };
    let c = color_grade_px([0.4, 0.4, 0.4], &cg);
    assert!(c[0] > 0.4 && (c[0] - c[1]).abs() < 1e-6 && (c[1] - c[2]).abs() < 1e-6,
        "uniform brighten, no tint: {c:?}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests`
Expected: FAIL to compile (`color_grade_px`, `grade_region_weights`, `grade_tint` undefined).

- [ ] **Step 3: Implement the math**

Add to `ferrolite-pipeline/src/uniforms.rs` (after `light_color_apply`, reusing the existing private `luma709` and `smoothstep`):

```rust
/// Chroma strength added per unit (sat × weight). Pragmatic constant (image
/// science secondary, like `wb_multipliers`); sat 1 in a region adds ~0.5 chroma.
pub const GRADE_TINT_STRENGTH: f32 = 0.5;
/// Luminance offset strength added per unit (lum × weight).
pub const GRADE_LUM_STRENGTH: f32 = 0.5;

/// HSV → linear RGB (h in degrees, s/v in [0,1]). Standard sextant conversion.
fn hsv_to_rgb(h_deg: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h_deg.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h.floor() as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// The zero-luminance chroma vector for a wheel (hue, sat): the wheel color's
/// hue-sat direction with its luminance removed, so adding it tints without a
/// net brightness shift. Zero at `sat == 0` (identity).
fn grade_tint(hue: f32, sat: f32) -> [f32; 3] {
    let s = sat.clamp(0.0, 1.0);
    if s <= 0.0 {
        return [0.0, 0.0, 0.0];
    }
    let c = hsv_to_rgb(hue, s, 1.0);
    let y = luma709(c);
    [c[0] - y, c[1] - y, c[2] - y]
}

/// Region weights (shadow, midtone, highlight) for pixel luminance `y`, shaped by
/// `blending` (overlap width, [0,1]) and `balance` (shifts the shadow↔highlight
/// midpoint, [-1,1]). Highlight rises with `y`; shadow is its complement; midtone
/// is a bump peaking at the pivot. Not a strict partition (regions overlap, as in
/// LR grading); the WGSL kernel mirrors this exactly.
fn grade_region_weights(y: f32, blending: f32, balance: f32) -> (f32, f32, f32) {
    let pivot = 0.5 + 0.5 * balance.clamp(-1.0, 1.0);
    let width = 0.15 + 0.35 * blending.clamp(0.0, 1.0);
    let w_hi = smoothstep(pivot - width, pivot + width, y);
    let w_sh = 1.0 - w_hi;
    let w_mid = 4.0 * w_sh * w_hi;
    (w_sh, w_mid, w_hi)
}

/// Pure per-pixel color grade — the reusable transform (design §2.5) and the
/// `color_grade.wgsl` kernel's reference. Adds each region's tint (weighted by
/// its luminance region) plus the region's luminance offset; the Global wheel
/// applies uniformly. Identity when all wheels are neutral. Not clamped (out-of-
/// range values pass through, honoring P2 §5.3; display clamps later).
pub fn color_grade_px(rgb: [f32; 3], cg: &crate::op::ColorGrade) -> [f32; 3] {
    let y = luma709(rgb);
    let (w_sh, w_mid, w_hi) = grade_region_weights(y, cg.blending, cg.balance);
    let t_sh = grade_tint(cg.shadows.hue, cg.shadows.sat);
    let t_mid = grade_tint(cg.midtones.hue, cg.midtones.sat);
    let t_hi = grade_tint(cg.highlights.hue, cg.highlights.sat);
    let t_gl = grade_tint(cg.global.hue, cg.global.sat);
    let lum = GRADE_LUM_STRENGTH
        * (w_sh * cg.shadows.lum + w_mid * cg.midtones.lum + w_hi * cg.highlights.lum + cg.global.lum);
    let mut out = [0.0f32; 3];
    for (c, slot) in out.iter_mut().enumerate() {
        let tint = w_sh * t_sh[c] + w_mid * t_mid[c] + w_hi * t_hi[c] + t_gl[c];
        *slot = rgb[c] + GRADE_TINT_STRENGTH * tint + lum;
    }
    out
}
```

- [ ] **Step 4: Export `color_grade_px`**

In `ferrolite-pipeline/src/lib.rs`, add `color_grade_px` to the `pub use uniforms::{ .. }` block (next to the other pure fns `curve_lut`/`parametric_curve_lut`/`tone_curve_luts`). Update the preceding comment to name it as another pure §2.5 reusable.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): add pure color_grade_px transform"
```

---

### Task 3: `ColorGradeUniform` + `color_grade_uniform` builder

The GPU uniform mirroring the shader's `struct P`. Per-wheel tint vectors are **precomputed** (via `grade_tint`, pre-scaled by `GRADE_TINT_STRENGTH`) and lum pre-scaled (by `GRADE_LUM_STRENGTH`), so the shader adds them directly and carries no magic constants.

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs` (`ColorGradeUniform`, `color_grade_uniform`; tests)
- Modify: `ferrolite-pipeline/src/lib.rs` (export `ColorGradeUniform`)
- Test: `ferrolite-pipeline/src/uniforms.rs`

**Interfaces:**
- Consumes: `ColorGrade` (Task 1); `grade_tint`, `GRADE_TINT_STRENGTH`, `GRADE_LUM_STRENGTH` (Task 2).
- Produces:
  - `#[repr(C)] pub struct ColorGradeUniform { pub shadows: [f32; 4], pub midtones: [f32; 4], pub highlights: [f32; 4], pub global: [f32; 4], pub params: [f32; 4] }` — each wheel `[tint_r, tint_g, tint_b, lum]` pre-scaled; `params = [blending, balance, 0, 0]`. Derives `Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable`.
  - `pub fn color_grade_uniform(op: Option<ColorGrade>) -> ColorGradeUniform`.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `ferrolite-pipeline/src/uniforms.rs`:

```rust
#[test]
fn color_grade_uniform_identity_is_all_zero_tints() {
    let u = color_grade_uniform(None);
    assert_eq!(u.shadows, [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(u.midtones, [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(u.highlights, [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(u.global, [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(u.params, [0.5, 0.0, 0.0, 0.0]); // default blending/balance
    assert_eq!(std::mem::size_of::<ColorGradeUniform>() % 16, 0);
}

#[test]
fn color_grade_uniform_prescales_tint_and_lum() {
    use crate::op::{ColorGrade, GradeWheel};
    let cg = ColorGrade {
        shadows: GradeWheel { hue: 240.0, sat: 1.0, lum: 0.4 },
        blending: 0.7,
        balance: -0.2,
        ..Default::default()
    };
    let u = color_grade_uniform(Some(cg));
    // Tint row = grade_tint(...) * GRADE_TINT_STRENGTH; lum = 0.4 * GRADE_LUM_STRENGTH.
    let t = grade_tint(240.0, 1.0);
    assert!((u.shadows[0] - t[0] * GRADE_TINT_STRENGTH).abs() < 1e-6);
    assert!((u.shadows[1] - t[1] * GRADE_TINT_STRENGTH).abs() < 1e-6);
    assert!((u.shadows[2] - t[2] * GRADE_TINT_STRENGTH).abs() < 1e-6);
    assert!((u.shadows[3] - 0.4 * GRADE_LUM_STRENGTH).abs() < 1e-6);
    assert_eq!(u.params, [0.7, -0.2, 0.0, 0.0]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::color_grade_uniform`
Expected: FAIL to compile (`ColorGradeUniform`, `color_grade_uniform` undefined).

- [ ] **Step 3: Implement the uniform + builder**

Add to `ferrolite-pipeline/src/uniforms.rs` (near the other `*Uniform` structs, after `color_grade_px`):

```rust
/// GPU uniform for `color_grade.wgsl`. `#[repr(C)]`, 16-byte aligned; field order
/// MIRRORS the WGSL `struct P`. Each wheel row is `[tint_r, tint_g, tint_b, lum]`
/// with tint pre-scaled by `GRADE_TINT_STRENGTH` and lum by `GRADE_LUM_STRENGTH`,
/// so the shader adds them directly (no magic constants in WGSL). `params` =
/// `[blending, balance, 0, 0]`.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ColorGradeUniform {
    pub shadows: [f32; 4],
    pub midtones: [f32; 4],
    pub highlights: [f32; 4],
    pub global: [f32; 4],
    pub params: [f32; 4],
}

pub fn color_grade_uniform(op: Option<crate::op::ColorGrade>) -> ColorGradeUniform {
    let cg = op.unwrap_or_default();
    let pack = |w: &crate::op::GradeWheel| {
        let t = grade_tint(w.hue, w.sat);
        [
            t[0] * GRADE_TINT_STRENGTH,
            t[1] * GRADE_TINT_STRENGTH,
            t[2] * GRADE_TINT_STRENGTH,
            w.lum * GRADE_LUM_STRENGTH,
        ]
    };
    ColorGradeUniform {
        shadows: pack(&cg.shadows),
        midtones: pack(&cg.midtones),
        highlights: pack(&cg.highlights),
        global: pack(&cg.global),
        params: [cg.blending, cg.balance, 0.0, 0.0],
    }
}
```

- [ ] **Step 4: Export it**

In `ferrolite-pipeline/src/lib.rs`, add `ColorGradeUniform` to the `pub use uniforms::{ .. }` block next to the other `*Uniform` structs.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): add ColorGradeUniform + precomputed-tint builder"
```

---

### Task 4: GPU shader + wire `ColorGrade` into both pipelines

Add `color_grade.wgsl` (per-pixel, mirrors `color_grade_px`), wire a `PointOpNode<ColorGradeUniform>` into `EditPipeline` and `TileEditPipeline` **after the HSL node, before local adjustments**, prewarm the shader, and add a golden.

**Files:**
- Create: `ferrolite-pipeline/src/shaders/color_grade.wgsl`
- Modify: `ferrolite-pipeline/src/pipeline.rs` (node + wiring + `set_stack` + `node_count`)
- Modify: `ferrolite-pipeline/src/tile_edit.rs` (same wiring for the tiled path)
- Modify: `ferrolite-pipeline/src/lib.rs` (`prewarm_shaders`)
- Test: `ferrolite-pipeline/tests/golden.rs`
- New golden fixture: `ferrolite-pipeline/tests/fixtures/color_grade.png`

**Interfaces:**
- Consumes: `color_grade_uniform`, `ColorGradeUniform` (Task 3); the existing `PointOpNode<U>` (in `nodes.rs`); `stack.color_grade()` (Task 1).
- Produces: a new graph node between `hsl_id` and `local_adjust_id` in both pipelines; a shared `Rc<Cell<ColorGradeUniform>>` updated in `set_stack`.

- [ ] **Step 1: Write the shader**

Create `ferrolite-pipeline/src/shaders/color_grade.wgsl` (mirrors `color_grade_px`; per-pixel; reuses the point-op bind layout 0=src, 1=dst, 2=uniform):

```wgsl
// Color grading: per-pixel hue-sat tint + luminance offset per tonal region.
// Point op (point-op bind layout). Tints/lum are PRE-SCALED on the CPU
// (color_grade_uniform), so this shader adds them directly — the per-pixel math
// mirrors uniforms.rs `color_grade_px` exactly. Not clamped: out-of-[0,1] values
// pass through (identity grade ⇒ exact pass-through), P2 §5.3.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P {
    shadows: vec4<f32>,    // xyz = tint (pre-scaled), w = lum (pre-scaled)
    midtones: vec4<f32>,
    highlights: vec4<f32>,
    global: vec4<f32>,
    params: vec4<f32>,     // x = blending, y = balance
};
@group(0) @binding(2) var<uniform> p: P;

fn smoothstep_f(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);

    let y = dot(c.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let pivot = 0.5 + 0.5 * p.params.y;   // balance
    let width = 0.15 + 0.35 * p.params.x; // blending
    let w_hi = smoothstep_f(pivot - width, pivot + width, y);
    let w_sh = 1.0 - w_hi;
    let w_mid = 4.0 * w_sh * w_hi;

    let tint = w_sh * p.shadows.xyz + w_mid * p.midtones.xyz
             + w_hi * p.highlights.xyz + p.global.xyz;
    let lum = w_sh * p.shadows.w + w_mid * p.midtones.w
            + w_hi * p.highlights.w + p.global.w;

    let out_rgb = c.rgb + tint + vec3<f32>(lum);
    textureStore(dst, xy, vec4<f32>(out_rgb, c.a));
}
```

> Parity note: `color_grade_px` uses the shared `luma709` (0.2126/0.7152/0.0722) and `smoothstep` — the WGSL `dot(...)` weights and `smoothstep_f` must match those exactly. The golden test is the cross-check.

- [ ] **Step 2: Wire into `pipeline.rs`**

In `ferrolite-pipeline/src/pipeline.rs` (locate by content — line numbers shifted after Plan 1):
- Import: add `color_grade_uniform, ColorGradeUniform` to the `use crate::uniforms::{ .. }` list.
- Add a struct field beside `hsl`/`hsl_id`:
```rust
    color_grade_id: NodeId,
    color_grade: Rc<Cell<ColorGradeUniform>>,
```
- In `new`, after the `hsl_id` node is added and before `local_layers`/`local_adjust_id`, insert:
```rust
        let color_grade = Rc::new(Cell::new(color_grade_uniform(stack.color_grade())));
        let color_grade_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/color_grade.wgsl"),
            "color-grade",
            color_grade.clone(),
        );
        let color_grade_id = graph.add_node(Box::new(color_grade_node), vec![hsl_id]);
```
- Change the local-adjust node's dependency from `vec![hsl_id]` to `vec![color_grade_id]`.
- Add `color_grade_id,` and `color_grade,` to the `Self { .. }` initializer.
- Bump `node_count: 11` to `node_count: 12`.
- In `set_stack`, after the HSL dirty-check block, add:
```rust
        let cg = color_grade_uniform(stack.color_grade());
        if cg != self.color_grade.get() {
            self.color_grade.set(cg);
            self.graph.mark_dirty(self.color_grade_id);
        }
```

- [ ] **Step 3: Wire into `tile_edit.rs`**

Apply the equivalent wiring in `ferrolite-pipeline/src/tile_edit.rs`: import `color_grade_uniform`/`ColorGradeUniform`; add the `color_grade`/`color_grade_id` fields; construct the node depending on the tiled path's HSL node id and re-point the downstream (local-adjust) node's dependency at it; add both to the struct initializer; and update the `set_stack` path to recompute + set the uniform (matching how that file already updates the HSL uniform). Follow the file's existing HSL-node pattern verbatim.

- [ ] **Step 4: Prewarm the shader**

In `ferrolite-pipeline/src/lib.rs` `prewarm_shaders`, add `("color-grade", include_str!("shaders/color_grade.wgsl")),` to the array, and update the count in the doc comment (e.g. "Ten passes" → "Eleven passes").

- [ ] **Step 5: Verify build + existing goldens unaffected**

Run: `cargo build -p ferrolite-pipeline`
Then: `cargo test -p ferrolite-pipeline --test golden`
Expected on a GPU dev box: ALL existing goldens still pass. An identity `ColorGrade` node adds `c.rgb + 0 + 0` (exact pass-through, including out-of-range values — no clamp), so `tone_curve*.png`, `hsl.png`, `full_seven_op_stack.png`, `identity_stack_matches_source_render`, etc. must be byte-identical within tolerance. Headless CI skips (no adapter).

> If any existing golden drifts, STOP: the inserted node is not neutral at identity. Do NOT re-author existing goldens — debug the shader (most likely an unintended clamp or a wrong default uniform). The `color_grade_uniform_identity_is_all_zero_tints` unit test is the oracle.

- [ ] **Step 6: Write the failing golden test**

Add to `ferrolite-pipeline/tests/golden.rs` (add `ColorGrade`, `GradeWheel` to the top `use ferrolite_pipeline::{ .. }`):

```rust
#[test]
fn color_grade_three_way_plus_global_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::ColorGrade(ColorGrade {
        shadows: GradeWheel { hue: 220.0, sat: 0.6, lum: -0.1 },   // cool shadows
        midtones: GradeWheel { hue: 120.0, sat: 0.2, lum: 0.0 },   // slight green mids
        highlights: GradeWheel { hue: 40.0, sat: 0.5, lum: 0.1 },  // warm highlights
        global: GradeWheel { hue: 300.0, sat: 0.15, lum: 0.0 },    // faint magenta cast
        blending: 0.6,
        balance: -0.1,
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "color_grade.png");
}
```

- [ ] **Step 7: Author + confirm the golden**

Run: `cargo test -p ferrolite-pipeline --test golden color_grade_three_way_plus_global_matches_golden`
Expected: first run authors `tests/fixtures/color_grade.png` (prints `wrote golden ...`) and passes; run it a **second** time — passes against the committed fixture. Eyeball the PNG once: cool/dark end, warm bright end, faint magenta overall.

- [ ] **Step 8: Commit**

```bash
git add ferrolite-pipeline/src/shaders/color_grade.wgsl ferrolite-pipeline/src/pipeline.rs ferrolite-pipeline/src/tile_edit.rs ferrolite-pipeline/src/lib.rs ferrolite-pipeline/tests/golden.rs ferrolite-pipeline/tests/fixtures/color_grade.png
git commit -m "feat(pipeline): per-pixel ColorGrade node after Hsl (both pipelines) + golden"
```

---

### Task 5: `ops_edit::set_color_grade` with identity elision

The app-side edit helper that maps a `ColorGrade` onto a new immutable `OpStack`, dropping the op when all wheels are neutral.

**Files:**
- Modify: `ferrolite-app/src/develop/ops_edit.rs` (add `set_color_grade`; import `ColorGrade`; tests)
- Test: `ferrolite-app/src/develop/ops_edit.rs`

**Interfaces:**
- Consumes: `ferrolite_pipeline::{ColorGrade, GradeWheel, Op, OpStack, OpKind}` and `ColorGrade::is_identity` (Task 1).
- Produces: `pub fn set_color_grade(s: &OpStack, cg: ColorGrade) -> OpStack`.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `ferrolite-app/src/develop/ops_edit.rs`:

```rust
#[test]
fn set_color_grade_identity_removes_the_op() {
    use ferrolite_pipeline::ColorGrade;
    let s = set_color_grade(&OpStack::default(), ColorGrade::default());
    assert!(s.color_grade().is_none(), "neutral grade = no op");
    assert!(s.is_identity());
}

#[test]
fn set_color_grade_tinted_wheel_sets_the_op() {
    use ferrolite_pipeline::{ColorGrade, GradeWheel};
    let cg = ColorGrade {
        highlights: GradeWheel { hue: 40.0, sat: 0.3, lum: 0.0 },
        ..Default::default()
    };
    let s = set_color_grade(&OpStack::default(), cg);
    assert_eq!(s.color_grade(), Some(cg));
}

#[test]
fn set_color_grade_lum_only_is_kept() {
    use ferrolite_pipeline::{ColorGrade, GradeWheel};
    let cg = ColorGrade {
        global: GradeWheel { hue: 0.0, sat: 0.0, lum: 0.25 },
        ..Default::default()
    };
    let s = set_color_grade(&OpStack::default(), cg);
    assert!(s.color_grade().is_some(), "a lum-only grade is not identity");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-app ops_edit::tests::set_color_grade`
Expected: FAIL to compile (`set_color_grade` undefined).

- [ ] **Step 3: Implement `set_color_grade`**

In `ferrolite-app/src/develop/ops_edit.rs`, add `ColorGrade` to the top-of-file `use ferrolite_pipeline::{ .. }`, then add (next to `set_tone_curve`):

```rust
/// Set the color grade, or REMOVE the op entirely when every wheel is neutral
/// (no tint, no lum) — so `is_identity()`/`has_edits` stay correct, mirroring
/// every other `set_*` helper here.
pub fn set_color_grade(s: &OpStack, cg: ColorGrade) -> OpStack {
    if cg.is_identity() {
        s.reset(ferrolite_pipeline::OpKind::ColorGrade)
    } else {
        s.set_op(Op::ColorGrade(cg))
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-app ops_edit::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/develop/ops_edit.rs
git commit -m "feat(app): add set_color_grade edit helper with neutral-grade elision"
```

---

### Task 6: Reusable `widgets/color_wheel.rs`

A reusable hue-sat disc: hue = angle, sat = radius, drawn as a plain egui `Mesh` (grey center → full-sat rim), with a draggable thumb, its own per-control reset (→ neutral `sat = 0`), and id-salting so four instances coexist. Pure polar math is unit-tested; rendering/interaction is visual.

**Files:**
- Create: `ferrolite-app/src/widgets/color_wheel.rs`
- Modify: `ferrolite-app/src/widgets/mod.rs` (register `pub mod color_wheel;` + re-export)
- Test: `ferrolite-app/src/widgets/color_wheel.rs`

**Interfaces:**
- Consumes: `crate::theme`, `crate::widgets::draw_reset_arrow`, egui.
- Produces:
  - `pub struct WheelEdit { pub hue: f32, pub sat: f32, pub commit: bool }`
  - `pub fn color_wheel(ui: &mut egui::Ui, id_source: impl std::hash::Hash, hue: f32, sat: f32) -> Option<WheelEdit>` — returns `Some` on drag/click/reset; `commit` true on release/click/reset, false mid-drag.
  - pure helpers `fn wheel_pos(center: egui::Pos2, radius: f32, hue: f32, sat: f32) -> egui::Pos2` and `fn wheel_from_pos(center: egui::Pos2, radius: f32, p: egui::Pos2) -> (f32, f32)`.

- [ ] **Step 1: Write the failing tests**

Create `ferrolite-app/src/widgets/color_wheel.rs` starting with the tests + signatures (implement in Step 3). Add this `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn center_maps_to_zero_sat() {
        let c = pos2(50.0, 50.0);
        let (_hue, sat) = wheel_from_pos(c, 40.0, c);
        assert!(sat.abs() < 1e-6, "pointer at center = sat 0");
    }

    #[test]
    fn edge_maps_to_full_sat() {
        let c = pos2(50.0, 50.0);
        let edge = pos2(50.0 + 40.0, 50.0); // one radius to the right
        let (_hue, sat) = wheel_from_pos(c, 40.0, edge);
        assert!((sat - 1.0).abs() < 1e-6, "pointer at the rim = sat 1");
    }

    #[test]
    fn beyond_edge_clamps_sat_to_one() {
        let c = pos2(50.0, 50.0);
        let far = pos2(50.0 + 80.0, 50.0); // two radii out
        let (_h, sat) = wheel_from_pos(c, 40.0, far);
        assert!((sat - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pos_and_from_pos_roundtrip() {
        let c = pos2(50.0, 50.0);
        let r = 40.0;
        for &(hue, sat) in &[(0.0, 0.5), (90.0, 1.0), (210.0, 0.3), (330.0, 0.8)] {
            let p = wheel_pos(c, r, hue, sat);
            let (h2, s2) = wheel_from_pos(c, r, p);
            assert!((s2 - sat).abs() < 1e-4, "sat roundtrip {sat} -> {s2}");
            let dh = ((h2 - hue + 180.0).rem_euclid(360.0)) - 180.0;
            assert!(dh.abs() < 1e-3, "hue roundtrip {hue} -> {h2}");
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-app color_wheel::tests`
Expected: FAIL to compile (`wheel_pos`/`wheel_from_pos` undefined / module not registered).

- [ ] **Step 3: Implement the widget**

Write the module body (above the tests) in `ferrolite-app/src/widgets/color_wheel.rs`:

```rust
//! Reusable hue-sat color wheel for color grading. Hue = angle (0° at +x,
//! increasing counter-clockwise on screen), sat = radius (0 at center = neutral,
//! 1 at the rim). Drawn as a plain egui `Mesh` (grey center → full-sat rim), with
//! a draggable thumb and its own per-control reset (→ neutral sat 0). All memory
//! is salted with `id_source` so multiple wheels coexist (design §4.3).

use crate::theme;
use crate::widgets::draw_reset_arrow;
use egui::{pos2, vec2, Color32, Mesh, Pos2, Sense, Shape, Stroke};

const RADIUS: f32 = 46.0;
const SEGMENTS: usize = 48;
const RESET_R: f32 = 4.5;

/// A change emitted by `color_wheel`. `commit` false = live drag preview.
pub struct WheelEdit {
    pub hue: f32,
    pub sat: f32,
    pub commit: bool,
}

/// Screen position of the thumb for a given (hue, sat). Screen y is down, so the
/// angle's sine is negated to make hue increase counter-clockwise visually.
fn wheel_pos(center: Pos2, radius: f32, hue: f32, sat: f32) -> Pos2 {
    let a = hue.to_radians();
    center + radius * sat.clamp(0.0, 1.0) * vec2(a.cos(), -a.sin())
}

/// (hue, sat) for a pointer position: sat = distance/radius clamped [0,1], hue =
/// screen angle (y-down inverted) in [0,360).
fn wheel_from_pos(center: Pos2, radius: f32, p: Pos2) -> (f32, f32) {
    let d = p - center;
    let sat = (d.length() / radius).clamp(0.0, 1.0);
    let mut hue = (-d.y).atan2(d.x).to_degrees();
    if hue < 0.0 {
        hue += 360.0;
    }
    (hue, sat)
}

/// HSV → egui Color32 (h in degrees, s/v in [0,1]) for the disc mesh (UI only).
fn hsv_color(h_deg: f32, s: f32, v: f32) -> Color32 {
    let h = h_deg.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h.floor() as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color32::from_rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

pub fn color_wheel(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    hue: f32,
    sat: f32,
) -> Option<WheelEdit> {
    let size = RADIUS * 2.0 + 4.0;
    // Extra height below the disc for the reset affordance.
    let (rect, resp) =
        ui.allocate_exact_size(vec2(size, size + 18.0), Sense::click_and_drag());
    let base_id = ui.id().with(id_source);
    let center = pos2(rect.center().x, rect.top() + RADIUS + 2.0);

    // Hue-sat disc as a triangle fan: grey center + full-sat rim.
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, Color32::from_gray(0x80));
    for i in 0..=SEGMENTS {
        let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let deg = a.to_degrees();
        mesh.colored_vertex(
            center + RADIUS * vec2(a.cos(), -a.sin()),
            hsv_color(deg, 1.0, 1.0),
        );
    }
    for i in 1..=SEGMENTS as u32 {
        mesh.add_triangle(0, i, i + 1);
    }
    let painter = ui.painter();
    painter.add(Shape::mesh(mesh));
    painter.circle_stroke(center, RADIUS, Stroke::new(1.0, theme::BORDER_STRONG));

    // Thumb at the current (hue, sat).
    let thumb = wheel_pos(center, RADIUS, hue, sat);
    painter.circle(thumb, 5.0, Color32::WHITE, Stroke::new(1.5, Color32::BLACK));

    // Interaction: drag/click sets hue+sat; release commits.
    let mut result: Option<WheelEdit> = None;
    if let Some(p) = resp.interact_pointer_pos() {
        if resp.dragged() {
            let (h, s) = wheel_from_pos(center, RADIUS, p);
            result = Some(WheelEdit { hue: h, sat: s, commit: false });
        } else if resp.clicked() {
            let (h, s) = wheel_from_pos(center, RADIUS, p);
            result = Some(WheelEdit { hue: h, sat: s, commit: true });
        }
    }
    if resp.drag_stopped() {
        // Commit the caller's current (already-applied) value on release.
        result = Some(WheelEdit { hue, sat, commit: true });
    }

    // Per-control reset (→ neutral sat 0), dim when already neutral.
    let reset_rect = egui::Rect::from_center_size(
        pos2(center.x, rect.bottom() - 8.0),
        vec2(16.0, 16.0),
    );
    let reset_resp = ui.interact(reset_rect, base_id.with("wheel_reset"), Sense::click());
    let modified = sat > 0.0;
    let reset_color = if modified {
        if reset_resp.hovered() {
            theme::ACCENT_BRIGHT
        } else {
            theme::TEXT_FAINT
        }
    } else {
        theme::BORDER_STRONG
    };
    draw_reset_arrow(ui.painter(), reset_rect.center(), RESET_R, reset_color);
    if reset_resp.clicked() && modified {
        result = Some(WheelEdit { hue, sat: 0.0, commit: true });
    }

    result
}
```

- [ ] **Step 4: Register the module**

In `ferrolite-app/src/widgets/mod.rs`, add `pub mod color_wheel;` alongside the other widget modules. (Check whether `mod.rs` re-exports widgets like `draw_reset_arrow`/`EguiSlider` with `pub use`; if it does, add `pub use color_wheel::{color_wheel, WheelEdit};` to match the established pattern — otherwise leave the module path and reference it as `crate::widgets::color_wheel::color_wheel` from Task 7.)

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-app color_wheel::tests`
Expected: PASS.

Then `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — clean.

> `wheel_pos`/`wheel_from_pos`/`hsv_color` are each called from `color_wheel`, so they are not dead. `color_wheel` itself has no caller until Task 7. If the `widgets` module is part of the crate's public lib API (like `icons`), a `pub fn` won't trip `dead_code` and no annotation is needed. If clippy DOES flag `color_wheel` as unused (widgets is bin-only / not pub-reachable), add a scoped `#[allow(dead_code)] // wired into the Grade tab in Task 7 — remove then` directly on `pub fn color_wheel`, and record it in your report so Task 7 removes it. Do NOT blanket-allow the module, and do NOT annotate the helpers.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/widgets/color_wheel.rs ferrolite-app/src/widgets/mod.rs
git commit -m "feat(app): reusable hue-sat color_wheel widget (egui mesh, id-salted, per-control reset)"
```

---

### Task 7: Grade tab (widget + `GradeTab` + icon)

Assemble the Grade panel: four wheel rows (Shadows/Midtones/Highlights/Global) each with a Lum slider, then Blending + Balance sliders, routed through `set_color_grade`. Register a new `GradeTab: PanelTab` in `base_tabs()`, and add the `GRADE` icon alias, shown as the panel's heading.

**Files:**
- Create: `ferrolite-app/src/develop/grade_widget.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod grade_widget;`)
- Modify: `ferrolite-app/src/develop/base_tabs.rs` (`GradeTab` + add to `base_tabs()`)
- Modify: `ferrolite-app/src/icons.rs` (`GRADE` alias + test-list entry)
- Modify: `ferrolite-app/src/develop/tool.rs` (test `standard_registry_has_the_shipped_tools_in_order` base-tab count 5 → 6)
- Test: `ferrolite-app/src/develop/grade_widget.rs` (pure helper), `icons.rs` (existing list test)

**Interfaces:**
- Consumes: `crate::widgets::color_wheel::{color_wheel, WheelEdit}` (Task 6), `crate::widgets::slider::EguiSlider`, `crate::develop::ops_edit::set_color_grade` (Task 5), `ferrolite_pipeline::{ColorGrade, GradeWheel, OpKind, OpStack}`, `crate::icons`, `crate::theme`.
- Produces: `pub fn show(ui: &mut egui::Ui, stack: &OpStack) -> Option<EditOutcome>`; `pub struct GradeTab` (`PanelTab`); `pub const icons::GRADE`.

- [ ] **Step 1: Add the icon alias (+ its test entry)**

In `ferrolite-app/src/icons.rs`, add near `COLOR`:
```rust
pub const GRADE: &str = p::CIRCLES_THREE;
```
> Verify `CIRCLES_THREE` exists in `egui_phosphor::regular` (it should). If it does not compile, substitute another color-grading-appropriate regular Phosphor const (candidates: `DROP`, `PAINT_BUCKET`) and note the substitution in your report.

Add `("GRADE", GRADE),` to the `every_alias_is_nonempty` test's array so the alias stays covered.

- [ ] **Step 2: Write the failing pure-helper test**

Create `ferrolite-app/src/develop/grade_widget.rs` with the tests + a pure helper (implemented in Step 4). Add:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{ColorGrade, GradeWheel};

    #[test]
    fn grade_changed_detects_a_wheel_edit() {
        let a = ColorGrade::default();
        let b = ColorGrade {
            shadows: GradeWheel { hue: 210.0, sat: 0.3, lum: 0.0 },
            ..Default::default()
        };
        assert!(grade_changed(&a, &b));
        assert!(!grade_changed(&a, &ColorGrade::default()));
    }

    #[test]
    fn grade_changed_detects_a_slider_edit() {
        let a = ColorGrade::default();
        let b = ColorGrade { balance: -0.4, ..Default::default() };
        assert!(grade_changed(&a, &b));
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p ferrolite-app grade_widget::tests`
Expected: FAIL to compile (`grade_changed` undefined / module not registered).

- [ ] **Step 4: Implement the Grade widget**

Write the module body (above the tests) in `ferrolite-app/src/develop/grade_widget.rs`:

```rust
//! The Grade tab: four color-grading wheels (Shadows/Midtones/Highlights/Global),
//! each with a Lum slider, plus Blending and Balance sliders. Reuses the shared
//! `color_wheel` widget 4× (id-salted) and routes every edit through the
//! identity-eliding `ops_edit::set_color_grade`. Per-control reset lives on each
//! wheel (its own reset → neutral) and each `EguiSlider` (its reset column).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::ops_edit::set_color_grade;
use crate::icons;
use crate::theme;
use crate::widgets::color_wheel::color_wheel;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{ColorGrade, GradeWheel, OpKind, OpStack};

/// True when any wheel or the blending/balance sliders differ (emit gate).
pub(crate) fn grade_changed(a: &ColorGrade, b: &ColorGrade) -> bool {
    a != b
}

/// Draw one wheel + its Lum slider; mutate `wheel` in place. Returns
/// `(changed, commit)`.
fn wheel_row(
    ui: &mut egui::Ui,
    id_source: &'static str,
    label: &str,
    wheel: &mut GradeWheel,
) -> (bool, bool) {
    let mut changed = false;
    let mut commit = false;
    ui.label(egui::RichText::new(label).color(theme::TEXT_FAINT));
    if let Some(e) = color_wheel(ui, id_source, wheel.hue, wheel.sat) {
        wheel.hue = e.hue;
        wheel.sat = e.sat;
        changed = true;
        commit |= e.commit;
    }
    let mut lum = wheel.lum;
    let r = ui.add(EguiSlider {
        label: "Lum",
        value: &mut lum,
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        signed: true,
    });
    if r.changed() {
        wheel.lum = lum;
        changed = true;
        commit |= r.drag_stopped() || !r.dragged();
    }
    (changed, commit)
}

pub fn show(ui: &mut egui::Ui, stack: &OpStack) -> Option<EditOutcome> {
    let mut cg = stack.color_grade().unwrap_or_default();
    let before = cg;

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(icons::GRADE).font(icons::font(14.0)));
        ui.label(egui::RichText::new("Color Grading").color(theme::TEXT_FAINT));
    });

    let mut changed = false;
    let mut commit = false;
    for (id, label, wheel) in [
        ("grade_shadows", "Shadows", &mut cg.shadows),
        ("grade_midtones", "Midtones", &mut cg.midtones),
        ("grade_highlights", "Highlights", &mut cg.highlights),
        ("grade_global", "Global", &mut cg.global),
    ] {
        let (c, m) = wheel_row(ui, id, label, wheel);
        changed |= c;
        commit |= m;
    }

    ui.separator();
    let mut blending = cg.blending;
    let rb = ui.add(EguiSlider {
        label: "Blending",
        value: &mut blending,
        min: 0.0,
        max: 1.0,
        default: 0.5,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: false,
        signed: false,
    });
    if rb.changed() {
        cg.blending = blending;
        changed = true;
        commit |= rb.drag_stopped() || !rb.dragged();
    }
    let mut balance = cg.balance;
    let rbal = ui.add(EguiSlider {
        label: "Balance",
        value: &mut balance,
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        signed: true,
    });
    if rbal.changed() {
        cg.balance = balance;
        changed = true;
        commit |= rbal.drag_stopped() || !rbal.dragged();
    }

    if !changed || !grade_changed(&before, &cg) {
        return None;
    }
    Some(EditOutcome {
        stack: set_color_grade(stack, cg),
        kind: OpKind::ColorGrade,
        commit,
    })
}
```

> Note on the `for` loop over a `[(…, &mut …)]` array: this borrows each wheel field mutably for one iteration; since the array is built inline and consumed by the loop, the borrows don't overlap. If the borrow checker rejects the array form, fall back to four explicit `wheel_row(ui, "grade_shadows", "Shadows", &mut cg.shadows)` calls (same behavior) — do not restructure `ColorGrade`.

- [ ] **Step 5: Register the module + tab**

- If Task 6 added an interim `#[allow(dead_code)]` on `pub fn color_wheel`, **remove it now** — `wheel_row` calls `color_wheel`, so it is no longer dead. Confirm `color_wheel` is actually referenced from `grade_widget.rs` before deleting the attribute.
- In `ferrolite-app/src/develop/mod.rs`, add `pub mod grade_widget;`.
- In `ferrolite-app/src/develop/base_tabs.rs`, add the tab struct (near `ColorTab`):
```rust
pub struct GradeTab;
impl PanelTab for GradeTab {
    fn id(&self) -> TabId {
        TabId("grade")
    }
    fn label(&self) -> &str {
        "Grade"
    }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        let stack = state.viewer.as_ref()?.op_stack.clone();
        crate::develop::grade_widget::show(ui, &stack)
    }
}
```
- Insert `Box::new(GradeTab),` into the `base_tabs()` vec, after `Box::new(ColorTab),`:
```rust
pub fn base_tabs() -> Vec<Box<dyn PanelTab>> {
    vec![
        Box::new(LightTab),
        Box::new(ColorTab),
        Box::new(GradeTab),
        Box::new(CurveTab),
        Box::new(DetailTab),
        Box::new(OpticsTab),
    ]
}
```

- [ ] **Step 6: Fix the base-tab-count test**

In `ferrolite-app/src/develop/tool.rs`, the test `standard_registry_has_the_shipped_tools_in_order` asserts `reg.base_tabs().len() == 5, "Light/Color/Curve/Detail/Optics"`. Update to `6` and the message to `"Light/Color/Grade/Curve/Detail/Optics"`.

- [ ] **Step 7: Run tests + clippy + build**

Run: `cargo test -p ferrolite-app grade_widget::tests icons::tests develop::tool::tests`
Expected: PASS.
Run: `cargo build --workspace` and `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add ferrolite-app/src/develop/grade_widget.rs ferrolite-app/src/develop/mod.rs ferrolite-app/src/develop/base_tabs.rs ferrolite-app/src/icons.rs ferrolite-app/src/develop/tool.rs
git commit -m "feat(app): Grade tab with 4 color-grading wheels + blending/balance"
```

---

### Task 8: Workspace green gate + self-review

Final verification against the CLAUDE.md gate.

**Files:** none (verification) — plus any small fixups the gate surfaces.

- [ ] **Step 1: Format** — `cargo fmt --all` then `cargo fmt --all --check` (no diff).
- [ ] **Step 2: Clippy** — `cargo clippy --workspace --all-targets -- -D warnings` (clean; watch for now-unused imports after the wiring).
- [ ] **Step 3: Tests** — `cargo test --workspace`. Expected PASS. GPU goldens run on this dev box and must be green (existing + new `color_grade.png`); headless CI skips them.
  > Known pre-existing/environmental failures NOT caused by this branch (do not chase): a timing flake in `ferrolite-app state::tests::cancel_pending_jobs_drains_thumb_handles` (passes in isolation), and `ferrolite-decode` tests that fail when local uncommitted `.ARW` files sit in `fixtures/raw/`. Confirm any workspace failure is one of these (branch touches neither subsystem) before treating the gate as green — re-run the specific failing test in isolation to confirm.
- [ ] **Step 4: Self-review against the spec.** Confirm each §4 requirement maps to a task: op model + renumber (§4.1 → T1), `color_grade_px` reusable (§4.2/§2.5 → T2), uniform + per-pixel node no-halo (§4.2 → T3/T4), new `color_wheel.rs` reused 4× (§4.3 → T6/T7), Grade `PanelTab` + icon (§4.3 → T7), per-control reset everywhere (§2.4 → T6/T7), tests incl. golden (§4.4 → T2/T4). Confirm the `opkind_renumber_does_not_change_serde_output` guard is intact and `color_grade_px` is public & pure.
- [ ] **Step 5: Commit any gate fixups**
```bash
git add -A
git commit -m "chore(p3-color-grading): workspace gate green (fmt/clippy/test)"
```

---

## Visual test plan (hand to the author after the gate is green — per CLAUDE.md)

This branch adds a reachable **Grade** tab and a new per-pixel GPU pass, so hands-on testing is required. Open an image in Develop.

1. **Grade tab present** — a new **Grade** tab appears in the right-panel tab strip (after Color). Selecting it shows a "Color Grading" heading with an icon, then four labeled wheels (Shadows/Midtones/Highlights/Global) each over a Lum slider, then Blending and Balance sliders. *Failure:* tab missing, heading icon renders as tofu/□, or controls missing.
   - *Design note for your call:* the icon shows as the panel **heading** (the tab chip itself stays text "Grade", matching the other 5 text tabs). If you'd rather the icon sit on the tab chip, that's a small follow-up (needs a `PanelTab::icon()` addition) — tell me.
2. **Wheels tint by region** — on **Shadows**, drag the thumb toward blue; the image's dark tones cool, highlights stay ~unchanged. On **Highlights**, drag toward orange; brights warm, shadows stay. On **Global**, any tint shifts the whole image. *Failure:* a region tints the wrong tones, or the wheel thumb doesn't track the pointer (hue = angle, sat = distance from center).
3. **Lum sliders** — each wheel's Lum brightens/darkens its region only (Global's affects everything). *Failure:* Lum tints instead of brightening, or affects the wrong region.
4. **Blending / Balance** — raise **Blending**: region transitions get softer/wider (more overlap). Move **Balance**: the shadow↔highlight split shifts (negative → more of the image reads as "highlight"). *Failure:* no visible effect, or they tint/brighten instead of reshaping regions.
5. **Per-control reset** — each wheel has its own reset affordance that returns just that wheel to neutral (sat 0) without touching the others or its Lum; each slider's reset column returns just it to default (Blending→0.5, Balance→0). *Failure:* a reset missing, or one reset disturbs a neighbor.
6. **Identity elision** — reset all four wheels (sat 0) and both Lums to 0 → the grade op drops and the image returns exactly to its pre-grade state (Blending/Balance alone leave no residual). *Failure:* a fully-neutral grade still alters the image.
7. **Responsiveness / no freeze** — drag a wheel or slider quickly; the preview updates smoothly with no multi-second stall on first grade edit or tab switch (pipeline built once; only the uniform uploads). *Failure:* a freeze/hitch on first edit.
8. **Persistence round-trip** — make a multi-wheel grade, close & reopen the image (sidecar reload) — the exact grade returns. A pre-P3 image (or one edited only in earlier tabs) opens unchanged. *Failure:* grade not restored, or an unexpected grade appears.
9. **Interaction with other tabs** — a grade plus edits in Light/Color/Curve all coexist (grade applies after HSL in the pipeline); toggling between tabs preserves each. *Failure:* switching tabs loses the grade or reorders visible effects.

**Fixtures:** a shot with clear shadow/highlight separation (e.g. a backlit scene or a portrait with bright sky) makes the per-region tinting easiest to judge. Optional offline glance: `ferrolite-pipeline/tests/fixtures/color_grade.png`.
