# P2 Plan 1 — Dual-illuminant decode + color math Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ColorProfile` in `ferrolite-decode` a dual-illuminant carrier (both DNG matrices + white points, additive), and add `camera_to_working_interpolated` (inverse-CCT/mired weighting) + CCT↔xy helpers to `ferrolite-color`, with single-calibration reducing to today's `camera_to_working`.

**Architecture:** Pure CPU, engine-internal. `ferrolite-decode` surfaces *all* usable camera calibrations as an additive `calibrations: Vec<CameraCalibration>` field while keeping today's single-matrix fields (`xyz_to_cam`/`white_xy`/`is_fallback`) unchanged for existing consumers. `ferrolite-color` gains CCT↔xy helpers and a `camera_to_working_interpolated` that linearly blends the two matrices' elements by mired weight, then composes via the existing `camera_to_working`. Nothing is wired into the app/GPU pipeline in this plan (that is Plan 2).

**Tech Stack:** Rust, `cargo` workspace. Crates touched: `ferrolite-color` (pure color math), `ferrolite-decode` (rawler-fed decode products), and one test-helper literal in `ferrolite-app`. No new dependencies.

## Global Constraints

- **rawler pinned at `0.7.2`** — do not bump. It exposes only `color_matrix: HashMap<Illuminant, FlatColorMatrix>` (DNG `ColorMatrix`, XYZ→camera); **no** typed `ForwardMatrix`.
- **Photo-tier crates only** (`ferrolite-decode`, `ferrolite-color`); no engine-tier changes. Pure CPU — **no UI, no GPU, no `rawler` in `ferrolite-color`** (its `lib.rs` invariant).
- **Additive decode product (map §3 / spec §4):** existing `ColorProfile` fields and their meaning must stay unchanged; only new fields/methods are added. Existing consumers keep compiling and behaving identically.
- **Never panic** on bad camera data (spec §6): missing/short/singular matrix → sRGB fallback, logged; a singular matrix degrades to identity camera→XYZ.
- **Immutability / small focused files** (CLAUDE.md-adjacent rules): return new values, don't mutate in place; ≤800 lines/file; per-function ≤50 lines.
- **Gate (per branch):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` all green.
- **No visual test this plan** (spec §7 Plan 1 — engine-internal, not yet wired into the running app). After the green gate, **hold at the gate**; the real color visual test lands with Plan 2. Do not merge/PR — hand the author the "nothing to visually test" note (CLAUDE.md "Finishing a branch").

## Resolved open questions (spec §8, resolved at plan-write time)

- **ForwardMatrix:** rawler 0.7.2 does **not** surface a decoded ForwardMatrix (`RawImage` has only `color_matrix` + a raw `dng_tags` map; the `ForwardMatrix1/2/3` tags appear only in the DNG *encoder*). **Decision: invert `ColorMatrix` for cam→XYZ, unchanged from today.** Revisit only if a future rawler exposes it as a typed per-illuminant product.
- **CCT↔xy method:** **`cct_to_xy` = Kim et al. (2002) cubic Planckian-locus approximation** (valid 1667–25000 K); **`xy_to_cct` = McCamy's cubic approximation.** They round-trip to ~0.2 % across the A (≈2856 K) – D65 (≈6504 K) range, matching DNG's interpolation domain closely enough.
- **Interpolation domain:** linearly interpolate the **`xyz_to_cam` (DNG `ColorMatrix`) elements**, weighted by **inverse CCT (mired)** — DNG's convention — then compose via the existing `camera_to_working` using the target illuminant's white point `cct_to_xy(target_cct)`.
- **Out of scope this plan (Plan 2+):** WB-op reconciliation, `ColorMatrixNode` recompute, any GPU/app wiring, RCD, unclamp.

---

## File Structure

- `ferrolite-color/src/cct.rs` **(new)** — CCT↔xy helpers (`cct_to_xy`, `xy_to_cct`). One responsibility: colour-temperature ⇄ chromaticity.
- `ferrolite-color/src/interpolate.rs` **(new)** — dual-illuminant blend (`camera_to_working_interpolated` + private `interpolate_xyz_to_cam`, `mired_weight`, `lerp_mat3`). One responsibility: mired-weighted matrix interpolation, composed via `camera::camera_to_working`.
- `ferrolite-color/src/lib.rs` **(modify)** — declare the two new modules; re-export the new public items.
- `ferrolite-decode/src/color.rs` **(modify)** — add `CameraCalibration`, add `calibrations: Vec<CameraCalibration>` to `ColorProfile`, populate it in `from_color_matrix` + `srgb_fallback` (primary fields unchanged).
- `ferrolite-decode/src/lib.rs` **(modify)** — re-export `CameraCalibration`.
- `ferrolite-app/src/develop/preview_cache.rs` **(modify, test only)** — update the `sample_profile()` struct literal for the new field.

---

## Task 1: CCT↔xy helpers (`ferrolite-color`)

**Files:**
- Create: `ferrolite-color/src/cct.rs`
- Modify: `ferrolite-color/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `ferrolite-color/src/cct.rs`

**Interfaces:**
- Consumes: `crate::matrix::Xy` (`{ x: f32, y: f32 }`).
- Produces:
  - `pub fn cct_to_xy(cct_k: f32) -> Xy` — Kelvin → CIE 1931 xy on the Planckian locus (input clamped to 1667–25000 K).
  - `pub fn xy_to_cct(xy: Xy) -> f32` — CIE 1931 xy → correlated colour temperature (Kelvin), McCamy.

- [ ] **Step 1: Write the failing tests**

Create `ferrolite-color/src/cct.rs`:

```rust
//! Correlated-colour-temperature ⇄ CIE 1931 xy helpers.
//!
//! `cct_to_xy` follows Kim et al.'s (2002) cubic Planckian-locus approximation
//! (valid 1667–25000 K); `xy_to_cct` uses McCamy's cubic approximation. Together
//! they round-trip to ~0.2 % across the 2000–7000 K range covering the DNG
//! calibration illuminants (Standard-A ≈ 2856 K, D65 ≈ 6504 K), matching DNG's
//! interpolation domain closely enough (P2 spec §8). Pure, `unsafe`-free.

use crate::matrix::Xy;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cct_to_xy_near_d65() {
        // D65 ≈ 6504 K sits just off the Planckian locus; expect close xy.
        let xy = cct_to_xy(6504.0);
        assert!((xy.x - 0.3128).abs() < 0.01, "x={}", xy.x);
        assert!((xy.y - 0.3290).abs() < 0.02, "y={}", xy.y);
    }

    #[test]
    fn cct_to_xy_near_standard_a() {
        // Standard illuminant A = 2856 K, xy ≈ (0.4476, 0.4074).
        let xy = cct_to_xy(2856.0);
        assert!((xy.x - 0.4476).abs() < 0.01, "x={}", xy.x);
        assert!((xy.y - 0.4074).abs() < 0.01, "y={}", xy.y);
    }

    #[test]
    fn xy_to_cct_recovers_standard_a() {
        let cct = xy_to_cct(Xy { x: 0.4476, y: 0.4074 });
        assert!((cct - 2856.0).abs() < 100.0, "cct={cct}");
    }

    #[test]
    fn round_trips_within_two_percent() {
        for &t in &[2856.0_f32, 3500.0, 5000.0, 6504.0] {
            let back = xy_to_cct(cct_to_xy(t));
            let rel = (back - t).abs() / t;
            assert!(rel < 0.02, "T={t} round-tripped to {back} (rel {rel})");
        }
    }

    #[test]
    fn cct_to_xy_clamps_out_of_range_input() {
        // Below/above the approximation's valid range must not produce NaN/Inf.
        for &t in &[100.0_f32, 1e6] {
            let xy = cct_to_xy(t);
            assert!(xy.x.is_finite() && xy.y.is_finite(), "T={t} -> {xy:?}");
        }
    }
}
```

Add the module to `ferrolite-color/src/lib.rs` — after `mod camera;` insert `mod cct;`, and after `pub use camera::...;` add:

```rust
pub use cct::{cct_to_xy, xy_to_cct};
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-color cct::`
Expected: FAIL — `cannot find function cct_to_xy` / `xy_to_cct` not found.

- [ ] **Step 3: Write minimal implementation**

Insert the two functions into `ferrolite-color/src/cct.rs`, directly below the `use crate::matrix::Xy;` line (above the `#[cfg(test)]` module):

```rust
/// Correlated colour temperature (Kelvin) → CIE 1931 xy on the Planckian locus.
/// Kim et al. (2002); input clamped to the approximation's valid 1667–25000 K.
pub fn cct_to_xy(cct_k: f32) -> Xy {
    let t = f64::from(cct_k.clamp(1667.0, 25000.0));
    let inv = 1.0 / t;
    let inv2 = inv * inv;
    let inv3 = inv2 * inv;
    let x = if t <= 4000.0 {
        -0.266_123_9e9 * inv3 - 0.234_358_9e6 * inv2 + 0.877_695_6e3 * inv + 0.179_910
    } else {
        -3.025_846_9e9 * inv3 + 2.107_037_9e6 * inv2 + 0.222_634_7e3 * inv + 0.240_390
    };
    let x2 = x * x;
    let x3 = x2 * x;
    let y = if t <= 2222.0 {
        -1.106_381_4 * x3 - 1.348_110_2 * x2 + 2.185_558_32 * x - 0.202_196_83
    } else if t <= 4000.0 {
        -0.954_947_6 * x3 - 1.374_185_93 * x2 + 2.091_370_15 * x - 0.167_488_67
    } else {
        3.081_758_0 * x3 - 5.873_386_7 * x2 + 3.751_129_97 * x - 0.370_014_83
    };
    Xy {
        x: x as f32,
        y: y as f32,
    }
}

/// CIE 1931 xy → correlated colour temperature (Kelvin), McCamy's approximation.
pub fn xy_to_cct(xy: Xy) -> f32 {
    let n = (xy.x - 0.3320) / (0.1858 - xy.y);
    449.0 * n * n * n + 3525.0 * n * n + 6823.3 * n + 5520.33
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-color cct::`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-color/src/cct.rs ferrolite-color/src/lib.rs
git commit -m "feat(color): CCT<->xy helpers (Kim locus + McCamy)"
```

---

## Task 2: Mired-weighted dual-illuminant interpolation (`ferrolite-color`)

**Files:**
- Create: `ferrolite-color/src/interpolate.rs`
- Modify: `ferrolite-color/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `ferrolite-color/src/interpolate.rs`

**Interfaces:**
- Consumes: `crate::camera::camera_to_working`, `crate::cct::{cct_to_xy, xy_to_cct}` (Task 1), `crate::matrix::{identity, Mat3, Xy}`, `crate::working_space::WorkingSpace`.
- Produces:
  - `pub fn camera_to_working_interpolated(calibrations: &[(Xy, Mat3)], target_cct: f32, working: WorkingSpace) -> Mat3` — dual-illuminant camera→working matrix. 0 calibrations → `identity()`; 1 → exactly `camera_to_working`; ≥2 → mired-weighted blend of the lowest/highest-CCT `xyz_to_cam`, composed via `camera_to_working` at `cct_to_xy(target_cct)`.
  - `pub(crate) fn interpolate_xyz_to_cam(calibrations: &[(Xy, Mat3)], target_cct: f32) -> Option<(Mat3, Xy)>` — the blended `xyz_to_cam` + its reference white (unit-testable apart from the working composition).

- [ ] **Step 1: Write the failing tests**

Create `ferrolite-color/src/interpolate.rs`:

```rust
//! Dual-illuminant camera→working interpolation (DNG-style, mired-weighted).
//!
//! Linearly blends the two DNG `ColorMatrix` (`xyz_to_cam`) calibrations by
//! inverse CCT (mired) — DNG's convention — then composes the camera→working
//! transform via [`camera_to_working`] using the target illuminant's white
//! point. A single calibration reduces exactly to [`camera_to_working`]; none
//! degrades to identity (decode always supplies ≥1). Pure, `unsafe`-free.

use crate::camera::camera_to_working;
use crate::cct::{cct_to_xy, xy_to_cct};
use crate::matrix::{identity, Mat3, Xy};
use crate::working_space::WorkingSpace;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::approx_eq_mat3;

    const A_WHITE: Xy = Xy { x: 0.4476, y: 0.4074 };
    const D65_WHITE: Xy = Xy { x: 0.3128, y: 0.3290 };

    // Two visibly distinct fake calibration matrices.
    const M_A: Mat3 = [[1.0, 0.1, 0.0], [0.2, 1.0, 0.1], [0.0, 0.2, 1.0]];
    const M_D65: Mat3 = [[1.5, -0.2, 0.0], [-0.1, 1.4, -0.1], [0.0, -0.3, 1.6]];

    #[test]
    fn zero_calibrations_is_identity() {
        let m = camera_to_working_interpolated(&[], 5000.0, WorkingSpace::Rec2020);
        assert!(approx_eq_mat3(&m, &identity(), 1e-6));
    }

    #[test]
    fn single_calibration_equals_camera_to_working() {
        let cal = [(D65_WHITE, M_D65)];
        let got = camera_to_working_interpolated(&cal, 5000.0, WorkingSpace::Rec2020);
        let want = camera_to_working(M_D65, D65_WHITE, WorkingSpace::Rec2020);
        assert!(approx_eq_mat3(&got, &want, 1e-6), "got {got:?} want {want:?}");
    }

    #[test]
    fn blend_at_low_endpoint_selects_low_matrix() {
        let cals = [(A_WHITE, M_A), (D65_WHITE, M_D65)];
        let (m, _white) =
            interpolate_xyz_to_cam(&cals, xy_to_cct(A_WHITE)).expect("two calibrations");
        assert!(approx_eq_mat3(&m, &M_A, 1e-6), "at A expected M_A, got {m:?}");
    }

    #[test]
    fn blend_at_high_endpoint_selects_high_matrix() {
        let cals = [(A_WHITE, M_A), (D65_WHITE, M_D65)];
        let (m, _white) =
            interpolate_xyz_to_cam(&cals, xy_to_cct(D65_WHITE)).expect("two calibrations");
        assert!(approx_eq_mat3(&m, &M_D65, 1e-6), "at D65 expected M_D65, got {m:?}");
    }

    #[test]
    fn blend_midpoint_is_between_endpoints() {
        let cals = [(A_WHITE, M_A), (D65_WHITE, M_D65)];
        let mid_mired = 0.5 * (1.0 / xy_to_cct(A_WHITE) + 1.0 / xy_to_cct(D65_WHITE));
        let mid_cct = 1.0 / mid_mired;
        let (m, _white) = interpolate_xyz_to_cam(&cals, mid_cct).expect("two calibrations");
        // Element [0][0] must sit strictly between 1.0 and 1.5.
        assert!(m[0][0] > 1.0 && m[0][0] < 1.5, "midpoint [0][0]={}", m[0][0]);
        // At the mired midpoint the blend weight is 0.5, so it is the average.
        let avg = 0.5 * (M_A[0][0] + M_D65[0][0]);
        assert!((m[0][0] - avg).abs() < 1e-4, "expected avg {avg}, got {}", m[0][0]);
    }

    #[test]
    fn calibration_order_does_not_matter() {
        let forward = [(A_WHITE, M_A), (D65_WHITE, M_D65)];
        let reversed = [(D65_WHITE, M_D65), (A_WHITE, M_A)];
        let a = camera_to_working_interpolated(&forward, 4000.0, WorkingSpace::Rec2020);
        let b = camera_to_working_interpolated(&reversed, 4000.0, WorkingSpace::Rec2020);
        assert!(approx_eq_mat3(&a, &b, 1e-6));
    }

    #[test]
    fn output_is_finite_for_all_working_spaces() {
        let cals = [(A_WHITE, M_A), (D65_WHITE, M_D65)];
        for space in WorkingSpace::ALL {
            let m = camera_to_working_interpolated(&cals, 5000.0, space);
            assert!(m.iter().flatten().all(|v: &f32| v.is_finite()), "{space:?}");
        }
    }
}
```

Add to `ferrolite-color/src/lib.rs` — after `mod cct;` insert `mod interpolate;`, and after the `pub use cct::...;` line add:

```rust
pub use interpolate::camera_to_working_interpolated;
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-color interpolate::`
Expected: FAIL — `cannot find function camera_to_working_interpolated` / `interpolate_xyz_to_cam`.

- [ ] **Step 3: Write minimal implementation**

Insert into `ferrolite-color/src/interpolate.rs`, between the `use` block and the `#[cfg(test)]` module:

```rust
/// Interpolated camera→working matrix following the white-balance temperature.
///
/// `calibrations` are the camera's DNG calibration points as
/// `(reference_white_xy, xyz_to_cam)` pairs (from `ColorProfile::calibrations`);
/// `target_cct` is the scene / white-balance colour temperature (Kelvin).
pub fn camera_to_working_interpolated(
    calibrations: &[(Xy, Mat3)],
    target_cct: f32,
    working: WorkingSpace,
) -> Mat3 {
    match interpolate_xyz_to_cam(calibrations, target_cct) {
        Some((xyz_to_cam, cam_white)) => camera_to_working(xyz_to_cam, cam_white, working),
        None => identity(),
    }
}

/// Blend the calibrations' `xyz_to_cam` for `target_cct`, returning the matrix
/// and its reference white. `None` when there are no calibrations.
///
/// * 1 calibration  → that matrix + its own white (reduces to `camera_to_working`).
/// * ≥2 calibrations → linear blend of the lowest- and highest-CCT matrices,
///   weighted by inverse CCT (mired), with the target illuminant's white
///   `cct_to_xy(target_cct)`.
pub(crate) fn interpolate_xyz_to_cam(
    calibrations: &[(Xy, Mat3)],
    target_cct: f32,
) -> Option<(Mat3, Xy)> {
    match calibrations.len() {
        0 => None,
        1 => Some((calibrations[0].1, calibrations[0].0)),
        _ => {
            // Order by CCT so the low/high endpoints are well-defined and the
            // result is independent of the input order.
            let mut by_cct: Vec<(f32, Mat3)> = calibrations
                .iter()
                .map(|(white, m)| (xy_to_cct(*white), *m))
                .collect();
            by_cct.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let (cct_lo, m_lo) = by_cct[0];
            let (cct_hi, m_hi) = by_cct[by_cct.len() - 1];
            let f = mired_weight(target_cct, cct_lo, cct_hi);
            Some((lerp_mat3(&m_lo, &m_hi, f), cct_to_xy(target_cct)))
        }
    }
}

/// DNG mired interpolation weight toward the high-CCT endpoint, clamped [0,1]:
/// `f = (1/target − 1/cct_lo) / (1/cct_hi − 1/cct_lo)`.
fn mired_weight(target_cct: f32, cct_lo: f32, cct_hi: f32) -> f32 {
    let denom = 1.0 / cct_hi - 1.0 / cct_lo;
    if denom.abs() < f32::EPSILON {
        return 0.0;
    }
    ((1.0 / target_cct - 1.0 / cct_lo) / denom).clamp(0.0, 1.0)
}

/// Element-wise linear blend `(1 − f)·a + f·b`.
#[allow(clippy::needless_range_loop)] // explicit i/j indexing is clearest for a fixed 3×3.
fn lerp_mat3(a: &Mat3, b: &Mat3, f: f32) -> Mat3 {
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = a[i][j] * (1.0 - f) + b[i][j] * f;
        }
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-color interpolate::`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-color/src/interpolate.rs ferrolite-color/src/lib.rs
git commit -m "feat(color): mired-weighted dual-illuminant camera_to_working_interpolated"
```

---

## Task 3: Dual-illuminant `ColorProfile` carrier (`ferrolite-decode`)

**Files:**
- Modify: `ferrolite-decode/src/color.rs`
- Modify: `ferrolite-decode/src/lib.rs:16` (re-export)
- Modify: `ferrolite-app/src/develop/preview_cache.rs:349-355` (test helper literal)
- Test: inline `#[cfg(test)] mod tests` in `ferrolite-decode/src/color.rs`

**Interfaces:**
- Consumes: `rawler::imgop::xyz::{FlatColorMatrix, Illuminant}`, existing `illuminant_to_xy`.
- Produces:
  - `pub struct CameraCalibration { pub xyz_to_cam: [[f32; 3]; 3], pub white_xy: [f32; 2] }` — one calibration point.
  - New field on `ColorProfile`: `pub calibrations: Vec<CameraCalibration>` — all usable calibrations (≥1; sorted by white_xy for determinism). Existing fields `xyz_to_cam` / `white_xy` / `is_fallback` are **unchanged** (the D65-or-first "primary"). The app/pipeline layer (Plan 2) maps `calibrations` into `&[(Xy, Mat3)]` for `camera_to_working_interpolated`.

- [ ] **Step 1: Write the failing tests**

Add these tests inside the existing `#[cfg(test)] mod tests` in `ferrolite-decode/src/color.rs` (after `illuminant_to_xy_covers_common_illuminants`):

```rust
    #[test]
    fn surfaces_both_calibrations_for_dual_illuminant() {
        let mut m: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        m.insert(Illuminant::A, vec![9.0; 9]);
        m.insert(
            Illuminant::D65,
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0],
        );
        let p = ColorProfile::from_color_matrix(&m);
        assert!(!p.is_fallback);
        assert_eq!(p.calibrations.len(), 2, "both A and D65 surfaced");
        // Primary fields unchanged: D65 preferred.
        assert_eq!(
            p.xyz_to_cam,
            [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]]
        );
        assert_eq!(p.white_xy, [0.31271, 0.32902]);
        // Both matrices present among the calibrations.
        let mats: Vec<_> = p.calibrations.iter().map(|c| c.xyz_to_cam).collect();
        assert!(mats.contains(&[[9.0; 3]; 3]));
        assert!(mats.contains(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]]));
    }

    #[test]
    fn single_illuminant_surfaces_one_calibration() {
        let mut m: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        m.insert(Illuminant::A, vec![2.0; 9]);
        let p = ColorProfile::from_color_matrix(&m);
        assert!(!p.is_fallback);
        assert_eq!(p.calibrations.len(), 1);
        assert_eq!(p.calibrations[0].xyz_to_cam, [[2.0; 3]; 3]);
        assert_eq!(p.calibrations[0].white_xy, illuminant_to_xy(Illuminant::A));
    }

    #[test]
    fn fallback_has_one_calibration_matching_primary() {
        let p = ColorProfile::srgb_fallback();
        assert!(p.is_fallback);
        assert_eq!(p.calibrations.len(), 1);
        assert_eq!(p.calibrations[0].xyz_to_cam, p.xyz_to_cam);
        assert_eq!(p.calibrations[0].white_xy, p.white_xy);
    }

    #[test]
    fn empty_map_falls_back_with_one_calibration() {
        let empty: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        let p = ColorProfile::from_color_matrix(&empty);
        assert!(p.is_fallback);
        assert_eq!(p.calibrations.len(), 1);
    }

    #[test]
    fn short_matrix_is_excluded_from_calibrations() {
        let mut m: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        m.insert(Illuminant::A, vec![1.0, 2.0, 3.0]); // too short
        m.insert(Illuminant::D65, vec![5.0; 9]); // usable
        let p = ColorProfile::from_color_matrix(&m);
        assert!(!p.is_fallback);
        assert_eq!(p.calibrations.len(), 1, "only the usable D65 matrix");
        assert_eq!(p.calibrations[0].xyz_to_cam, [[5.0; 3]; 3]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-decode color::`
Expected: FAIL — `no field 'calibrations' on type 'ColorProfile'` / `cannot find type 'CameraCalibration'` (and the existing `prefers_d65_and_reshapes_to_3x3` literal will need the new field — fixed in Step 3).

- [ ] **Step 3: Write minimal implementation**

In `ferrolite-decode/src/color.rs`, add the `CameraCalibration` struct immediately above `pub struct ColorProfile`:

```rust
/// One camera calibration point: a DNG-style XYZ→camera 3×3 matrix and the
/// CIE 1931 xy white point of the reference illuminant it was calibrated at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraCalibration {
    /// XYZ (reference illuminant) → camera-native linear RGB, row-major 3×3.
    pub xyz_to_cam: [[f32; 3]; 3],
    /// Reference illuminant white point, CIE 1931 xy.
    pub white_xy: [f32; 2],
}
```

Add the new field to `ColorProfile` (keep all existing fields and their doc comments unchanged; append):

```rust
    /// True when this is the synthetic sRGB fallback (no usable camera matrix).
    pub is_fallback: bool,
    /// All usable camera calibrations (≥1), sorted by white point for a
    /// deterministic order. Additive (architecture map §3): `xyz_to_cam` /
    /// `white_xy` above remain the primary single-matrix view for existing
    /// consumers; new consumers (dual-illuminant interpolation) read this.
    pub calibrations: Vec<CameraCalibration>,
```

Replace `srgb_fallback` so it populates one calibration:

```rust
    pub fn srgb_fallback() -> Self {
        let xyz_to_cam = [
            [3.2404542, -1.5371385, -0.4985314],
            [-0.969_266, 1.8760108, 0.0415560],
            [0.0556434, -0.2040259, 1.0572252],
        ];
        let white_xy = [0.31271, 0.32902]; // D65
        Self {
            xyz_to_cam,
            white_xy,
            is_fallback: true,
            calibrations: vec![CameraCalibration {
                xyz_to_cam,
                white_xy,
            }],
        }
    }
```

Replace `from_color_matrix` so it collects all usable calibrations while keeping the primary selection unchanged:

```rust
    pub fn from_color_matrix(matrices: &HashMap<Illuminant, FlatColorMatrix>) -> Self {
        // All usable (≥9-element) calibrations, sorted by white point so the
        // order is deterministic regardless of HashMap iteration order.
        let mut calibrations: Vec<CameraCalibration> = matrices
            .iter()
            .filter(|(_, flat)| flat.len() >= 9)
            .map(|(illum, flat)| CameraCalibration {
                xyz_to_cam: reshape_3x3(flat),
                white_xy: illuminant_to_xy(*illum),
            })
            .collect();
        calibrations.sort_by(|a, b| {
            a.white_xy[0]
                .partial_cmp(&b.white_xy[0])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    a.white_xy[1]
                        .partial_cmp(&b.white_xy[1])
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });

        // Primary single matrix: prefer D65, else any usable matrix (unchanged).
        let picked = matrices
            .get(&Illuminant::D65)
            .filter(|flat| flat.len() >= 9)
            .map(|flat| (Illuminant::D65, flat))
            .or_else(|| {
                matrices
                    .iter()
                    .find(|(_, flat)| flat.len() >= 9)
                    .map(|(illum, flat)| (*illum, flat))
            });

        match picked {
            Some((illum, flat)) => Self {
                xyz_to_cam: reshape_3x3(flat),
                white_xy: illuminant_to_xy(illum),
                is_fallback: false,
                calibrations,
            },
            None => {
                eprintln!("ferrolite-decode: no usable camera color matrix; using sRGB fallback");
                Self::srgb_fallback()
            }
        }
    }
```

Add the private reshape helper below the `impl ColorProfile` block (above `illuminant_to_xy`):

```rust
/// Reshape a rawler flat color matrix (≥9 elements) into a row-major 3×3.
fn reshape_3x3(flat: &FlatColorMatrix) -> [[f32; 3]; 3] {
    [
        [flat[0], flat[1], flat[2]],
        [flat[3], flat[4], flat[5]],
        [flat[6], flat[7], flat[8]],
    ]
}
```

Update the existing `prefers_d65_and_reshapes_to_3x3` test — its final assertions on primary fields stay; add after them:

```rust
        assert_eq!(p.calibrations.len(), 2);
```

Re-export the new type from `ferrolite-decode/src/lib.rs:16` — change:

```rust
pub use color::ColorProfile;
```
to:
```rust
pub use color::{CameraCalibration, ColorProfile};
```

Update the `ferrolite-app` test helper `sample_profile()` at `ferrolite-app/src/develop/preview_cache.rs:349` to include the new field:

```rust
    fn sample_profile() -> ColorProfile {
        let xyz_to_cam = [[1.0, 0.1, 0.2], [0.3, 1.1, 0.4], [0.5, 0.6, 1.2]];
        let white_xy = [0.3127, 0.3290];
        ColorProfile {
            xyz_to_cam,
            white_xy,
            is_fallback: false,
            calibrations: vec![ferrolite_decode::CameraCalibration {
                xyz_to_cam,
                white_xy,
            }],
        }
    }
```

(`preview_cache.rs` already imports `ColorProfile`; reference `CameraCalibration` via the full `ferrolite_decode::` path as shown to avoid touching the import list.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-decode color:: && cargo test -p ferrolite-app --lib preview_cache`
Expected: PASS — new `color::` tests pass, existing `color::` tests still pass, `preview_cache` tests compile and pass.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-decode/src/color.rs ferrolite-decode/src/lib.rs ferrolite-app/src/develop/preview_cache.rs
git commit -m "feat(decode): dual-illuminant ColorProfile carrier (additive calibrations)"
```

---

## Task 4: Workspace green gate

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then: `cargo fmt --check`
Expected: no diff.

- [ ] **Step 2: Clippy (warnings as errors)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings/errors. If `clippy::excessive_precision` fires on any `cct.rs` literal, that literal is already at f64 precision — leave the f64 computation and cast at the end (as written); do not truncate the constants.

- [ ] **Step 3: Full workspace test**

Run: `cargo test --workspace`
Expected: PASS (all crates, including the new `cct::`, `interpolate::`, and `color::` tests).

- [ ] **Step 4: Commit any formatting fixups**

```bash
git add -A
git commit -m "chore: cargo fmt for P2 plan 1" || echo "nothing to format"
```

- [ ] **Step 5: Hold at the gate — hand the author the visual-test note**

Per CLAUDE.md "Finishing a branch" and spec §7 Plan 1: **there is nothing to visually test this plan.** *Why:* all changes are engine-internal pure-CPU library code (`ferrolite-color` interpolation/CCT helpers + an additive `ferrolite-decode` `ColorProfile` field). Nothing is wired into the running app's render path — `camera_to_working_interpolated` has no caller yet, and the app still reads the unchanged primary `xyz_to_cam` / `white_xy`. No UI, panel, control, or gesture changed; no behavior reachable from FerroLite is altered. The real hands-on color test (dragging WB temp on a dual-illuminant RAW and watching color track) lands in **Plan 2** ("Live WB-driven matrix wiring"), which wires these functions into `ColorMatrixNode`. Present the finish options only after stating this; do **not** merge/PR until the author acknowledges.

---

## Self-Review

**Spec coverage (§7 Plan 1 + §5.1 + §10):**
- "ColorProfile dual-illuminant carrier in ferrolite-decode" → Task 3 (additive `calibrations` field, both matrices + white points).
- "camera_to_working_interpolated (inverse-CCT / mired weighting)" → Task 2 (`mired_weight` + `lerp_mat3`, composed via `camera_to_working`).
- "CCT↔xy helpers" → Task 1 (`cct_to_xy`, `xy_to_cct`).
- "single-calibration reduces to today's camera_to_working" → Task 2 `single_calibration_equals_camera_to_working` test.
- §10 CPU tests: weight at A → matrix A / at D65 → matrix D65 / sane midpoint (Task 2); CCT↔xy round-trips (Task 1); single-calibration == old (Task 2); no-matrix → fallback, ColorProfile surfaces both calibrations, single/none fallback selection (Task 3). ✓
- §8 open questions (ForwardMatrix, CCT↔xy method, interpolation domain) resolved in the "Resolved open questions" section. ✓
- Out-of-scope items (unclamp, RCD, WB wiring, GPU) correctly excluded — those are Plans 2–5. ✓

**Placeholder scan:** No TBD/TODO/"handle edge cases"; every code step shows complete code; every test step shows complete assertions; every run step states expected output. ✓

**Type consistency:** `Xy`/`Mat3` used consistently from `crate::matrix`; `camera_to_working_interpolated(&[(Xy, Mat3)], f32, WorkingSpace) -> Mat3` matches its consumer description; `CameraCalibration { xyz_to_cam: [[f32;3];3], white_xy: [f32;2] }` matches the decode field type and the app literal; `xy_to_cct`/`cct_to_xy` signatures match between Task 1 (definition) and Task 2 (use). `reshape_3x3` used in both `from_color_matrix` branches. ✓
