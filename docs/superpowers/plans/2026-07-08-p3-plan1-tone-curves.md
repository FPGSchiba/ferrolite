# P3 Plan 1 — Advanced Tone Curves Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the existing `ToneCurve` op with Lightroom-parity per-channel (Master/R/G/B) point curves plus a parametric region editor (Highlights/Lights/Darks/Shadows + 3 split points), baked into three per-channel GPU LUTs, all as a GLOBAL op.

**Architecture:** Additive `#[serde(default)]` fields on `ToneCurve` keep pre-P3 sidecars byte-compatible (new fields → identity). A new pure `parametric_curve_lut` and a `tone_curve_luts` compositor bake three per-channel final LUTs `finalₖ(x) = channelₖ(master(parametric(x)))` on the CPU (256-entry passes, no GPU). The tone-curve compute shader moves from one shared 256-LUT to three packed R/G/B LUTs (one 768-entry storage buffer, row per channel); the pipeline is built once and only the uploaded LUT bytes change per edit. The Curve tab gains a channel selector + parametric sub-panel; every control keeps a per-control reset.

**Tech Stack:** Rust, `ferrolite-pipeline` (op model + CPU LUT math + WGSL compute node), `ferrolite-app` (egui Curve tab), `wgpu`, `bytemuck`, `serde`/`serde_json`.

## Global Constraints

Copied verbatim from the P3 design (`docs/superpowers/specs/2026-07-08-p3-tone-and-color-grading-design.md`) §2/§3 and the v2 architecture map §5; every task's requirements implicitly include these.

- **Global-only op.** No per-mask / `LocalAdjustments` work — that is the deferred "P3-local" spec. `ToneCurve` stays a global stack op.
- **Op-order unchanged.** `ToneCurve` keeps its existing discriminant (`OpKind::ToneCurve = 3`). Plan 1 inserts nothing before it and performs **no `OpKind` renumber**. Keep the `opkind_renumber_does_not_change_serde_output` guard test intact.
- **Back-compat (contract 2).** All new state is additive `#[serde(default)]` on the op structs; a sidecar written before P3 must deserialize to today's exact behavior (new fields = identity). Catalog stays a pure cache; op params live only in the OpStack `.xmp`/JSON sidecar.
- **Contract 4 (GPU executor is photo-agnostic).** `ferrolite-gpu`'s `Graph<PipelineImage>` executor is NOT modified. The curve stays a LUT-node extension supplied by `ferrolite-pipeline`.
- **Build-once GPU (CLAUDE.md).** Build the tone-curve pipeline/shader ONCE and reuse it; only the uploaded LUT data changes per edit. Never rebuild per image/open/interaction. No UI-thread blocking.
- **Reusable-math constraint (§2.5).** The core transform must be a pure function in `ferrolite-pipeline`: add `parametric_curve_lut(&ParametricCurve) -> [f32; 256]` (and the `tone_curve_luts` compositor), independent of node/shader wiring, so the future per-mask path reuses it with no rework. No transform logic may live only inside a node's `evaluate`/shader.
- **Per-control reset (CLAUDE.md, load-bearing).** Every new control (each channel curve, the mode selector, each parametric slider) exposes its own reset-to-default affordance. Reuse `widgets::draw_reset_arrow` + the `EguiSlider` reset column + the `curve_editor`'s built-in Reset.
- **UI icons (CLAUDE.md).** No raw glyphs / hand-drawn `Painter` icons. Plan 1 needs no new icon (channel selector uses text labels; per the design's §6 UI table the Curve tab adds no new icon alias).
- **No new dependencies, no engine-tier edits, no copyleft.** Pure-Rust math in the photo tier (`ferrolite-pipeline` + `ferrolite-app`) only.
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` must be clean; 100-col width; no `unwrap()` outside tests; immutable-by-default.
- **Identity elision:** a `ToneCurve` whose Master + R + G + B curves and parametric are all identity is dropped from the stack (mirrors every other `set_*` helper in `ops_edit`).

**Branch:** `feat/p3-tone-curves` off `main` (create it before Task 1 if not already on it).

---

### Task 1: Extend the `ToneCurve` op model (back-compat)

Add the per-channel point curves + parametric region curve as defaulted fields, and identity helpers. This is a compile-forcing change: every `ToneCurve { .. }` literal in the workspace must gain the new fields (use `..Default::default()`).

**Files:**
- Modify: `ferrolite-pipeline/src/op.rs` (add `PointCurve`, `ParametricCurve`; extend `ToneCurve`; add `is_identity` helpers; tests)
- Modify: `ferrolite-pipeline/src/lib.rs:30-33` (export `PointCurve`, `ParametricCurve`)
- Modify (literal fixups): `ferrolite-pipeline/src/serialize.rs:68`, `ferrolite-pipeline/tests/golden.rs:156,171,249`, `ferrolite-app/src/develop/curve_widget.rs:51`
- Test: `ferrolite-pipeline/src/op.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `pub struct PointCurve { pub points: Vec<(f32, f32)>, #[serde(default)] pub mode: CurveMode }` — derives `Clone, PartialEq, Debug, Default, Serialize, Deserialize`.
  - `pub struct ParametricCurve { pub highlights: f32, pub lights: f32, pub darks: f32, pub shadows: f32, pub shadow_split: f32, pub midtone_split: f32, pub highlight_split: f32 }` — derives `Clone, Copy, PartialEq, Debug, Serialize, Deserialize`; **manual** `Default` (splits 0.25/0.50/0.75, regions 0).
  - `ToneCurve` gains `#[serde(default)] pub red: PointCurve`, `green`, `blue`, `#[serde(default)] pub parametric: ParametricCurve`; now also derives `Default`.
  - `impl PointCurve { pub fn is_identity(&self) -> bool }`, `impl ParametricCurve { pub fn is_identity(&self) -> bool }`, `impl ToneCurve { pub fn is_identity(&self) -> bool }`.
- Consumes: existing `CurveMode` (Default = Linear).

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `ferrolite-pipeline/src/op.rs`:

```rust
#[test]
fn point_curve_default_is_identity() {
    let p = PointCurve::default();
    assert!(p.points.is_empty());
    assert_eq!(p.mode, CurveMode::Linear);
    assert!(p.is_identity());
}

#[test]
fn parametric_default_splits_are_quarter_half_threequarter() {
    let p = ParametricCurve::default();
    assert_eq!(p.shadow_split, 0.25);
    assert_eq!(p.midtone_split, 0.50);
    assert_eq!(p.highlight_split, 0.75);
    assert_eq!(
        (p.highlights, p.lights, p.darks, p.shadows),
        (0.0, 0.0, 0.0, 0.0)
    );
    assert!(p.is_identity(), "zero regions = identity regardless of splits");
}

#[test]
fn tone_curve_default_is_fully_identity() {
    let tc = ToneCurve::default();
    assert!(tc.is_identity());
    assert!(tc.red.is_identity() && tc.green.is_identity() && tc.blue.is_identity());
    assert!(tc.parametric.is_identity());
}

#[test]
fn tone_curve_red_channel_makes_it_non_identity() {
    let tc = ToneCurve {
        red: PointCurve {
            points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
            mode: CurveMode::Smooth,
        },
        ..Default::default()
    };
    assert!(!tc.is_identity(), "a non-identity red curve makes the op non-identity");
}

#[test]
fn tone_curve_parametric_makes_it_non_identity() {
    let tc = ToneCurve {
        parametric: ParametricCurve {
            shadows: 0.5,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(!tc.is_identity());
}

#[test]
fn pre_p3_tonecurve_loads_with_identity_new_fields() {
    // A sidecar written before P3 has only points + mode.
    let json = r#"{ "points": [[0.0,0.0],[1.0,1.0]], "mode": "Linear" }"#;
    let tc: ToneCurve = serde_json::from_str(json).unwrap();
    assert_eq!(tc.points, vec![(0.0, 0.0), (1.0, 1.0)]);
    assert!(tc.red.is_identity() && tc.green.is_identity() && tc.blue.is_identity());
    assert!(tc.parametric.is_identity());
}

#[test]
fn tonecurve_with_new_fields_roundtrips() {
    let tc = ToneCurve {
        points: vec![(0.0, 0.0), (1.0, 1.0)],
        mode: CurveMode::Smooth,
        red: PointCurve {
            points: vec![(0.0, 0.0), (0.4, 0.6), (1.0, 1.0)],
            mode: CurveMode::Smooth,
        },
        green: PointCurve::default(),
        blue: PointCurve::default(),
        parametric: ParametricCurve {
            shadows: 0.3,
            highlight_split: 0.8,
            ..Default::default()
        },
    };
    let s = serde_json::to_string(&tc).unwrap();
    assert_eq!(serde_json::from_str::<ToneCurve>(&s).unwrap(), tc);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline --lib op::tests`
Expected: FAIL to **compile** (`PointCurve`/`ParametricCurve` unknown, `ToneCurve` has no `red`/`parametric` fields, no `is_identity`).

- [ ] **Step 3: Implement the model change**

In `ferrolite-pipeline/src/op.rs`, replace the existing `ToneCurve` struct (currently lines 44-52) with the new structs + the extended `ToneCurve`. Insert `PointCurve` and `ParametricCurve` just above `ToneCurve`:

```rust
/// A single point-curve channel (control points + interpolation mode).
/// Identity = empty `points` (or `[(0,0),(1,1)]`). Reuses the shared
/// `curve_lut` bake; `Default` is identity so it is a valid `#[serde(default)]`.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct PointCurve {
    pub points: Vec<(f32, f32)>,
    #[serde(default)]
    pub mode: CurveMode,
}

impl PointCurve {
    /// True when this channel is the identity ramp (no effect).
    pub fn is_identity(&self) -> bool {
        points_are_identity(&self.points)
    }
}

/// Lightroom-style parametric region curve applied to all channels via the
/// composited LUT. Region values in `[-1,1]` (0 = identity); split points in
/// `[0,1]` partition the tonal range into Shadows|Darks|Lights|Highlights.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct ParametricCurve {
    pub highlights: f32,
    pub lights: f32,
    pub darks: f32,
    pub shadows: f32,
    pub shadow_split: f32,
    pub midtone_split: f32,
    pub highlight_split: f32,
}

impl Default for ParametricCurve {
    fn default() -> Self {
        // All region shifts 0, splits at the LR defaults → identity.
        Self {
            highlights: 0.0,
            lights: 0.0,
            darks: 0.0,
            shadows: 0.0,
            shadow_split: 0.25,
            midtone_split: 0.50,
            highlight_split: 0.75,
        }
    }
}

impl ParametricCurve {
    /// True when no region is shifted (splits alone have no effect).
    pub fn is_identity(&self) -> bool {
        self.highlights == 0.0 && self.lights == 0.0 && self.darks == 0.0 && self.shadows == 0.0
    }
}

/// Control points form the identity ramp when empty or exactly the two corners.
fn points_are_identity(points: &[(f32, f32)]) -> bool {
    points.is_empty()
        || (points.len() == 2
            && (points[0].0).abs() < 1e-6
            && (points[0].1).abs() < 1e-6
            && (points[1].0 - 1.0).abs() < 1e-6
            && (points[1].1 - 1.0).abs() < 1e-6)
}

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct ToneCurve {
    /// Master (RGB/luminance) curve — legacy field names, unchanged for
    /// back-compat. Baked to a 256-entry monotone LUT by `uniforms::curve_lut`.
    pub points: Vec<(f32, f32)>,
    /// Interpolation mode. Absent in pre-feature sidecars → Linear (serde default).
    #[serde(default)]
    pub mode: CurveMode,
    // New in P3 — all `#[serde(default)]` = identity, so pre-P3 sidecars load unchanged.
    #[serde(default)]
    pub red: PointCurve,
    #[serde(default)]
    pub green: PointCurve,
    #[serde(default)]
    pub blue: PointCurve,
    #[serde(default)]
    pub parametric: ParametricCurve,
}

impl ToneCurve {
    /// True when Master + R/G/B + parametric are all identity (op can be dropped).
    pub fn is_identity(&self) -> bool {
        points_are_identity(&self.points)
            && self.red.is_identity()
            && self.green.is_identity()
            && self.blue.is_identity()
            && self.parametric.is_identity()
    }
}
```

- [ ] **Step 4: Fix all `ToneCurve { .. }` literal sites in the pipeline crate**

The new required fields break existing literals. In `ferrolite-pipeline/src/op.rs` tests (the 3 sites at ~L366, ~L427, ~L462) and `ferrolite-pipeline/src/serialize.rs:68`, append `..Default::default()` to each `ToneCurve { points: .., mode: .. }`. Example for op.rs L366:

```rust
.set_op(Op::ToneCurve(ToneCurve {
    points: vec![(0.0, 0.0), (1.0, 1.0)],
    mode: CurveMode::Linear,
    ..Default::default()
}))
```

Apply the identical `..Default::default()` addition to op.rs L427, op.rs L462 (`let tc = ToneCurve { .. }`), and serialize.rs L68.

- [ ] **Step 5: Export the new types**

In `ferrolite-pipeline/src/lib.rs`, add `ParametricCurve` and `PointCurve` to the `pub use op::{ .. }` list (line ~30-33), keeping it alphabetical-ish:

```rust
pub use op::{
    Aspect, Contrast, Correction, CropRect, CurveMode, Exposure, Geometry, Hsl, HslBand,
    LensCorrection, Op, OpKind, OpStack, ParametricCurve, PointCurve, Sharpen, ToneCurve,
    WhiteBalance, STACK_VERSION,
};
```

- [ ] **Step 6: Fix the app-side `ToneCurve` literal so the workspace compiles**

`ferrolite-app/src/develop/curve_widget.rs:51` constructs `Op::ToneCurve(ToneCurve { points: edit.points, mode: edit.mode })`. Add `..Default::default()`:

```rust
stack: stack.set_op(Op::ToneCurve(ToneCurve {
    points: edit.points,
    mode: edit.mode,
    ..Default::default()
})),
```

(Task 6 rewrites this file; this keeps the workspace green in the interim.)

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --lib op::tests`
Expected: PASS (all new tests + existing op tests green).

- [ ] **Step 8: Verify the whole workspace still compiles**

Run: `cargo build --workspace`
Expected: builds clean (curve_widget.rs literal fixed).

- [ ] **Step 9: Commit**

```bash
git add ferrolite-pipeline/src/op.rs ferrolite-pipeline/src/lib.rs ferrolite-pipeline/src/serialize.rs ferrolite-pipeline/tests/golden.rs ferrolite-app/src/develop/curve_widget.rs
git commit -m "feat(pipeline): extend ToneCurve with per-channel + parametric curves (serde-default, back-compat)"
```

---

### Task 2: `parametric_curve_lut` pure function

Bake the parametric region curve into a 256-entry display-linear LUT: a smooth partition-of-unity over four tonal regions, offsetting each band by its region value, forced monotone non-decreasing.

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs` (add `MAX_PARAMETRIC_SHIFT`, `region_weights`, `parametric_curve_lut`; tests)
- Modify: `ferrolite-pipeline/src/lib.rs:42-46` (export `parametric_curve_lut`)
- Test: `ferrolite-pipeline/src/uniforms.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::op::ParametricCurve` (Task 1).
- Produces:
  - `pub const MAX_PARAMETRIC_SHIFT: f32 = 0.25;`
  - `pub fn parametric_curve_lut(p: &ParametricCurve) -> [f32; 256]` — identity ramp when all regions are 0; monotone non-decreasing; values in `[0,1]`.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `ferrolite-pipeline/src/uniforms.rs`:

```rust
#[test]
fn parametric_identity_is_a_linear_ramp() {
    use crate::op::ParametricCurve;
    let lut = parametric_curve_lut(&ParametricCurve::default());
    for i in 0..256 {
        assert!(
            (lut[i] - i as f32 / 255.0).abs() < 1e-4,
            "identity parametric must be the identity ramp at {i}"
        );
    }
}

#[test]
fn parametric_is_monotone_non_decreasing() {
    use crate::op::ParametricCurve;
    // Opposing extreme regions — still must not dip.
    let p = ParametricCurve {
        shadows: 1.0,
        darks: -1.0,
        lights: 1.0,
        highlights: -1.0,
        ..Default::default()
    };
    let lut = parametric_curve_lut(&p);
    for i in 1..256 {
        assert!(lut[i] >= lut[i - 1] - 1e-6, "dipped at {i}");
        assert!((0.0..=1.0).contains(&lut[i]), "out of range at {i}: {}", lut[i]);
    }
}

#[test]
fn raising_shadows_lifts_low_end_only() {
    use crate::op::ParametricCurve;
    let p = ParametricCurve {
        shadows: 1.0,
        ..Default::default()
    };
    let lut = parametric_curve_lut(&p);
    // Low quarter is lifted above the identity ramp.
    let x_lo = 32usize;
    assert!(lut[x_lo] > x_lo as f32 / 255.0 + 0.01, "shadows lifted low end");
    // The far highlight end is essentially untouched.
    let x_hi = 240usize;
    assert!(
        (lut[x_hi] - x_hi as f32 / 255.0).abs() < 0.02,
        "highlights end unchanged by a shadows lift"
    );
}

#[test]
fn raising_highlights_lifts_high_end_only() {
    use crate::op::ParametricCurve;
    let p = ParametricCurve {
        highlights: 1.0,
        ..Default::default()
    };
    let lut = parametric_curve_lut(&p);
    let x_hi = 224usize;
    assert!(lut[x_hi] > x_hi as f32 / 255.0 + 0.01, "highlights lifted high end");
    let x_lo = 16usize;
    assert!(
        (lut[x_lo] - x_lo as f32 / 255.0).abs() < 0.02,
        "shadows end unchanged by a highlights lift"
    );
}

#[test]
fn out_of_order_splits_do_not_panic_and_stay_monotone() {
    use crate::op::ParametricCurve;
    // User dragged splits into a degenerate/reversed order.
    let p = ParametricCurve {
        shadows: 0.5,
        highlights: 0.5,
        shadow_split: 0.9,
        midtone_split: 0.1,
        highlight_split: 0.5,
        ..Default::default()
    };
    let lut = parametric_curve_lut(&p);
    for i in 1..256 {
        assert!(lut[i] >= lut[i - 1] - 1e-6, "dipped at {i}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::parametric`
Expected: FAIL to compile (`parametric_curve_lut` not defined).

- [ ] **Step 3: Implement `parametric_curve_lut`**

Add to `ferrolite-pipeline/src/uniforms.rs` (after `curve_lut`, before the uniform structs). Note the existing private `smoothstep` helper already lives lower in this file (used by `light_color_apply`); reuse it — do not redefine.

```rust
/// Maximum tonal shift (in display-linear `[0,1]`) applied by a single region at
/// its extreme (region value ±1). Pragmatic constant (image science secondary,
/// same spirit as `wb_multipliers`); a region of +1 lifts its band by ~0.25.
pub const MAX_PARAMETRIC_SHIFT: f32 = 0.25;

/// Partition-of-unity weights for the four parametric regions
/// [shadows, darks, lights, highlights] at sample `x`, given the four region
/// centers (strictly ascending). Weights sum to 1 everywhere and transition
/// smoothly (smoothstep) between adjacent centers; flat past the end centers.
fn region_weights(x: f32, centers: [f32; 4]) -> [f32; 4] {
    let mut w = [0.0f32; 4];
    if x <= centers[0] {
        w[0] = 1.0;
        return w;
    }
    if x >= centers[3] {
        w[3] = 1.0;
        return w;
    }
    for k in 0..3 {
        if x >= centers[k] && x <= centers[k + 1] {
            // smoothstep guards a degenerate (equal) center pair by clamping to 0/1.
            let t = smoothstep(centers[k], centers[k + 1], x);
            w[k] = 1.0 - t;
            w[k + 1] = t;
            return w;
        }
    }
    w[3] = 1.0; // numerical fallthrough (shouldn't hit)
    w
}

/// Bake a parametric region curve into a 256-entry display-linear LUT. Each
/// sample is offset by the weighted sum of the four region shifts, then the
/// result is clamped to `[0,1]` and forced monotone non-decreasing (mirroring
/// `curve_lut`). All-zero regions → the identity ramp. Pure — no GPU.
pub fn parametric_curve_lut(p: &crate::op::ParametricCurve) -> [f32; 256] {
    // Sanitize splits into ascending order in [0,1] so a user-dragged
    // out-of-order set can't produce non-ascending centers.
    let s1 = p.shadow_split.clamp(0.0, 1.0);
    let s2 = p.midtone_split.clamp(0.0, 1.0).max(s1);
    let s3 = p.highlight_split.clamp(0.0, 1.0).max(s2);
    let centers = [
        s1 * 0.5,
        (s1 + s2) * 0.5,
        (s2 + s3) * 0.5,
        (s3 + 1.0) * 0.5,
    ];
    let region = [p.shadows, p.darks, p.lights, p.highlights];

    let mut lut = [0.0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let x = i as f32 / 255.0;
        let w = region_weights(x, centers);
        let shift = MAX_PARAMETRIC_SHIFT
            * (region[0] * w[0] + region[1] * w[1] + region[2] * w[2] + region[3] * w[3]);
        *slot = (x + shift).clamp(0.0, 1.0);
    }
    for i in 1..256 {
        if lut[i] < lut[i - 1] {
            lut[i] = lut[i - 1];
        }
    }
    lut
}
```

- [ ] **Step 4: Export it**

In `ferrolite-pipeline/src/lib.rs`, add **only** `parametric_curve_lut` to the existing `pub use uniforms::{ .. }` block (line ~42-46), next to `curve_lut` — do not add or reorder any other item. Update the preceding comment so it notes `parametric_curve_lut` (and, after Task 3, `tone_curve_luts`) are the pure reusable exceptions mandated by design §2.5. The block becomes:

```rust
// The uniform structs are exported as the documented GPU memory layout the
// edit passes consume. Most param→uniform helpers are crate-internal; the pure
// LUT-baking fns (`curve_lut`, `parametric_curve_lut`, `tone_curve_luts`) are
// public per design §2.5 so the future per-mask path reuses them with no rework.
// `sharpen_halo`/`lens_halo_px` are public for Plan 3's tile producer.
pub use uniforms::{
    curve_lut, geometry_tile_uniform, lens_halo_px, lens_uniform, parametric_curve_lut,
    sharpen_halo, vignette_amount, ContrastUniform, ExposureUniform, GeometryUniform, HslUniform,
    LensUniform, LocalAdjustUniform, SharpenUniform, VignetteUniform, WbUniform, MAX_SHARPEN_RADIUS,
};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): add pure parametric_curve_lut region baker"
```

---

### Task 3: Composite three per-channel final LUTs (`tone_curve_luts`)

Compose `finalₖ(x) = channelₖ(master(parametric(x)))` for k ∈ {R,G,B} into a `[[f32; 256]; 3]` (row per channel), with a LUT-sampling helper that mirrors the shader's interpolation.

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs` (add `sample_lut`, `compose_lut`, `tone_curve_luts`; tests)
- Modify: `ferrolite-pipeline/src/lib.rs:42-46` (export `tone_curve_luts`)
- Test: `ferrolite-pipeline/src/uniforms.rs`

**Interfaces:**
- Consumes: `crate::op::ToneCurve` (Task 1), `curve_lut`, `parametric_curve_lut` (Task 2).
- Produces:
  - `pub fn tone_curve_luts(tc: Option<&ToneCurve>) -> [[f32; 256]; 3]` — index 0=R,1=G,2=B. `None` (or fully-identity) → three identity ramps.
  - crate-internal `fn sample_lut(&[f32; 256], f32) -> f32` (mirrors `tone_curve.wgsl`'s `apply_lut`), `fn compose_lut(inner, outer) -> [f32; 256]` where `result[i] = sample_lut(outer, inner[i])`.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `ferrolite-pipeline/src/uniforms.rs`:

```rust
#[test]
fn tone_curve_luts_none_is_three_identity_ramps() {
    let luts = tone_curve_luts(None);
    for ch in 0..3 {
        for i in 0..256 {
            assert!(
                (luts[ch][i] - i as f32 / 255.0).abs() < 1e-4,
                "channel {ch} entry {i} must be identity"
            );
        }
    }
}

#[test]
fn master_only_curve_equals_legacy_lut_on_all_channels() {
    use crate::op::{CurveMode, ToneCurve};
    // A master-only edit must bake the SAME curve onto R, G and B (regression
    // guard: existing single-LUT goldens must stay valid).
    let pts = vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)];
    let tc = ToneCurve {
        points: pts.clone(),
        mode: CurveMode::Linear,
        ..Default::default()
    };
    let master = curve_lut(&pts, CurveMode::Linear);
    let luts = tone_curve_luts(Some(&tc));
    for ch in 0..3 {
        for i in 0..256 {
            assert!(
                (luts[ch][i] - master[i]).abs() < 1e-4,
                "channel {ch} entry {i}: {} vs master {}",
                luts[ch][i],
                master[i]
            );
        }
    }
}

#[test]
fn red_only_curve_changes_red_row_leaves_green_blue_identity() {
    use crate::op::{CurveMode, PointCurve, ToneCurve};
    let tc = ToneCurve {
        red: PointCurve {
            points: vec![(0.0, 0.0), (0.5, 0.2), (1.0, 1.0)],
            mode: CurveMode::Linear,
        },
        ..Default::default()
    };
    let luts = tone_curve_luts(Some(&tc));
    // Red midtone pulled below the diagonal.
    assert!(luts[0][128] < 128.0 / 255.0 - 0.02, "red midtones darkened");
    // Green and Blue remain the identity ramp.
    for ch in [1usize, 2usize] {
        for i in 0..256 {
            assert!(
                (luts[ch][i] - i as f32 / 255.0).abs() < 1e-4,
                "channel {ch} entry {i} must stay identity"
            );
        }
    }
}

#[test]
fn compose_order_is_channel_of_master_of_parametric() {
    use crate::op::{CurveMode, ParametricCurve, PointCurve, ToneCurve};
    // Parametric lifts shadows; master is identity; red darkens midtones.
    // The red row must equal red_curve( parametric(x) ) since master is identity.
    let param = ParametricCurve {
        shadows: 0.5,
        ..Default::default()
    };
    let red = PointCurve {
        points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
        mode: CurveMode::Linear,
    };
    let tc = ToneCurve {
        parametric: param,
        red: red.clone(),
        ..Default::default()
    };
    let luts = tone_curve_luts(Some(&tc));
    // Hand-compose the expected red row.
    let p_lut = parametric_curve_lut(&param);
    let r_lut = curve_lut(&red.points, red.mode);
    for i in 0..256 {
        let expected = sample_lut(&r_lut, p_lut[i]); // master identity is a no-op
        assert!(
            (luts[0][i] - expected).abs() < 2e-3,
            "red row entry {i}: {} vs expected {}",
            luts[0][i],
            expected
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests`
Expected: FAIL to compile (`tone_curve_luts`, `sample_lut` not defined).

- [ ] **Step 3: Implement the compositor**

Add to `ferrolite-pipeline/src/uniforms.rs` (after `parametric_curve_lut`):

```rust
/// Sample a 256-entry LUT at a continuous input `v`, mirroring
/// `tone_curve.wgsl`'s `apply_lut`: linear interpolation inside `[0,1]`, and
/// unit-slope extrapolation from the endpoints outside it (so an identity LUT is
/// exact pass-through). Kept in lock-step with the shader.
fn sample_lut(lut: &[f32; 256], v: f32) -> f32 {
    if v < 0.0 {
        return lut[0] + v;
    }
    if v > 1.0 {
        return lut[255] + (v - 1.0);
    }
    let x = v * 255.0;
    let i0 = x.floor() as usize;
    let i1 = (i0 + 1).min(255);
    let f = x - x.floor();
    lut[i0] * (1.0 - f) + lut[i1] * f
}

/// Compose two LUTs: `result[i] = sample_lut(outer, inner[i])` — i.e. apply
/// `inner` first, then `outer` (function composition `outer ∘ inner`).
fn compose_lut(inner: &[f32; 256], outer: &[f32; 256]) -> [f32; 256] {
    let mut out = [0.0f32; 256];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = sample_lut(outer, inner[i]);
    }
    out
}

/// Bake the three per-channel final tone-curve LUTs:
/// `finalₖ(x) = channelₖ( master( parametric(x) ) )` for k ∈ {R,G,B}.
/// Returns `[R, G, B]` rows. `None` (or a fully-identity curve) yields three
/// identity ramps. Pure — no GPU; the reusable transform per design §2.5.
pub fn tone_curve_luts(tc: Option<&crate::op::ToneCurve>) -> [[f32; 256]; 3] {
    let default = crate::op::ToneCurve::default();
    let tc = tc.unwrap_or(&default);
    let param = parametric_curve_lut(&tc.parametric);
    let master = curve_lut(&tc.points, tc.mode);
    let base = compose_lut(&param, &master); // master ∘ parametric
    let r = compose_lut(&base, &curve_lut(&tc.red.points, tc.red.mode));
    let g = compose_lut(&base, &curve_lut(&tc.green.points, tc.green.mode));
    let b = compose_lut(&base, &curve_lut(&tc.blue.points, tc.blue.mode));
    [r, g, b]
}
```

- [ ] **Step 4: Export it**

In `ferrolite-pipeline/src/lib.rs`, add `tone_curve_luts` to the `pub use uniforms::{ .. }` block (it was already named in the Task 2 comment). Keep `sample_lut`/`compose_lut` crate-internal (not exported).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests`
Expected: PASS (including the master-only regression guard).

- [ ] **Step 6: Commit**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): composite per-channel final tone-curve LUTs (channel∘master∘parametric)"
```

---

### Task 4: GPU shader + `CurveNode` per-channel LUTs

Move the tone-curve compute pass from one shared 256-LUT to three packed R/G/B LUTs (a single 768-entry storage buffer, row per channel). Build-once pipeline; only the uploaded bytes change. Rewire both pipelines. Existing master-only goldens must stay green; add a new combined golden.

**Files:**
- Modify: `ferrolite-pipeline/src/shaders/tone_curve.wgsl` (768-entry buffer, `apply_lut(v, ch)`)
- Modify: `ferrolite-pipeline/src/nodes.rs` (`CurveNode`: `Rc<Cell<[[f32; 256]; 3]>>`, 768-float buffer)
- Modify: `ferrolite-pipeline/src/pipeline.rs:15,18,41-42,120-128,172-173,267-277` (use `tone_curve_luts`; Cell type)
- Modify: `ferrolite-pipeline/src/tile_edit.rs:41,45,70,179-187,254,290-293` (same rewire)
- Test: `ferrolite-pipeline/tests/golden.rs` (keep existing; add combined golden)
- New golden fixture: `ferrolite-pipeline/tests/fixtures/tone_curve_p3.png` (authored on dev GPU)

**Interfaces:**
- Consumes: `tone_curve_luts` (Task 3), `ToneCurve` (Task 1).
- Produces: `CurveNode::new(ctx, lut: Rc<Cell<[[f32; 256]; 3]>>)` (changed param type). Shader binding-2 storage buffer becomes `array<f32, 768>` (r rows 0..256, g 256..512, b 512..768).

- [ ] **Step 1: Rewrite the shader**

Replace `ferrolite-pipeline/src/shaders/tone_curve.wgsl` entirely:

```wgsl
// Tone curve: three packed 256-entry display-linear LUTs (R,G,B rows) with
// linear interpolation between entries (identity ramp ⇒ exact identity).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
// 3 channels × 256 entries, row-major: R=[0,256), G=[256,512), B=[512,768).
@group(0) @binding(2) var<storage, read> lut: array<f32, 768>;

fn apply_lut(v: f32, ch: u32) -> f32 {
    let base = ch * 256u;
    // Preserve out-of-[0,1] values (P2 §5.3): extrapolate from the endpoints with
    // unit slope so highlights >1 and negatives pass through, instead of clamping.
    if (v < 0.0) { return lut[base] + v; }
    if (v > 1.0) { return lut[base + 255u] + (v - 1.0); }
    let x = v * 255.0;
    let i0 = u32(floor(x));
    let i1 = min(i0 + 1u, 255u);
    let f = x - floor(x);
    return mix(lut[base + i0], lut[base + i1], f);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    let rgb = vec3<f32>(apply_lut(c.r, 0u), apply_lut(c.g, 1u), apply_lut(c.b, 2u));
    textureStore(dst, xy, vec4<f32>(rgb, c.a));
}
```

- [ ] **Step 2: Update `CurveNode` in `nodes.rs`**

In `ferrolite-pipeline/src/nodes.rs`, change the `CurveNode` struct field, `new` signature, buffer size, and the `evaluate` upload. The bind-group layout `curve_bgl` is unchanged (still one read-only storage buffer at binding 2). Edits:

- Struct field (was `lut: Rc<Cell<[f32; 256]>>`):
```rust
    lut: Rc<Cell<[[f32; 256]; 3]>>,
```
- `CurveNode::new` param (was `lut: Rc<Cell<[f32; 256]>>`):
```rust
    pub(crate) fn new(ctx: Arc<GpuContext>, lut: Rc<Cell<[[f32; 256]; 3]>>) -> Self {
```
- Buffer size (was `size_of::<f32>() * 256`):
```rust
        let lut_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tone-curve-lut"),
            size: (std::mem::size_of::<f32>() * 768) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
```
- In `evaluate`, the upload is unchanged in shape (bytemuck handles the `[[f32; 256]; 3]: Pod` array): the existing
```rust
        let lut = self.lut.get();
        self.ctx.queue.write_buffer(&self.lut_buf, 0, bytemuck::bytes_of(&lut));
```
still compiles — `bytes_of` on `[[f32; 256]; 3]` writes 3072 contiguous bytes (R,G,B rows), matching the shader's `array<f32, 768>` layout. Update the adjacent comment (currently `// `[f32; 256]: Pod` ...`) to `// `[[f32; 256]; 3]: Pod` → 768 contiguous f32 (R,G,B rows).`

- [ ] **Step 3: Rewire `pipeline.rs`**

In `ferrolite-pipeline/src/pipeline.rs`:
- Import (line ~18): add `tone_curve_luts` to the `use crate::uniforms::{ .. }` list (it already imports `curve_lut`; keep `curve_lut` — it is still used elsewhere? if not, remove it to avoid an unused-import clippy error. Check after editing.).
- Struct field (line ~42): `tone_curve: Rc<Cell<[f32; 256]>>,` → `tone_curve: Rc<Cell<[[f32; 256]; 3]>>,`
- Construction (lines ~120-126): replace
```rust
        let tone_curve = Rc::new(Cell::new(curve_lut(
            &stack.tone_curve().map(|t| t.points).unwrap_or_default(),
            stack
                .tone_curve()
                .map(|t| t.mode)
                .unwrap_or(crate::op::CurveMode::Linear),
        )));
```
with
```rust
        let tone_curve = Rc::new(Cell::new(tone_curve_luts(stack.tone_curve().as_ref())));
```
- `set_stack` update (lines ~267-277): replace the `curve_lut(...)` block with
```rust
        let luts = tone_curve_luts(stack.tone_curve().as_ref());
        if luts != self.tone_curve.get() {
            self.tone_curve.set(luts);
            self.graph.mark_dirty(self.tone_curve_id);
        }
```

- [ ] **Step 4: Rewire `tile_edit.rs`**

In `ferrolite-pipeline/src/tile_edit.rs`, apply the same three edits as pipeline.rs:
- Import (line ~45): add `tone_curve_luts` to the `use crate::uniforms::{ .. }` list; drop `curve_lut` if it becomes unused.
- Field (line ~70): `tone_curve: Rc<Cell<[f32; 256]>>,` → `tone_curve: Rc<Cell<[[f32; 256]; 3]>>,`
- Construction (lines ~179-185): replace the `curve_lut(...)` `Rc::new(Cell::new(..))` with
```rust
        let tone_curve = Rc::new(Cell::new(tone_curve_luts(stack.tone_curve().as_ref())));
```
- The `set_stack`/rebind path (lines ~290-293) that currently calls `self.tone_curve.set(curve_lut(..))` → `self.tone_curve.set(tone_curve_luts(stack.tone_curve().as_ref()));`

- [ ] **Step 5: Verify it compiles + existing goldens still pass**

Run: `cargo build -p ferrolite-pipeline`
Expected: builds clean (no unused `curve_lut` import warning).

Run: `cargo test -p ferrolite-pipeline --test golden`
Expected on a GPU dev box: existing `tone_curve_darken_midtones_matches_golden`, `tone_curve_smooth_matches_golden`, `full_seven_op_stack_matches_golden` PASS (master-only path is bit-compatible with the old single LUT within the 4/255 golden tolerance). On headless CI these skip (no adapter) — that is expected.

> If a master-only golden drifts beyond tolerance, STOP: the compose/sample path is not neutral for the master-only case — do not re-author the golden to hide it. Debug `sample_lut`/`compose_lut` until master-only equals the legacy LUT (Task 3's `master_only_curve_equals_legacy_lut_on_all_channels` unit test is the oracle).

- [ ] **Step 6: Write the failing combined golden test**

Add to `ferrolite-pipeline/tests/golden.rs` (imports `PointCurve`, `ParametricCurve` — add them to the `use ferrolite_pipeline::{ .. }` list at the top of the file):

```rust
#[test]
fn tone_curve_per_channel_and_parametric_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Master smooth + a red channel curve + a parametric shadows lift.
    let stack = OpStack::default().set_op(Op::ToneCurve(ToneCurve {
        points: vec![(0.0, 0.0), (0.5, 0.55), (1.0, 1.0)],
        mode: CurveMode::Smooth,
        red: PointCurve {
            points: vec![(0.0, 0.0), (0.5, 0.35), (1.0, 1.0)],
            mode: CurveMode::Linear,
        },
        green: PointCurve::default(),
        blue: PointCurve {
            points: vec![(0.0, 0.05), (1.0, 0.95)],
            mode: CurveMode::Linear,
        },
        parametric: ParametricCurve {
            shadows: 0.4,
            highlights: -0.2,
            ..Default::default()
        },
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "tone_curve_p3.png");
}
```

- [ ] **Step 7: Run to author the golden (dev GPU) and confirm it passes**

Run: `cargo test -p ferrolite-pipeline --test golden tone_curve_per_channel_and_parametric_matches_golden`
Expected: on first run the fixture is absent → `assert_golden` authors `tests/fixtures/tone_curve_p3.png` and passes (prints `wrote golden ...`). Run it a **second** time — Expected: PASS against the committed fixture. Eyeball the PNG once: red should be visibly darker (red curve pulls red midtones), shadows lifted, highlights slightly reduced.

- [ ] **Step 8: Commit**

```bash
git add ferrolite-pipeline/src/shaders/tone_curve.wgsl ferrolite-pipeline/src/nodes.rs ferrolite-pipeline/src/pipeline.rs ferrolite-pipeline/src/tile_edit.rs ferrolite-pipeline/tests/golden.rs ferrolite-pipeline/tests/fixtures/tone_curve_p3.png
git commit -m "feat(pipeline): per-channel R/G/B tone-curve LUTs on GPU (768-entry packed buffer)"
```

---

### Task 5: `ops_edit::set_tone_curve` with identity elision

Add the app-side edit helper that maps a `ToneCurve` onto a new immutable `OpStack`, dropping the op when the whole curve is identity (mirrors every other `set_*`).

**Files:**
- Modify: `ferrolite-app/src/develop/ops_edit.rs` (add `set_tone_curve`; import `ToneCurve`, `Op`; tests)
- Test: `ferrolite-app/src/develop/ops_edit.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `ferrolite_pipeline::{ToneCurve, Op, OpStack, OpKind}` and `ToneCurve::is_identity` (Task 1).
- Produces: `pub fn set_tone_curve(s: &OpStack, tc: ToneCurve) -> OpStack`.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `ferrolite-app/src/develop/ops_edit.rs`:

```rust
#[test]
fn set_tone_curve_identity_removes_the_op() {
    use ferrolite_pipeline::ToneCurve;
    let s = set_tone_curve(&OpStack::default(), ToneCurve::default());
    assert!(s.tone_curve().is_none(), "fully-identity curve = no op");
    assert!(s.is_identity());
}

#[test]
fn set_tone_curve_master_edit_sets_the_op() {
    use ferrolite_pipeline::{CurveMode, ToneCurve};
    let tc = ToneCurve {
        points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
        mode: CurveMode::Smooth,
        ..Default::default()
    };
    let s = set_tone_curve(&OpStack::default(), tc.clone());
    assert_eq!(s.tone_curve(), Some(tc));
}

#[test]
fn set_tone_curve_channel_only_edit_is_kept() {
    use ferrolite_pipeline::{CurveMode, PointCurve, ToneCurve};
    let tc = ToneCurve {
        blue: PointCurve {
            points: vec![(0.0, 0.0), (0.5, 0.7), (1.0, 1.0)],
            mode: CurveMode::Linear,
        },
        ..Default::default()
    };
    let s = set_tone_curve(&OpStack::default(), tc);
    assert!(s.tone_curve().is_some(), "a blue-only curve is not identity");
}

#[test]
fn set_tone_curve_parametric_only_edit_is_kept() {
    use ferrolite_pipeline::{ParametricCurve, ToneCurve};
    let tc = ToneCurve {
        parametric: ParametricCurve {
            highlights: -0.5,
            ..Default::default()
        },
        ..Default::default()
    };
    let s = set_tone_curve(&OpStack::default(), tc);
    assert!(s.tone_curve().is_some());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-app ops_edit::tests::set_tone_curve`
Expected: FAIL to compile (`set_tone_curve` undefined).

- [ ] **Step 3: Implement `set_tone_curve`**

In `ferrolite-app/src/develop/ops_edit.rs`, extend the top-of-file `use` to bring in `ToneCurve` (the `Op` and `OpStack` are already imported):

```rust
use ferrolite_pipeline::{
    sharpen_halo, Contrast, Exposure, LensCorrection, Op, OpStack, Sharpen, ToneCurve,
    WhiteBalance,
};
```

Add the helper (next to `set_sharpen`):

```rust
/// Set the tone curve, or REMOVE the op entirely when the whole curve (Master +
/// R/G/B + parametric) is identity — so `is_identity()`/`has_edits` stay correct,
/// mirroring every other `set_*` helper here.
pub fn set_tone_curve(s: &OpStack, tc: ToneCurve) -> OpStack {
    if tc.is_identity() {
        s.reset(ferrolite_pipeline::OpKind::ToneCurve)
    } else {
        s.set_op(Op::ToneCurve(tc))
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-app ops_edit::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/develop/ops_edit.rs
git commit -m "feat(app): add set_tone_curve edit helper with full-identity elision"
```

---

### Task 6: Curve tab — channel selector + per-channel point curves

Rewrite the tone-curve adapter to add a Master/R/G/B channel selector (tinted per channel) above the reusable `curve_editor`, routing each channel's edits through `set_tone_curve`. Parametric passes through unchanged in this task (Task 7 adds its controls). Active channel is UI-only state held in egui memory.

**Files:**
- Modify: `ferrolite-app/src/develop/curve_widget.rs` (rewrite `show`; add `Channel` enum + `channel_style`)
- Test: `ferrolite-app/src/develop/curve_widget.rs` (`#[cfg(test)] mod tests` — pure helpers only; UI itself is visual)

**Interfaces:**
- Consumes: `widgets::curve::{curve_editor, CurveStyle, CurveEdit}`, `develop::curve_math`, `develop::ops_edit::set_tone_curve` (Task 5), `ferrolite_pipeline::{ToneCurve, PointCurve, CurveMode}`.
- Produces: unchanged public entry `pub fn show(ui: &mut egui::Ui, stack: &OpStack) -> Option<EditOutcome>` (called by `base_tabs::CurveTab`, no change there).

- [ ] **Step 1: Write the failing test (pure helper)**

Add a `#[cfg(test)] mod tests` at the bottom of `ferrolite-app/src/develop/curve_widget.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_channel_has_a_distinct_tint() {
        let m = channel_style(Channel::Master).curve_color;
        let r = channel_style(Channel::Red).curve_color;
        let g = channel_style(Channel::Green).curve_color;
        let b = channel_style(Channel::Blue).curve_color;
        assert!(m != r && r != g && g != b && b != m, "channel tints must differ");
    }

    #[test]
    fn channel_label_is_short_and_stable() {
        assert_eq!(Channel::Master.label(), "Master");
        assert_eq!(Channel::Red.label(), "R");
        assert_eq!(Channel::Green.label(), "G");
        assert_eq!(Channel::Blue.label(), "B");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p ferrolite-app curve_widget::tests`
Expected: FAIL to compile (`Channel`, `channel_style` undefined).

- [ ] **Step 3: Rewrite `curve_widget.rs`**

Replace the entire contents of `ferrolite-app/src/develop/curve_widget.rs` with (Task 7 will extend the parametric section — the parametric field is threaded through unchanged here):

```rust
//! Tone-curve adapter over the reusable `widgets::curve::curve_editor`. Adds a
//! Master/R/G/B channel selector (tinted per channel) above the curve; each
//! channel edits its own `PointCurve` (Master = the legacy `points`/`mode`).
//! Parametric region controls live in the sub-panel (see `parametric` section).
//! The whole `ToneCurve` is routed through `ops_edit::set_tone_curve`, which
//! drops the op when everything is identity. Active channel is UI-only state.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::ops_edit::set_tone_curve;
use crate::develop::{curve_math, curve_widget_parametric};
use crate::theme;
use crate::widgets::curve::{curve_editor, CurveStyle};
use egui::Color32;
use ferrolite_pipeline::{CurveMode, OpKind, OpStack, PointCurve, ToneCurve};

/// Which tone-curve channel the editor is currently editing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    Master,
    Red,
    Green,
    Blue,
}

impl Channel {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Channel::Master => "Master",
            Channel::Red => "R",
            Channel::Green => "G",
            Channel::Blue => "B",
        }
    }
    const ALL: [Channel; 4] = [Channel::Master, Channel::Red, Channel::Green, Channel::Blue];
}

/// Per-channel curve tint. Master reuses the app accent; R/G/B use their hue.
pub(crate) fn channel_style(ch: Channel) -> CurveStyle {
    match ch {
        Channel::Master => CurveStyle {
            curve_color: theme::ACCENT,
            point_color: theme::ACCENT_BRIGHT,
        },
        Channel::Red => CurveStyle {
            curve_color: Color32::from_rgb(0xe0, 0x6c, 0x6c),
            point_color: Color32::from_rgb(0xf0, 0x9a, 0x9a),
        },
        Channel::Green => CurveStyle {
            curve_color: Color32::from_rgb(0x6c, 0xd0, 0x7c),
            point_color: Color32::from_rgb(0x9a, 0xe6, 0xa6),
        },
        Channel::Blue => CurveStyle {
            curve_color: Color32::from_rgb(0x6c, 0x9c, 0xe0),
            point_color: Color32::from_rgb(0x9a, 0xc0, 0xf0),
        },
    }
}

/// Read the currently-selected channel from egui memory (UI-only; not persisted).
fn read_channel(ui: &egui::Ui, id: egui::Id) -> Channel {
    ui.memory(|m| m.data.get_temp::<Channel>(id))
        .unwrap_or(Channel::Master)
}

/// Borrow a channel's `(points, mode)` out of the `ToneCurve`. Master = the
/// legacy `points`/`mode`; R/G/B = the matching `PointCurve`.
fn channel_curve(tc: &ToneCurve, ch: Channel) -> (Vec<(f32, f32)>, CurveMode) {
    match ch {
        Channel::Master => (tc.points.clone(), tc.mode),
        Channel::Red => (tc.red.points.clone(), tc.red.mode),
        Channel::Green => (tc.green.points.clone(), tc.green.mode),
        Channel::Blue => (tc.blue.points.clone(), tc.blue.mode),
    }
}

/// Return a new `ToneCurve` with `ch`'s points+mode replaced.
fn with_channel(mut tc: ToneCurve, ch: Channel, points: Vec<(f32, f32)>, mode: CurveMode) -> ToneCurve {
    match ch {
        Channel::Master => {
            tc.points = points;
            tc.mode = mode;
        }
        Channel::Red => tc.red = PointCurve { points, mode },
        Channel::Green => tc.green = PointCurve { points, mode },
        Channel::Blue => tc.blue = PointCurve { points, mode },
    }
    tc
}

pub fn show(ui: &mut egui::Ui, stack: &OpStack) -> Option<EditOutcome> {
    let tc = stack.tone_curve().unwrap_or_default();
    let channel_id = ui.id().with("tone_curve_channel");
    let mut channel = read_channel(ui, channel_id);

    // ── Channel selector: Master / R / G / B, tinted so the active one reads.
    ui.horizontal(|ui| {
        for ch in Channel::ALL {
            let selected = ch == channel;
            if ui.selectable_label(selected, ch.label()).clicked() {
                channel = ch;
                ui.memory_mut(|m| m.data.insert_temp(channel_id, channel));
            }
        }
    });

    let (points, stored_mode) = channel_curve(&tc, channel);
    // A never-edited channel (empty points) starts in Smooth — the new-curve
    // default (Linear only exists for pre-feature master sidecars).
    let display_points = if points.is_empty() {
        curve_math::identity_points()
    } else {
        points
    };
    let display_mode = if curve_math::is_identity(&display_points) {
        CurveMode::Smooth
    } else {
        stored_mode
    };

    let mut out: Option<EditOutcome> = None;

    if let Some(edit) = curve_editor(
        ui,
        ("tone_curve", channel.label()),
        &display_points,
        display_mode,
        &channel_style(channel),
    ) {
        // Reset OR an edit that lands on identity → clear this channel.
        let new_points = if edit.reset || curve_math::is_identity(&edit.points) {
            Vec::new()
        } else {
            edit.points
        };
        let new_tc = with_channel(tc.clone(), channel, new_points, edit.mode);
        out = Some(EditOutcome {
            stack: set_tone_curve(stack, new_tc),
            kind: OpKind::ToneCurve,
            commit: edit.commit,
        });
    }

    // Parametric region sub-panel (Task 7). Takes precedence only when it emits.
    if let Some(param_out) = curve_widget_parametric::show(ui, stack, &tc) {
        out = Some(param_out);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_channel_has_a_distinct_tint() {
        let m = channel_style(Channel::Master).curve_color;
        let r = channel_style(Channel::Red).curve_color;
        let g = channel_style(Channel::Green).curve_color;
        let b = channel_style(Channel::Blue).curve_color;
        assert!(m != r && r != g && g != b && b != m, "channel tints must differ");
    }

    #[test]
    fn channel_label_is_short_and_stable() {
        assert_eq!(Channel::Master.label(), "Master");
        assert_eq!(Channel::Red.label(), "R");
        assert_eq!(Channel::Green.label(), "G");
        assert_eq!(Channel::Blue.label(), "B");
    }
}
```

- [ ] **Step 4: Add a stub parametric module so it compiles**

`show` references `curve_widget_parametric::show` (implemented in Task 7). Create a minimal identity stub now so Task 6 compiles and is independently testable. Create `ferrolite-app/src/develop/curve_widget_parametric.rs`:

```rust
//! Parametric region sub-panel for the Curve tab (Task 7 fills this in).
use crate::develop::adjustment_panel::EditOutcome;
use ferrolite_pipeline::{OpStack, ToneCurve};

/// Draw the parametric region controls. Task 6 stub: renders nothing, emits none.
pub fn show(_ui: &mut egui::Ui, _stack: &OpStack, _tc: &ToneCurve) -> Option<EditOutcome> {
    None
}
```

Register both edits in the develop module. In `ferrolite-app/src/develop/mod.rs` (or wherever the develop submodules are declared — grep for `mod curve_widget;`), add:

```rust
pub mod curve_widget_parametric;
```

- [ ] **Step 5: Run tests + clippy**

Run: `cargo test -p ferrolite-app curve_widget::tests`
Expected: PASS.

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/develop/curve_widget.rs ferrolite-app/src/develop/curve_widget_parametric.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(app): Curve tab channel selector (Master/R/G/B) with per-channel point curves"
```

---

### Task 7: Parametric sub-panel + read-only overlay

Fill in the parametric region controls: four region sliders (Highlights/Lights/Darks/Shadows) + three split sliders, each with the `EguiSlider` per-control reset, plus a small read-only plot of the baked parametric shape. Route edits through `set_tone_curve`.

**Files:**
- Modify: `ferrolite-app/src/develop/curve_widget_parametric.rs` (implement `show` + overlay)
- Test: `ferrolite-app/src/develop/curve_widget_parametric.rs` (pure helper test)

**Interfaces:**
- Consumes: `widgets::slider::EguiSlider`, `develop::ops_edit::set_tone_curve` (Task 5), `ferrolite_pipeline::{ParametricCurve, ToneCurve, OpKind, parametric_curve_lut}` (Tasks 1-3), `theme`.
- Produces: implemented `pub fn show(ui, stack, tc) -> Option<EditOutcome>`; helper `fn param_changed(&ParametricCurve, &ParametricCurve) -> bool` (pure, tested).

- [ ] **Step 1: Write the failing test (pure helper)**

Add a `#[cfg(test)] mod tests` in `ferrolite-app/src/develop/curve_widget_parametric.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::ParametricCurve;

    #[test]
    fn param_changed_detects_a_region_edit() {
        let a = ParametricCurve::default();
        let b = ParametricCurve { shadows: 0.3, ..Default::default() };
        assert!(param_changed(&a, &b));
        assert!(!param_changed(&a, &ParametricCurve::default()));
    }

    #[test]
    fn param_changed_detects_a_split_edit() {
        let a = ParametricCurve::default();
        let b = ParametricCurve { midtone_split: 0.6, ..Default::default() };
        assert!(param_changed(&a, &b));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p ferrolite-app curve_widget_parametric::tests`
Expected: FAIL to compile (`param_changed` undefined).

- [ ] **Step 3: Implement the parametric sub-panel**

Replace `ferrolite-app/src/develop/curve_widget_parametric.rs` with:

```rust
//! Parametric region sub-panel for the Curve tab: Highlights/Lights/Darks/
//! Shadows region sliders + three split sliders (each with the `EguiSlider`
//! per-control reset), plus a small read-only plot of the baked parametric
//! shape. Edits route through `ops_edit::set_tone_curve` (identity-eliding).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::ops_edit::set_tone_curve;
use crate::theme;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{parametric_curve_lut, OpKind, OpStack, ParametricCurve, ToneCurve};

const OVERLAY_W: f32 = 200.0;
const OVERLAY_H: f32 = 60.0;

/// True when any region value OR split point differs (drives the emit gate).
pub(crate) fn param_changed(a: &ParametricCurve, b: &ParametricCurve) -> bool {
    a != b
}

pub fn show(ui: &mut egui::Ui, stack: &OpStack, tc: &ToneCurve) -> Option<EditOutcome> {
    let mut p = tc.parametric;
    let before = p;

    ui.separator();
    ui.label(egui::RichText::new("Parametric").color(theme::TEXT_FAINT));

    // Read-only preview of the baked parametric shape.
    draw_overlay(ui, &p);

    let mut dragged = false;
    let mut drag_stopped = false;
    // ONE closure (not one per slider group) so `dragged`/`drag_stopped` are
    // borrowed mutably by a single closure — two closures each capturing them
    // would fail the borrow checker. The `EguiSlider` (owning its `&mut f32`) is
    // built at the call site and moved in.
    let mut add = |ui: &mut egui::Ui, s: EguiSlider| {
        let r = ui.add(s);
        if r.changed() {
            if r.drag_stopped() {
                drag_stopped = true;
            } else if r.dragged() {
                dragged = true;
            } else {
                drag_stopped = true; // click / typed / double-click-reset commits now
            }
        }
    };
    // Region sliders, light→dark (design §3.3 order). `EguiSlider` is built
    // inline: a helper returning one would have to borrow its `&mut f32` arg,
    // which closure lifetime inference can't express.
    add(ui, EguiSlider { label: "Highlights", value: &mut p.highlights, min: -1.0, max: 1.0, default: 0.0, step: 0.01, decimals: 2, unit: "", bipolar: true, signed: true });
    add(ui, EguiSlider { label: "Lights", value: &mut p.lights, min: -1.0, max: 1.0, default: 0.0, step: 0.01, decimals: 2, unit: "", bipolar: true, signed: true });
    add(ui, EguiSlider { label: "Darks", value: &mut p.darks, min: -1.0, max: 1.0, default: 0.0, step: 0.01, decimals: 2, unit: "", bipolar: true, signed: true });
    add(ui, EguiSlider { label: "Shadows", value: &mut p.shadows, min: -1.0, max: 1.0, default: 0.0, step: 0.01, decimals: 2, unit: "", bipolar: true, signed: true });
    // Split sliders (defaults 0.25 / 0.50 / 0.75).
    add(ui, EguiSlider { label: "Shadow split", value: &mut p.shadow_split, min: 0.0, max: 1.0, default: 0.25, step: 0.01, decimals: 2, unit: "", bipolar: false, signed: false });
    add(ui, EguiSlider { label: "Midtone split", value: &mut p.midtone_split, min: 0.0, max: 1.0, default: 0.50, step: 0.01, decimals: 2, unit: "", bipolar: false, signed: false });
    add(ui, EguiSlider { label: "Highlight split", value: &mut p.highlight_split, min: 0.0, max: 1.0, default: 0.75, step: 0.01, decimals: 2, unit: "", bipolar: false, signed: false });

    if !param_changed(&before, &p) {
        return None;
    }
    let new_tc = ToneCurve {
        parametric: p,
        ..tc.clone()
    };
    Some(EditOutcome {
        stack: set_tone_curve(stack, new_tc),
        kind: OpKind::ToneCurve,
        commit: drag_stopped || !dragged,
    })
}

/// Draw a small read-only plot of the baked parametric LUT (diagonal reference +
/// the parametric shape), so the region/split effect is visible at a glance.
fn draw_overlay(ui: &mut egui::Ui, p: &ParametricCurve) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(OVERLAY_W, OVERLAY_H),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::BG_BASE);
    // Identity reference diagonal.
    painter.line_segment(
        [
            egui::pos2(rect.left(), rect.bottom()),
            egui::pos2(rect.right(), rect.top()),
        ],
        egui::Stroke::new(1.0, theme::BORDER_STRONG),
    );
    // Baked parametric curve.
    let lut = parametric_curve_lut(p);
    let poly: Vec<egui::Pos2> = lut
        .iter()
        .enumerate()
        .map(|(i, &y)| {
            egui::pos2(
                rect.left() + (i as f32 / 255.0) * OVERLAY_W,
                rect.bottom() - y * OVERLAY_H,
            )
        })
        .collect();
    painter.add(egui::Shape::line(poly, egui::Stroke::new(1.5, theme::ACCENT)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::ParametricCurve;

    #[test]
    fn param_changed_detects_a_region_edit() {
        let a = ParametricCurve::default();
        let b = ParametricCurve { shadows: 0.3, ..Default::default() };
        assert!(param_changed(&a, &b));
        assert!(!param_changed(&a, &ParametricCurve::default()));
    }

    #[test]
    fn param_changed_detects_a_split_edit() {
        let a = ParametricCurve::default();
        let b = ParametricCurve { midtone_split: 0.6, ..Default::default() };
        assert!(param_changed(&a, &b));
    }
}
```

> Note: verify the `theme` constants used (`theme::BG_BASE`, `theme::BORDER_STRONG`, `theme::TEXT_FAINT`, `theme::ACCENT`) exist — they are all used by `widgets/curve.rs` already, so they do. If `EguiSlider`'s field set differs from what is shown (cross-check against the live struct in `ferrolite-app/src/widgets/slider.rs`), match the exact fields — the `LightTab`/`DetailTab` call sites in `base_tabs.rs` are the reference for the correct field list.

- [ ] **Step 4: Run tests + clippy + build**

Run: `cargo test -p ferrolite-app curve_widget_parametric::tests`
Expected: PASS.

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/develop/curve_widget_parametric.rs
git commit -m "feat(app): parametric region sub-panel + read-only overlay in Curve tab"
```

---

### Task 8: Workspace green gate + self-review

Final verification that the whole workspace passes the CLAUDE.md gate, and a documentation sweep.

**Files:** none (verification) — plus any small fixups the gate surfaces.

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then: `cargo fmt --all --check`
Expected: no diff.

- [ ] **Step 2: Clippy (workspace, all targets, warnings-as-errors)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean. (Watch for a now-unused `curve_lut` import in `pipeline.rs`/`tile_edit.rs` if it is no longer referenced — remove if flagged.)

- [ ] **Step 3: Full test suite**

Run: `cargo test --workspace`
Expected: PASS. On this dev machine the GPU goldens run and must be green (existing `tone_curve*.png` + the new `tone_curve_p3.png`); on headless CI they skip.

- [ ] **Step 4: Self-review against the spec**

Confirm each design §3 requirement maps to a task: op model (§3.1 → T1), bake `parametric_curve_lut` + composite finals (§3.2 → T2,T3), per-channel GPU LUTs (§3.2 → T4), channel selector + parametric sub-panel + per-control reset (§3.3 → T6,T7), serde back-compat + per-channel composite + golden (§3.4 → T1,T3,T4). Confirm the reusable-fn constraint (§2.5): `parametric_curve_lut` + `tone_curve_luts` are public and pure. Confirm no `OpKind` renumber happened and the guard test is intact.

- [ ] **Step 5: Commit any gate fixups**

```bash
git add -A
git commit -m "chore(p3-tone-curves): workspace gate green (fmt/clippy/test)"
```

---

## Visual test plan (hand to the author after the gate is green — per CLAUDE.md)

This branch changes reachable app UI (the Develop **Curve** tab) and the tone-curve GPU pass, so hands-on testing **is** required. Open a RAW/image in Develop and go to the **Curve** tab.

1. **Channel selector present & tinted.** Look for a `Master / R / G / B` row above the curve. Selecting each switches the curve area to that channel; the curve line is tinted (Master = accent blue-grey, R = red, G = green, B = blue). *Failure:* no selector, missing channel, or all channels share one color.
2. **Per-channel point curves are independent.** On **R**, drag the midtone down — reds darken in the image only (greens/blues unchanged). Switch to **G**, drag up — greens brighten; the R edit persists when you switch back. *Failure:* editing one channel moves another, or switching channels loses the previous channel's shape.
3. **Master curve still works & matches old behavior.** On **Master**, pull the midtone down — the whole image darkens as before. Toggle Linear/Smooth; the shape changes accordingly. *Failure:* master edit looks different from pre-P3, or the Linear/Smooth toggle does nothing.
4. **Parametric sub-panel.** Below the curve, find **Parametric** with Highlights/Lights/Darks/Shadows + Shadow/Midtone/Highlight split sliders and a small read-only preview plot. Raise **Shadows** — only dark tones lift (highlights unaffected) and the preview plot bulges up on the left. Raise **Highlights** — only brights lift, plot bulges on the right. Drag **Midtone split** — the transition point visibly shifts. *Failure:* a region affects the wrong tones, the preview plot doesn't track the sliders, or the plot is non-monotone (dips).
5. **Per-control reset on everything.** Each parametric slider has its own reset affordance (the `EguiSlider` reset column) that returns just that slider to its default (regions→0, splits→0.25/0.50/0.75) without touching neighbors. The curve editor's **Reset** clears just the active channel; the mode-selector reset returns mode to Smooth. *Failure:* a reset is missing, or resetting one control disturbs another.
6. **Identity elision.** Reset every channel + every parametric slider to default → the curve op should drop out (the image returns exactly to its pre-curve state; no residual tint). *Failure:* a fully-reset curve still alters the image.
7. **Responsiveness / no freeze (CLAUDE.md rule 1/2).** Drag any curve point or parametric slider quickly — the preview updates smoothly with no multi-second stall on the first curve edit or on channel switching (pipeline is built once; only LUT bytes upload). *Failure:* a visible freeze/hitch on first edit or channel switch.
8. **Persistence round-trip.** Make a per-channel + parametric edit, close & reopen the image (sidecar reload) — the exact curve returns. Then confirm an image edited **before** this branch still opens unchanged (no curve applied where none existed). *Failure:* edit not restored, or a pre-P3 image gains an unexpected curve.

**Fixtures:** any RAW or image works; a colorful subject (skin tones + sky) makes the per-channel R/G/B effect and shadows/highlights split easiest to judge.

**Offline artifact to glance at (optional):** `ferrolite-pipeline/tests/fixtures/tone_curve_p3.png` — the committed golden for the combined per-channel + parametric edit on the gradient.
