# P2 Plan 2 — Live WB-driven matrix wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the `ColorMatrixNode` camera→working matrix follow the WhiteBalance temperature — dragging WB temp on a dual-illuminant RAW re-interpolates the dual-illuminant matrix live (Lightroom's model), pushed as a uniform with `mark_dirty` while the pipeline stays built-once.

**Architecture:** The interpolation stays in the **app layer** (it owns the `ColorProfile`, working space, and op stack; `ferrolite-pipeline` receives a pre-composed 3×3, as today). A pure `wb_temp_to_cct` helper (ferrolite-color) maps the WB op's normalized `temp ∈ [-1,1]` to an absolute CCT anchored at D65, linear in mired. A pure `wb_camera_to_working` free function (app) turns `(ColorProfile, temp, working_space)` into the interpolated, row-normalized matrix via Plan 1's `camera_to_working_interpolated`. The app's `camera_to_working` becomes temp-aware and pushes the recomputed matrix through the existing `set_color_matrix` (which already does `mark_dirty`) on every edit path — so a WB temp change re-runs the chain from the head with no rebuild.

**Tech Stack:** Rust, `cargo` workspace. Crates touched: `ferrolite-color` (pure helper), `ferrolite-pipeline` (GPU golden tests only), `ferrolite-app` (pure helper + glue). No new dependencies. Depends on **Plan 1** (dual-illuminant `ColorProfile.calibrations` + `camera_to_working_interpolated` + CCT↔xy), already merged to `main`.

## Global Constraints

- **Pipeline built once (CLAUDE.md §2).** A WB temp change updates ONLY the `ColorMatrixNode` uniform (`set_color_matrix` → `mark_dirty`); never rebuild pipelines/shaders. Pre-warm/caching unchanged.
- **Never block the UI thread (CLAUDE.md §1).** The recompute is a cheap 3×3 interpolation on the UI thread (microseconds); no I/O, no decode. Heavy tiles still run as jobs, unchanged.
- **No new persisted state (contract §2 / S5).** The `WhiteBalance` op keeps its schema (`temp`/`tint ∈ [-1,1]`, 0 = identity) and its neutral-shifting working-space multiplier. Nothing new is serialized.
- **Additive / no new controls (spec §2).** No new UI, sliders, or persisted fields → per-component-reset, keybind-tooltip, keybind-discoverability, and icon rules are N/A (no new controls added).
- **Photo tier only.** `ferrolite-color`, `ferrolite-pipeline`, `ferrolite-app`. No engine-tier changes; the generic GPU executor and VT are untouched (contracts §4/§5).
- **Gate (per branch):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → **then STOP and hold for the author's (Jann's) visual test** (CLAUDE.md "Finishing a branch"). This plan HAS a visual test (below).

## Resolved open questions (spec §8, resolved at plan-write time)

- **WB-op reconciliation (how much of the WB op changes vs stays):**
  - **Stays:** The `WhiteBalance { temp, tint }` op is unchanged — same `[-1,1]` normalized range, same `wb_uniform` working-space multiplier (`white_balance.wgsl`) that produces the actual neutral white-balance shift. No schema/sidecar/persist change.
  - **Changes (adds):** The `temp` value now *also* drives the camera→working matrix. `temp` maps to an absolute **scene-CCT estimate** anchored at **D65 (temp 0 → 6504 K)**, linear in **mired** (DNG-native, perceptually even); warm (`temp>0`) → higher mired → lower Kelvin, reaching ≈ Standard-A at `temp≈+1`. On a **dual-illuminant** RAW the matrix re-interpolates with `temp` (S3); on a **single-illuminant / fallback** profile `camera_to_working_interpolated` reduces to the static matrix, so `temp` has no matrix effect and only the WB uniform shifts (correct — no dual data to blend).
  - **As-shot baseline:** D65 is the deterministic temp-0 anchor (no extra metadata needed; the RAW demosaic already applied as-shot neutral gains and `normalize_neutral` keeps neutrals neutral). A truer as-shot-CCT-from-`wb_coeffs` derivation is a possible later refinement, out of scope here.
- **CCT↔xy method / interpolation domain:** already resolved in Plan 1 (Kim locus for `cct_to_xy`, McCamy for `xy_to_cct`; linear blend of `xyz_to_cam` elements weighted by inverse-CCT/mired). Reused unchanged.
- **Out of scope (Plan 3+):** unclamp, RCD (CPU/GPU), CFA-as-GPU-source, halo/tiling. Not designed for here.

---

## File Structure

- `ferrolite-color/src/cct.rs` **(modify)** — add `wb_temp_to_cct(temp_norm) -> f32` beside the existing CCT helpers (same responsibility: colour-temperature math).
- `ferrolite-color/src/lib.rs` **(modify)** — re-export `wb_temp_to_cct`.
- `ferrolite-pipeline/tests/color_golden.rs` **(modify)** — add two GPU goldens: interpolated matrix flows through `ColorMatrixNode` (§10), and a live `set_color_matrix` re-push changes output (the recompute+dirty mechanic). Auto-skip headless.
- `ferrolite-app/src/camera_matrix.rs` **(new)** — pure `wb_camera_to_working(profile, temp, working) -> [[f32;3];3]` (interpolate + `normalize_neutral`) + unit tests. One responsibility: profile→working matrix for the current WB temp.
- `ferrolite-app/src/main.rs` **(modify)** — declare `mod camera_matrix;`.
- `ferrolite-app/src/app.rs` **(modify)** — `current_wb_temp()` helper; `camera_to_working(&self, temp)`; update the 6 call sites; push the recomputed matrix on the two `set_preview_and_full` reuse paths.

---

## Task 1: `wb_temp_to_cct` — WB temp → scene CCT (`ferrolite-color`)

**Files:**
- Modify: `ferrolite-color/src/cct.rs`
- Modify: `ferrolite-color/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `ferrolite-color/src/cct.rs`

**Interfaces:**
- Consumes: nothing new (pure arithmetic).
- Produces: `pub fn wb_temp_to_cct(temp_norm: f32) -> f32` — normalized WB temp (`[-1,1]`, warm positive, 0 = D65 baseline) → absolute CCT in Kelvin, clamped to the Kim-locus valid range `[1667, 25000]`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `ferrolite-color/src/cct.rs` (after the existing tests):

```rust
    #[test]
    fn wb_temp_zero_is_d65() {
        assert!((wb_temp_to_cct(0.0) - 6504.0).abs() < 1.0, "{}", wb_temp_to_cct(0.0));
    }

    #[test]
    fn wb_temp_warm_lowers_cct_cool_raises_it() {
        // Warm (positive) is a lower colour temperature than neutral; cool higher.
        assert!(wb_temp_to_cct(0.5) < wb_temp_to_cct(0.0));
        assert!(wb_temp_to_cct(-0.5) > wb_temp_to_cct(0.0));
    }

    #[test]
    fn wb_temp_is_monotonic_nonincreasing_and_strict_in_warm_range() {
        // Non-increasing across the full slider. The extreme-cool end saturates
        // at the Kim-locus clamp (temp ≲ -0.57 → 25000 K), which is harmless:
        // the dual matrix is already pinned to the D65 endpoint for ALL cool
        // temps (interpolation weight = 0), so the cool-side CCT value never
        // affects the matrix — only the (unchanged) WB uniform shifts neutrals.
        let mut prev = f32::INFINITY;
        for i in -10..=10 {
            let t = i as f32 / 10.0;
            let cct = wb_temp_to_cct(t);
            assert!(cct <= prev + 1e-3, "not non-increasing at t={t}: {cct} > {prev}");
            prev = cct;
        }
        // Strictly decreasing across the unclamped warm/interior range [-0.5, 1.0].
        let mut prev = f32::INFINITY;
        for i in -5..=10 {
            let t = i as f32 / 10.0;
            let cct = wb_temp_to_cct(t);
            assert!(cct < prev, "not strictly decreasing at t={t}: {cct} !< {prev}");
            prev = cct;
        }
    }

    #[test]
    fn wb_temp_plus_one_is_near_standard_a() {
        // +1 reaches roughly Standard illuminant A (2856 K).
        assert!((wb_temp_to_cct(1.0) - 2856.0).abs() < 200.0, "{}", wb_temp_to_cct(1.0));
    }

    #[test]
    fn wb_temp_clamps_finite_beyond_range() {
        for &t in &[-5.0_f32, 5.0] {
            let cct = wb_temp_to_cct(t);
            assert!(cct.is_finite() && (1667.0..=25000.0).contains(&cct), "t={t} -> {cct}");
        }
    }
```

Add the re-export to `ferrolite-color/src/lib.rs` — change:

```rust
pub use cct::{cct_to_xy, xy_to_cct};
```
to:
```rust
pub use cct::{cct_to_xy, wb_temp_to_cct, xy_to_cct};
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-color cct::tests::wb_temp`
Expected: FAIL — `cannot find function wb_temp_to_cct in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `ferrolite-color/src/cct.rs`, directly below `xy_to_cct` (above the `#[cfg(test)]` module):

```rust
/// Map the `WhiteBalance` op's normalized temperature (`[-1, 1]`, warm positive,
/// 0 = D65 baseline) to an absolute correlated colour temperature (Kelvin), for
/// driving dual-illuminant matrix interpolation (P2 §5.1 / §8).
///
/// Anchored at D65 (temp 0 → 6504 K) and linear in **mired** (reciprocal
/// megakelvin) — the perceptually even, DNG-native domain — so equal slider
/// steps are equal perceived colour-temperature steps. Warm (`temp > 0`) raises
/// mired → lowers Kelvin; `TEMP_MIRED_SPAN` sets how far ±1 reaches (≈ Standard-A
/// at +1). Clamped to the Kim-locus valid range so downstream `cct_to_xy` stays
/// finite.
pub fn wb_temp_to_cct(temp_norm: f32) -> f32 {
    const D65_CCT: f32 = 6504.0;
    const TEMP_MIRED_SPAN: f32 = 200.0; // mired per unit of normalized temp
    let baseline_mired = 1.0e6 / D65_CCT;
    // mired ∈ [40, 600] ⇒ CCT ∈ [1667, 25000] (Kim-locus valid range).
    let mired = (baseline_mired + temp_norm * TEMP_MIRED_SPAN).clamp(40.0, 600.0);
    1.0e6 / mired
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-color cct::tests::wb_temp`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-color/src/cct.rs ferrolite-color/src/lib.rs
git commit -m "feat(color): wb_temp_to_cct — normalized WB temp to scene CCT (mired, D65-anchored)"
```

---

## Task 2: GPU goldens — interpolated matrix through the node + live re-push (`ferrolite-pipeline`)

**Files:**
- Modify: `ferrolite-pipeline/tests/color_golden.rs`
- Test: the two new `#[test]` functions themselves (integration test, auto-skips headless).

**Interfaces:**
- Consumes: `ferrolite_color::{camera_to_working_interpolated, Xy}` (dev-dependency, already in `Cargo.toml`), `EditPipeline`, `OpStack`, the existing `probe_image` / `srgb_oetf` / `TOL` helpers in the file.
- Produces: nothing consumed by later tasks (pure verification).

- [ ] **Step 1: Write the failing tests**

Append to `ferrolite-pipeline/tests/color_golden.rs`:

```rust
/// §10 GPU golden: a dual-illuminant matrix INTERPOLATED at a fixed CCT (via
/// `ferrolite_color::camera_to_working_interpolated`) must flow through the
/// `ColorMatrixNode` and match the same matrix applied on the CPU (+ sRGB OETF).
/// Proves Plan 1's interpolation result reaches the GPU head unchanged.
#[test]
fn interpolated_matrix_flows_through_color_matrix_node() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    use ferrolite_color::Xy;
    // Two distinct fake calibrations (Standard-A-ish and D65-ish white points).
    let a_white = Xy { x: 0.4476, y: 0.4074 };
    let d65_white = Xy { x: 0.3128, y: 0.3290 };
    let m_a = [[1.0, 0.1, 0.0], [0.2, 1.0, 0.1], [0.0, 0.2, 1.0]];
    let m_d65 = [[1.2, -0.1, 0.0], [-0.05, 1.1, -0.05], [0.0, -0.1, 1.3]];
    let cals = [(a_white, m_a), (d65_white, m_d65)];
    let m = ferrolite_color::camera_to_working_interpolated(
        &cals,
        4000.0,
        ferrolite_color::WorkingSpace::Rec2020,
    );

    let img = probe_image();
    let mut ep = EditPipeline::new(std::sync::Arc::new(ctx), &img, OpStack::default(), m);
    let out = ep.render_to_image();

    for i in 0..4usize {
        let (r, g, b) = (img.pixels[i * 4], img.pixels[i * 4 + 1], img.pixels[i * 4 + 2]);
        // Expected linear = m · [r,g,b] (row-major), matching the shader.
        let lin = [
            m[0][0] * r + m[0][1] * g + m[0][2] * b,
            m[1][0] * r + m[1][1] * g + m[1][2] * b,
            m[2][0] * r + m[2][1] * g + m[2][2] * b,
        ];
        for c in 0..3 {
            let want = (srgb_oetf(lin[c].clamp(0.0, 1.0)) * 255.0).round() as i32;
            let got = out[i * 4 + c] as i32;
            assert!((want - got).abs() <= TOL as i32, "texel {i} ch {c}: want {want} got {got}");
        }
    }
}

/// The Plan 2 mechanic: re-pushing a new matrix via `set_color_matrix` (the same
/// call the app makes on a WB temp change) updates the output live — no rebuild.
#[test]
fn set_color_matrix_repush_changes_output_live() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let img = probe_image();
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let mut ep = EditPipeline::new(std::sync::Arc::new(ctx), &img, OpStack::default(), identity);
    let before = ep.render_to_image();

    // Channel-swap matrix: out.r = b, out.g = r, out.b = g — visibly different.
    let swap = [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    ep.set_color_matrix(swap);
    let after = ep.render_to_image();

    assert_ne!(before, after, "re-pushing the color matrix must change the output");
    // Spot-check: after-swap red channel == before green OETF-wise (r_out = b_in path
    // is hard to compare directly; assert the swapped output matches a CPU apply).
    for i in 0..4usize {
        let (r, g, b) = (img.pixels[i * 4], img.pixels[i * 4 + 1], img.pixels[i * 4 + 2]);
        let lin = [b, r, g];
        for c in 0..3 {
            let want = (srgb_oetf(lin[c].clamp(0.0, 1.0)) * 255.0).round() as i32;
            let got = after[i * 4 + c] as i32;
            assert!((want - got).abs() <= TOL as i32, "texel {i} ch {c}: want {want} got {got}");
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail (or skip cleanly headless)**

Run: `cargo test -p ferrolite-pipeline --test color_golden interpolated_matrix_flows_through_color_matrix_node set_color_matrix_repush_changes_output_live`
Expected on a GPU dev box: FAIL to COMPILE first (they reference the new tests) → once compiling, they must PASS if the plumbing is already correct; if a golden FAILS, that is a real defect to fix in the shader/uniform path, not the test. On headless CI: both print "no GPU adapter; skipping" and pass trivially.

Note: these tests exercise ALREADY-shipped code (`set_color_matrix`, `ColorMatrixNode`, Plan 1's `camera_to_working_interpolated`) — there is no new production code in this task, so they are expected to pass immediately on a GPU box. Their value is locking the Plan 2 contract (interpolated matrix reaches the node; re-push is live) against regression. Confirm they pass on the dev GPU before committing.

- [ ] **Step 3: (No implementation needed)**

This task adds only tests over existing behavior. If a golden fails on the dev GPU, debug the real cause (uniform packing, shader) before proceeding.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --test color_golden`
Expected: PASS on a GPU box (4 tests: the 2 existing + 2 new); "skipping" lines on headless.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/tests/color_golden.rs
git commit -m "test(pipeline): GPU goldens for interpolated matrix through ColorMatrixNode + live re-push"
```

---

## Task 3: `wb_camera_to_working` — profile → working matrix for the current WB temp (`ferrolite-app`)

**Files:**
- Create: `ferrolite-app/src/camera_matrix.rs`
- Modify: `ferrolite-app/src/main.rs`
- Test: inline `#[cfg(test)] mod tests` in `ferrolite-app/src/camera_matrix.rs`

**Interfaces:**
- Consumes: `ferrolite_color::{camera_to_working, camera_to_working_interpolated, normalize_neutral, wb_temp_to_cct, mul_vec3, Mat3, Xy, WorkingSpace}` (Task 1 + Plan 1), `ferrolite_decode::{CameraCalibration, ColorProfile}` (Plan 1).
- Produces: `pub fn wb_camera_to_working(profile: &ColorProfile, temp: f32, working: WorkingSpace) -> [[f32; 3]; 3]` — the row-normalized camera→working matrix for the given normalized WB temp. Dual-illuminant → re-interpolated by temp; single/fallback → temp-independent (reduces to today's `normalize_neutral(camera_to_working(...))`).

- [ ] **Step 1: Write the failing tests**

Create `ferrolite-app/src/camera_matrix.rs`:

```rust
//! Camera→working colour matrix for the current white-balance temperature.
//!
//! P2 Plan 2 (S3): a dual-illuminant `ColorProfile` re-interpolates its
//! camera→working matrix as the WhiteBalance temp changes (Lightroom's model),
//! anchored at D65 and linear in mired. Single-illuminant / fallback profiles
//! reduce to the static matrix (temp only drives the WB uniform, not the
//! matrix). Row-normalized because the RAW demosaic already applied the as-shot
//! neutral gains (see `ferrolite_color::normalize_neutral`).

use ferrolite_color::{
    camera_to_working_interpolated, normalize_neutral, wb_temp_to_cct, Mat3, WorkingSpace, Xy,
};
use ferrolite_decode::ColorProfile;

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_color::{camera_to_working, mul_vec3};
    use ferrolite_decode::CameraCalibration;

    const M_A: Mat3 = [[1.0, 0.1, 0.0], [0.2, 1.0, 0.1], [0.0, 0.2, 1.0]];
    const M_D65: Mat3 = [[1.2, -0.1, 0.0], [-0.05, 1.1, -0.05], [0.0, -0.1, 1.3]];
    const A_WHITE: [f32; 2] = [0.4476, 0.4074];
    const D65_WHITE: [f32; 2] = [0.3128, 0.3290];

    fn dual_profile() -> ColorProfile {
        ColorProfile {
            xyz_to_cam: M_D65,
            white_xy: D65_WHITE,
            is_fallback: false,
            calibrations: vec![
                CameraCalibration { xyz_to_cam: M_A, white_xy: A_WHITE },
                CameraCalibration { xyz_to_cam: M_D65, white_xy: D65_WHITE },
            ],
        }
    }

    fn single_profile() -> ColorProfile {
        ColorProfile {
            xyz_to_cam: M_D65,
            white_xy: D65_WHITE,
            is_fallback: false,
            calibrations: vec![CameraCalibration { xyz_to_cam: M_D65, white_xy: D65_WHITE }],
        }
    }

    fn approx_eq(a: &Mat3, b: &Mat3, tol: f32) -> bool {
        (0..3).all(|i| (0..3).all(|j| (a[i][j] - b[i][j]).abs() <= tol))
    }

    #[test]
    fn dual_illuminant_matrix_tracks_temp() {
        let warm = wb_camera_to_working(&dual_profile(), 0.8, WorkingSpace::Rec2020);
        let cool = wb_camera_to_working(&dual_profile(), -0.8, WorkingSpace::Rec2020);
        assert!(!approx_eq(&warm, &cool, 1e-4), "matrix must change with WB temp");
    }

    #[test]
    fn single_illuminant_matrix_is_temp_independent() {
        let a = wb_camera_to_working(&single_profile(), 0.8, WorkingSpace::Rec2020);
        let b = wb_camera_to_working(&single_profile(), -0.8, WorkingSpace::Rec2020);
        assert!(approx_eq(&a, &b, 1e-6), "single calibration: temp has no matrix effect");
    }

    #[test]
    fn single_illuminant_equals_legacy_normalize_neutral_path() {
        // Reduces to today's behaviour: normalize_neutral(camera_to_working(...)).
        let got = wb_camera_to_working(&single_profile(), 0.3, WorkingSpace::Rec2020);
        let want = normalize_neutral(camera_to_working(
            M_D65,
            Xy { x: D65_WHITE[0], y: D65_WHITE[1] },
            WorkingSpace::Rec2020,
        ));
        assert!(approx_eq(&got, &want, 1e-6), "got {got:?} want {want:?}");
    }

    #[test]
    fn neutral_stays_neutral_for_any_temp() {
        for &t in &[-1.0_f32, 0.0, 0.7] {
            let m = wb_camera_to_working(&dual_profile(), t, WorkingSpace::Rec2020);
            let out = mul_vec3(&m, &[1.0, 1.0, 1.0]);
            assert!(
                (0..3).all(|i| (out[i] - 1.0).abs() < 1e-4),
                "temp {t}: neutral skewed to {out:?}"
            );
        }
    }

    #[test]
    fn fallback_profile_is_finite() {
        let m = wb_camera_to_working(&ColorProfile::srgb_fallback(), 0.5, WorkingSpace::Rec2020);
        assert!(m.iter().flatten().all(|v: &f32| v.is_finite()));
    }
}
```

Declare the module in `ferrolite-app/src/main.rs` — after `mod canvas;` insert:

```rust
mod camera_matrix;
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-app camera_matrix`
Expected: FAIL — `cannot find function wb_camera_to_working in this scope`.

- [ ] **Step 3: Write minimal implementation**

Insert into `ferrolite-app/src/camera_matrix.rs`, between the `use` block and the `#[cfg(test)]` module:

```rust
/// Camera→working 3×3 for `profile` at the normalized WhiteBalance `temp`,
/// row-normalized. Dual-illuminant profiles re-interpolate with `temp` (S3);
/// single-illuminant / fallback profiles are temp-independent (reduce to the
/// static camera→working matrix — the WB uniform still shifts neutrals).
pub fn wb_camera_to_working(profile: &ColorProfile, temp: f32, working: WorkingSpace) -> Mat3 {
    let calibrations: Vec<(Xy, Mat3)> = profile
        .calibrations
        .iter()
        .map(|c| {
            (
                Xy { x: c.white_xy[0], y: c.white_xy[1] },
                c.xyz_to_cam,
            )
        })
        .collect();
    let target_cct = wb_temp_to_cct(temp);
    let m = camera_to_working_interpolated(&calibrations, target_cct, working);
    normalize_neutral(m)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-app camera_matrix`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/camera_matrix.rs ferrolite-app/src/main.rs
git commit -m "feat(app): wb_camera_to_working — temp-driven interpolated camera->working matrix"
```

---

## Task 4: Wire the WB-driven matrix into the app render paths (`ferrolite-app`)

**Files:**
- Modify: `ferrolite-app/src/app.rs` (the `camera_to_working` method + 6 call sites + 2 reuse-path matrix pushes)

**Interfaces:**
- Consumes: `crate::camera_matrix::wb_camera_to_working` (Task 3), `OpStack::white_balance()` (existing), `ViewerState::preview_tier_source` (existing), `EditPipeline::set_color_matrix` / `EditTileProducer::set_color_matrix` (existing).
- Produces: no new public API — behavioral change: a WB temp edit re-interpolates and pushes the camera→working matrix to both tiers.

> **TDD note:** This task is UI-integration glue over already-unit-tested pure logic (Tasks 1 & 3) and already-golden-tested GPU behavior (Task 2). It is not unit-testable in isolation (it wires `&mut self` app state to the GPU pipelines). Per CLAUDE.md, its correctness is confirmed by `cargo build` + the workspace gate + the **author's visual test** (Task 5). Every edit below is shown in full — no placeholders.

- [ ] **Step 1: Replace the `camera_to_working` method and add `current_wb_temp`**

In `ferrolite-app/src/app.rs`, replace the whole method (currently at ~439-444):

```rust
    fn camera_to_working(&self) -> [[f32; 3]; 3] {
        match self.state.viewer.as_ref() {
            Some(v) => ferrolite_color::normalize_neutral(self.source_to_working(&v.color_profile)),
            None => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }
```

with:

```rust
    /// Normalized WhiteBalance temperature of the open viewer's current op stack
    /// (0.0 = as-shot/identity when there is no WB op or no viewer).
    fn current_wb_temp(&self) -> f32 {
        self.state
            .viewer
            .as_ref()
            .and_then(|v| v.op_stack.white_balance())
            .map(|w| w.temp)
            .unwrap_or(0.0)
    }

    /// camera→working for the open viewer's RAW profile at the given normalized WB
    /// `temp` (full-res tier). Dual-illuminant profiles re-interpolate with `temp`
    /// (P2 Plan 2 / S3); single-illuminant reduce to the static matrix. Already
    /// row-normalized by `wb_camera_to_working` (the demosaic applied as-shot
    /// gains). The sRGB preview tier is NOT normalized — see `preview_to_working`.
    fn camera_to_working(&self, temp: f32) -> [[f32; 3]; 3] {
        match self.state.viewer.as_ref() {
            Some(v) => crate::camera_matrix::wb_camera_to_working(
                &v.color_profile,
                temp,
                self.state.working_space,
            ),
            None => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }
```

(`source_to_working` stays as-is — it remains the un-normalized single-matrix path used only by `preview_to_working` for sRGB sources.)

- [ ] **Step 2: Update the five "current-stack" call sites**

Each of these recomputes the matrix for the currently-open image, so each passes `self.current_wb_temp()`.

Site A (~540):
```rust
        let camera_to_working = self.camera_to_working();
```
→
```rust
        let camera_to_working = self.camera_to_working(self.current_wb_temp());
```

Site B (~751, inside the `is_raw` before-view branch):
```rust
            let cam = self.camera_to_working();
```
→
```rust
            let cam = self.camera_to_working(self.current_wb_temp());
```

Site C (~819, in `apply_full_decoded`):
```rust
        let cam = self.camera_to_working();
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
```
→
```rust
        let cam = self.camera_to_working(self.current_wb_temp());
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
```

Site D (~1185):
```rust
        let cam = self.camera_to_working();
        let Some(v) = self.state.viewer.as_mut() else {
```
→
```rust
        let cam = self.camera_to_working(self.current_wb_temp());
        let Some(v) = self.state.viewer.as_mut() else {
```

Site E (~1887, working-space-change handler):
```rust
        let cam = self.camera_to_working();
        let pw = self.preview_to_working();
        let Some(v) = self.state.viewer.as_mut() else {
            ctx.request_repaint();
            return;
        };
```
→
```rust
        let cam = self.camera_to_working(self.current_wb_temp());
        let pw = self.preview_to_working();
        let Some(v) = self.state.viewer.as_mut() else {
            ctx.request_repaint();
            return;
        };
```

- [ ] **Step 3: Update `set_preview_and_full` to use the NEW stack's temp and push the matrix on both reuse paths**

In `set_preview_and_full` (~1279), the sixth call site must use the temp of the **incoming** `stack` (not the still-old `v.op_stack`). Replace (~1286-1287):

```rust
        let cam = self.camera_to_working();
        let pw = self.preview_to_working();
```
→
```rust
        // WB temp of the INCOMING stack (v.op_stack is updated below), so a WB
        // temp edit re-interpolates the dual-illuminant matrix this same frame.
        let temp = stack.white_balance().map(|w| w.temp).unwrap_or(0.0);
        let cam = self.camera_to_working(temp);
        let pw = self.preview_to_working();
```

Immediately after `v.opstack_version = v.opstack_version.wrapping_add(1);` (~1293), add the kind-correct preview matrix computation (RAW = `cam`, Standard = `pw`):

```rust

        // Preview-tier matrix (RAW = WB-driven camera→working `cam`; Standard =
        // sRGB `pw`). Recomputed each edit; `set_color_matrix` no-ops when
        // unchanged, so only a WB temp change actually dirties the head (P2 §5.1).
        let pv_matrix = v.preview_tier_source(cam, pw).1;
```

In the preview reuse block, after `ep.set_stack(shown.clone());` (~1347), add:

```rust
            ep.set_color_matrix(pv_matrix);
```

In the full-res color-only reuse block, after `producer.set_stack(shown.clone());` (~1426), add:

```rust
                producer.set_color_matrix(cam);
```

- [ ] **Step 4: Verify it compiles and there are no stale no-arg calls**

Run: `cargo build -p ferrolite-app 2>&1 | tail -20`
Expected: builds clean. Then confirm no `camera_to_working()` no-arg calls remain:

Run: `grep -rn 'self\.camera_to_working()' ferrolite-app/src/`
Expected: no matches (all six now pass a temp argument).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/app.rs
git commit -m "feat(app): WhiteBalance temp drives the ColorMatrixNode (live re-interpolation on edit)"
```

---

## Task 5: Workspace green gate + author visual-test handoff

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all && cargo fmt --check`
Expected: no diff.

- [ ] **Step 2: Clippy (warnings as errors)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings/errors.

- [ ] **Step 3: Full workspace test**

Run: `cargo test --workspace`
Expected: PASS (new `wb_temp_to_cct`, `camera_matrix`, and — on a GPU box — the two new color goldens; headless skips the GPU goldens).

- [ ] **Step 4: Commit any formatting fixups**

```bash
git add -A && git commit -m "chore: cargo fmt for P2 plan 2" || echo "nothing to format"
```

- [ ] **Step 5: STOP — hand the author the visual test**

Per CLAUDE.md "Finishing a branch", the automated gate is necessary but not sufficient. Present the visual test plan and **hold** for Jann's hands-on results before merging/PR-ing:

**Visual test (real surface this plan):**
1. **Open a dual-illuminant RAW** (a camera whose DNG profile has two CalibrationIlluminants — most modern Canon/Nikon/Sony RAWs; a single-illuminant or non-RAW file is a negative control). Go to Develop.
2. **Drag the White Balance _Temp_ slider warm (right) then cool (left)** on a scene with **saturated, non-neutral colours** (skin, foliage, a colour chart).
   - **Expected:** as Temp changes, the overall balance shifts warmer/cooler (unchanged behaviour, from the WB uniform) **and** saturated colours re-render subtly as the camera matrix re-interpolates toward the tungsten (warm) / daylight (cool) calibration — hue/saturation of coloured regions tracks the temperature. Neutrals stay neutral at every temp.
   - **Failure signatures:** colour does NOT change beyond a flat global tint (matrix not tracking); neutrals skew (double white balance / bad normalize); a **freeze or stutter** while dragging (a rebuild leaked in — must be uniform-only); the image flickers to the wrong colour then back (matrix pushed from the wrong/old stack).
3. **Confirm responsiveness:** the temp drag stays smooth at fit zoom and at 1:1 (no per-frame pipeline rebuild — CLAUDE.md §2). Full-res tiles re-produce with the new matrix after the drag settles.
4. **Negative control:** open a **single-illuminant RAW or a JPEG** and drag Temp — the global warm/cool shift still works (WB uniform), but there is no extra matrix re-render (expected: single calibration → temp-independent matrix).

Do NOT merge/PR until Jann confirms the color tracks correctly and there is no freeze. Address any issue found, then re-run the gate before finishing.

---

## Self-Review

**Spec coverage (§7 Plan 2 + §5.1 + §8 + §10):**
- "ColorMatrixNode uniform recomputed from the current WhiteBalance temperature (mark_dirty + uniform push; pipeline built once)" → Task 4 (recompute `cam` from `temp`, push via `set_color_matrix` which `mark_dirty`s; no rebuild). Mechanic golden in Task 2 (`set_color_matrix_repush_changes_output_live`).
- "reconcile WB temp as scene-CCT (§8 WB-op reconciliation)" → Resolved-questions section + Task 1 (`wb_temp_to_cct`, D65-anchored, mired-linear) + Task 3 (WB op unchanged; temp additionally drives the matrix).
- "interpolated-matrix GPU golden (§10)" → Task 2 (`interpolated_matrix_flows_through_color_matrix_node`).
- "single-illuminant reduces to today's path" → Task 3 tests (`single_illuminant_matrix_is_temp_independent`, `single_illuminant_equals_legacy_normalize_neutral_path`).
- "no new persisted state / no new controls" → nothing serialized; `WhiteBalance` schema untouched; no UI added (Global Constraints).
- "pipeline built once / no UI-thread block" → Task 4 uses only `set_color_matrix` (uniform + dirty); visual test step 3 checks no freeze.
- Depends-on Plan 1 (merged) → uses `ColorProfile.calibrations`, `CameraCalibration`, `camera_to_working_interpolated`, `wb_temp_to_cct` builds on `cct_to_xy`/`xy_to_cct`. ✓

**Placeholder scan:** No TBD/TODO/"handle edge cases"; every code step shows complete code; every run step states expected output. The one non-TDD task (Task 4) is explicitly justified (UI glue) with full before/after code for every edit. ✓

**Type consistency:** `wb_temp_to_cct(f32) -> f32` matches its use in Task 3. `wb_camera_to_working(&ColorProfile, f32, WorkingSpace) -> [[f32;3];3]` matches its call in Task 4's `camera_to_working`. `camera_to_working(&self, temp: f32)` — all six call sites updated to pass a temp. `preview_tier_source(cam, pw).1` returns the `[[f32;3];3]` matrix (consistent with existing sites at 1326/1901). `set_color_matrix([[f32;3];3])` exists on both `EditPipeline` and `EditTileProducer` (via `edit_producer`). ✓
