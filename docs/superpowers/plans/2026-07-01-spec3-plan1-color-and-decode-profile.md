# Spec 3 — Plan 1: `ferrolite-color` foundation + decode color profile Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the new photo-tier crate `ferrolite-color` — pure, `unsafe`-free color math (working-space definitions, RGB↔XYZ matrices, Bradford camera→working adaptation, working→display / working→output transforms, sRGB OETF, and ICC emit/parse via `moxcms`) — and extend `ferrolite-decode` to surface a camera `ColorProfile` (XYZ→camera matrix + reference white point) from `rawler`, with a graceful sRGB fallback.

**Architecture:** `ferrolite-color` is a leaf photo-tier crate: it depends only on `moxcms` (pure Rust), `thiserror`, and `serde` — **no GPU, no UI, no `rawler`, no `ferrolite-*`**. All transforms are computed from primaries + white points at runtime through a tiny `Mat3` linear-algebra core, so every value is checkable against published references. `ferrolite-decode` gains one additive product — `ColorProfile` — extracted from `rawler`'s `color_matrix` and hung on the existing `RawDecoded`; existing consumers ignore it. The two crates do **not** depend on each other; `ferrolite-pipeline` (Plan 2) is the glue that feeds `RawDecoded.color_profile`'s arrays into `ferrolite_color::camera_to_working`.

**Tech Stack:** Rust 2021, `moxcms` 0.8 (pure-Rust ICC), `thiserror`, `serde` (WorkingSpace persistence in Plan 2), `rawler` 0.7.2 (decode side only). No GPU, no async. All tests are pure CPU unit tests that run on every OS in CI.

## Global Constraints

Copied verbatim from the spec (`docs/superpowers/specs/2026-07-01-spec3-color-and-export-design.md`) and the architecture map + repo conventions — every task implicitly includes these:

- **Branch:** all work on `feat/color-and-export` (already created off `main`).
- **License:** both touched/created crates are **photo tier**, `GPL-3.0-only` (may pull LGPL/GPL deps). This plan does **not** touch any engine-transferable crate (`ferrolite-gpu` / `ferrolite-vt` / `ferrolite-image` / `ferrolite-jobs`) — the photo-specific choice of working space and matrices lives only in `ferrolite-color`.
- **`ferrolite-color` is pure:** `Clone`, no GPU/UI coupling, **no `unsafe`**, no `rawler`, no `ferrolite-*` deps. The whole crate is unit-testable on every OS in CI (spec §4.1).
- **No UI, no GPU in this plan.** No `wgpu`, no `egui`, no shader files. Those arrive in Plan 2+.
- **ICC path is moxcms-only for Plan 1.** `moxcms` emits standard profiles for all 5 spaces; the `lcms2` C-binding fallback is **deliberately deferred** (adding a C toolchain dep would break "unit-testable on every OS in CI"). Record this; do not add `lcms2`.
- **Pinned versions (do not bump):** `rawler = "0.7.2"`, `rusqlite` pinned 0.32 (untouched here). Rust floor `rust-version = "1.88"`. Add `moxcms = "0.8"` (already resolved transitively in `Cargo.lock`).
- **Decode contract (architecture map §3):** decode yields **separable products**; `ColorProfile` is purely additive alongside `{ PreviewImage, RawImage, Metadata }`. Never panic — a missing/invalid matrix logs and falls back (spec §6, §10).
- **Immutability / style:** functions return new values; borrow (`&`) by default; typed errors via `thiserror`; no `unwrap()`/`expect()` outside tests and provably-invertible constant matrices (document each `expect`).
- **Logging convention:** the repo uses `eprintln!` for diagnostics (no `log`/`tracing` dep anywhere). The fallback message uses `eprintln!` to match.
- **Clippy:** `cargo clippy --workspace --all-targets -- -D warnings` must pass. The 3×3 math uses index loops; annotate those fns with `#[allow(clippy::needless_range_loop)]` + a one-line rationale rather than contorting the math.
- **Commit style:** conventional commits (`feat:` / `test:` / `chore:`), no attribution trailer (disabled globally; matches repo history).
- **Gate (end of plan):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green, then **STOP and hold for the author's (Jann's) review** before finishing (CLAUDE.md; this plan has no runnable UI, so "visual test" = Jann's code/design review of the two crates).

---

## File Structure

**New crate `ferrolite-color/`:**
- `Cargo.toml` — manifest (workspace deps: `moxcms`, `thiserror`, `serde`).
- `src/lib.rs` — module declarations + public re-exports + crate doc.
- `src/matrix.rs` — `Mat3` alias + `Xy` white-point type; pure 3×3 ops (`identity`, `mul_mat3`, `mul_vec3`, `inverse`, `diag`) and `approx_eq_mat3` test helper. Generic linear algebra, no color concepts.
- `src/working_space.rs` — `WorkingSpace` enum (the curated 5), `Default = Rec2020`, primaries/white per space, `rgb_to_xyz()`, `xyz_to_rgb()`, `white_point()`.
- `src/adapt.rs` — Bradford `chromatic_adaptation(src, dst) -> Mat3`.
- `src/camera.rs` — `camera_to_working(xyz_to_cam, cam_white, working) -> Mat3`.
- `src/tail.rs` — `working_to_display(working) -> Mat3`, `working_to_output(working, output) -> Mat3`.
- `src/oetf.rs` — `srgb_oetf`, `srgb_eotf` (pure transfer functions).
- `src/icc.rs` — `emit_icc(space) -> Result<Vec<u8>, ColorError>`, `parse_icc(bytes) -> Result<(), ColorError>`.
- `src/error.rs` — `ColorError` (`thiserror`).

**New file in `ferrolite-decode/`:**
- `src/color.rs` — `ColorProfile { xyz_to_cam, white_xy, is_fallback }`, `srgb_fallback()`, `from_color_matrix(&HashMap<Illuminant, FlatColorMatrix>)`, `illuminant_to_xy(Illuminant)`.

**Modified:**
- `Cargo.toml` (root) — add `ferrolite-color` to `members` + `workspace.dependencies`; add `moxcms = "0.8"`.
- `ferrolite-decode/src/raw.rs` — add `color_profile: ColorProfile` field to `RawDecoded`; populate in `decode_full`.
- `ferrolite-decode/src/lib.rs` — `mod color;` + `pub use color::ColorProfile;`.

**Reference constants used in tests (from `rawler-0.7.2/src/imgop/xyz.rs`, for cross-checking — do not import rawler into `ferrolite-color`):**
- `SRGB_TO_XYZ_D65 = [[0.4124564,0.3575761,0.1804375],[0.2126729,0.7151522,0.0721750],[0.0193339,0.1191920,0.9503041]]`
- `XYZ_TO_SRGB_D65 = [[3.2404542,-1.5371385,-0.4985314],[-0.9692660,1.8760108,0.0415560],[0.0556434,-0.2040259,1.0572252]]`

---

### Task 1: Crate skeleton + `Mat3` linear-algebra core

**Files:**
- Create: `ferrolite-color/Cargo.toml`
- Create: `ferrolite-color/src/lib.rs`
- Create: `ferrolite-color/src/matrix.rs`
- Modify: `Cargo.toml` (root — `members` + `workspace.dependencies`)

**Interfaces:**
- Produces: `pub type Mat3 = [[f32; 3]; 3];`; `pub struct Xy { pub x: f32, pub y: f32 }` with `pub fn to_xyz(&self) -> [f32; 3]`; free fns `pub fn identity() -> Mat3`, `pub fn mul_mat3(a: &Mat3, b: &Mat3) -> Mat3`, `pub fn mul_vec3(a: &Mat3, v: &[f32; 3]) -> [f32; 3]`, `pub fn inverse(m: &Mat3) -> Option<Mat3>`, `pub fn diag(v: &[f32; 3]) -> Mat3`; and `#[cfg(test)] pub(crate) fn approx_eq_mat3(a: &Mat3, b: &Mat3, tol: f32) -> bool`.
- Consumes: nothing (leaf task).

- [ ] **Step 1: Add the crate to the workspace**

In root `Cargo.toml`, add `"ferrolite-color"` to `members`, and under `[workspace.dependencies]` add:

```toml
ferrolite-color = { path = "ferrolite-color" }
moxcms = "0.8"
```

(`serde`, `thiserror` already exist in `[workspace.dependencies]`.)

- [ ] **Step 2: Create the crate manifest**

`ferrolite-color/Cargo.toml`:

```toml
[package]
name = "ferrolite-color"
version = "0.0.1"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
moxcms = { workspace = true }
thiserror = { workspace = true }
serde = { workspace = true }
```

- [ ] **Step 3: Create `src/lib.rs` with modules + re-exports**

```rust
//! ferrolite-color — pure, `unsafe`-free color math for ferrolite.
//!
//! Working-space definitions, RGB↔XYZ matrices, Bradford camera→working
//! adaptation, working→display / working→output transforms, sRGB transfer
//! functions, and ICC emit/parse via `moxcms`. No GPU, no UI, no `rawler`.
//! Photo tier (GPL-OK); the whole crate is unit-testable on every OS.

mod adapt;
mod camera;
mod error;
mod icc;
mod matrix;
mod oetf;
mod tail;
mod working_space;

pub use adapt::chromatic_adaptation;
pub use camera::camera_to_working;
pub use error::ColorError;
pub use icc::{emit_icc, parse_icc};
pub use matrix::{diag, identity, inverse, mul_mat3, mul_vec3, Mat3, Xy};
pub use oetf::{srgb_eotf, srgb_oetf};
pub use tail::{working_to_display, working_to_output};
pub use working_space::WorkingSpace;
```

Modules `adapt`, `camera`, `error`, `icc`, `oetf`, `tail`, `working_space` are created in later tasks. To keep Task 1 compiling on its own, create each of those seven files now containing only a placeholder line `// implemented in a later task` **and** comment out every `pub use` line except the `matrix` one. Re-enable each `pub use` in the task that implements its module. (This is the same skeleton pattern Spec 2 Plan 1 used.)

- [ ] **Step 4: Write the failing test for `matrix.rs`**

`ferrolite-color/src/matrix.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_multiplicative_unit() {
        let m: Mat3 = [[2.0, 0.0, 1.0], [0.0, 3.0, 0.0], [4.0, 0.0, 5.0]];
        assert!(approx_eq_mat3(&mul_mat3(&identity(), &m), &m, 1e-6));
        assert!(approx_eq_mat3(&mul_mat3(&m, &identity()), &m, 1e-6));
    }

    #[test]
    fn inverse_round_trips_to_identity() {
        let m: Mat3 = [[2.0, 0.0, 1.0], [0.0, 3.0, 0.0], [4.0, 0.0, 5.0]];
        let inv = inverse(&m).expect("m is invertible");
        assert!(approx_eq_mat3(&mul_mat3(&m, &inv), &identity(), 1e-5));
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        let singular: Mat3 = [[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 0.0, 0.0]];
        assert!(inverse(&singular).is_none());
    }

    #[test]
    fn mul_vec3_matches_hand_computation() {
        let m: Mat3 = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        assert_eq!(mul_vec3(&m, &[1.0, 0.0, -1.0]), [-2.0, -2.0, -2.0]);
    }

    #[test]
    fn xy_to_xyz_normalizes_luminance_to_one() {
        let xyz = (Xy { x: 0.3127, y: 0.3290 }).to_xyz();
        assert!((xyz[1] - 1.0).abs() < 1e-6);
        assert!((xyz[0] - 0.3127 / 0.3290).abs() < 1e-5);
    }
}
```

- [ ] **Step 5: Run tests to verify they fail**

Run: `cargo test -p ferrolite-color matrix`
Expected: FAIL — `Mat3`, `identity`, etc. not defined (compile error).

- [ ] **Step 6: Implement `matrix.rs`**

Prepend above the `#[cfg(test)]` block:

```rust
//! Tiny 3×3 linear-algebra core — generic, no color concepts.

/// Row-major 3×3 matrix.
pub type Mat3 = [[f32; 3]; 3];

/// A CIE 1931 xy chromaticity coordinate (a white point or primary).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xy {
    pub x: f32,
    pub y: f32,
}

impl Xy {
    /// Chromaticity → tristimulus XYZ, normalized so Y = 1.
    pub fn to_xyz(&self) -> [f32; 3] {
        [self.x / self.y, 1.0, (1.0 - self.x - self.y) / self.y]
    }
}

/// The 3×3 identity.
pub fn identity() -> Mat3 {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}

/// Matrix product `a · b`.
#[allow(clippy::needless_range_loop)] // explicit i/j/k indexing is clearest for a fixed 3×3.
pub fn mul_mat3(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut r = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    r
}

/// Matrix–vector product `a · v`.
pub fn mul_vec3(a: &Mat3, v: &[f32; 3]) -> [f32; 3] {
    [
        a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
        a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
        a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
    ]
}

/// Diagonal matrix from a 3-vector.
pub fn diag(v: &[f32; 3]) -> Mat3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

/// Inverse via cofactors; `None` when (near-)singular.
pub fn inverse(m: &Mat3) -> Option<Mat3> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ])
}

/// Test helper: element-wise closeness within `tol`.
#[cfg(test)]
pub(crate) fn approx_eq_mat3(a: &Mat3, b: &Mat3, tol: f32) -> bool {
    (0..3).all(|i| (0..3).all(|j| (a[i][j] - b[i][j]).abs() <= tol))
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p ferrolite-color matrix`
Expected: PASS (5 tests).

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml ferrolite-color/
git commit -m "feat(color): scaffold ferrolite-color crate + Mat3 linear-algebra core"
```

---

### Task 2: `WorkingSpace` + RGB↔XYZ matrices

**Files:**
- Modify: `ferrolite-color/src/working_space.rs`
- Modify: `ferrolite-color/src/lib.rs` (re-enable the `working_space` re-export)

**Interfaces:**
- Consumes: `Mat3`, `Xy`, `mul_mat3`, `mul_vec3`, `diag`, `inverse` (Task 1).
- Produces: `pub enum WorkingSpace { Srgb, AdobeRgb, DisplayP3, Rec2020, ProPhoto }` deriving `Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize` with `#[default]` on `Rec2020` (derive, not a manual impl — a manual `impl Default` here trips `clippy::derivable_impls` under `-D warnings`); methods `pub fn white_point(&self) -> Xy`, `pub fn rgb_to_xyz(&self) -> Mat3`, `pub fn xyz_to_rgb(&self) -> Mat3`, and `pub const ALL: [WorkingSpace; 5]`.

- [ ] **Step 1: Write the failing tests**

`ferrolite-color/src/working_space.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::{approx_eq_mat3, identity, mul_mat3};

    // From rawler-0.7.2/src/imgop/xyz.rs SRGB_TO_XYZ_D65 (Bruce Lindbloom).
    const SRGB_TO_XYZ_D65: Mat3 = [
        [0.4124564, 0.3575761, 0.1804375],
        [0.2126729, 0.7151522, 0.0721750],
        [0.0193339, 0.1191920, 0.9503041],
    ];

    #[test]
    fn default_is_rec2020() {
        assert_eq!(WorkingSpace::default(), WorkingSpace::Rec2020);
    }

    #[test]
    fn srgb_rgb_to_xyz_matches_reference() {
        // Computed from primaries+white; must match the published matrix.
        assert!(approx_eq_mat3(
            &WorkingSpace::Srgb.rgb_to_xyz(),
            &SRGB_TO_XYZ_D65,
            1e-3
        ));
    }

    #[test]
    fn every_space_rgb_to_xyz_inverts_cleanly() {
        for space in WorkingSpace::ALL {
            let round = mul_mat3(&space.xyz_to_rgb(), &space.rgb_to_xyz());
            assert!(
                approx_eq_mat3(&round, &identity(), 1e-4),
                "{space:?} rgb_to_xyz/xyz_to_rgb not inverse"
            );
        }
    }

    #[test]
    fn white_maps_to_white_point_xyz() {
        // rgb_to_xyz * (1,1,1) == white point XYZ (definition of the adaptation).
        for space in WorkingSpace::ALL {
            let got = crate::matrix::mul_vec3(&space.rgb_to_xyz(), &[1.0, 1.0, 1.0]);
            let want = space.white_point().to_xyz();
            assert!(
                (0..3).all(|i| (got[i] - want[i]).abs() < 1e-4),
                "{space:?} white mismatch: got {got:?} want {want:?}"
            );
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-color working_space`
Expected: FAIL — `WorkingSpace` not defined.

- [ ] **Step 3: Implement `working_space.rs`**

Prepend above the test module:

```rust
//! The curated 5 working spaces and their linear RGB↔XYZ matrices, computed
//! from primaries + white point (Bruce Lindbloom's method).

use crate::matrix::{diag, inverse, mul_mat3, mul_vec3, Mat3, Xy};

/// The curated working/output color spaces. Default = linear Rec.2020.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum WorkingSpace {
    Srgb,
    AdobeRgb,
    DisplayP3,
    #[default]
    Rec2020,
    ProPhoto,
}

impl WorkingSpace {
    /// All five spaces, for iteration in tests and UI selectors.
    pub const ALL: [WorkingSpace; 5] = [
        WorkingSpace::Srgb,
        WorkingSpace::AdobeRgb,
        WorkingSpace::DisplayP3,
        WorkingSpace::Rec2020,
        WorkingSpace::ProPhoto,
    ];

    /// RGB primaries (R, G, B) and the reference white point, all as CIE xy.
    fn primaries(&self) -> ([Xy; 3], Xy) {
        let d65 = Xy { x: 0.31271, y: 0.32902 };
        let d50 = Xy { x: 0.34567, y: 0.35850 };
        match self {
            WorkingSpace::Srgb => (
                [
                    Xy { x: 0.640, y: 0.330 },
                    Xy { x: 0.300, y: 0.600 },
                    Xy { x: 0.150, y: 0.060 },
                ],
                d65,
            ),
            WorkingSpace::AdobeRgb => (
                [
                    Xy { x: 0.640, y: 0.330 },
                    Xy { x: 0.210, y: 0.710 },
                    Xy { x: 0.150, y: 0.060 },
                ],
                d65,
            ),
            WorkingSpace::DisplayP3 => (
                [
                    Xy { x: 0.680, y: 0.320 },
                    Xy { x: 0.265, y: 0.690 },
                    Xy { x: 0.150, y: 0.060 },
                ],
                d65,
            ),
            WorkingSpace::Rec2020 => (
                [
                    Xy { x: 0.708, y: 0.292 },
                    Xy { x: 0.170, y: 0.797 },
                    Xy { x: 0.131, y: 0.046 },
                ],
                d65,
            ),
            WorkingSpace::ProPhoto => (
                [
                    Xy { x: 0.7347, y: 0.2653 },
                    Xy { x: 0.1596, y: 0.8404 },
                    Xy { x: 0.0366, y: 0.0001 },
                ],
                d50,
            ),
        }
    }

    /// The space's reference white point (CIE xy).
    pub fn white_point(&self) -> Xy {
        self.primaries().1
    }

    /// Linear RGB → XYZ (under this space's own white point).
    pub fn rgb_to_xyz(&self) -> Mat3 {
        let (p, white) = self.primaries();
        let (xr, xg, xb) = (p[0].to_xyz(), p[1].to_xyz(), p[2].to_xyz());
        // Columns are the primary tristimulus values.
        let m: Mat3 = [
            [xr[0], xg[0], xb[0]],
            [xr[1], xg[1], xb[1]],
            [xr[2], xg[2], xb[2]],
        ];
        let s = mul_vec3(
            &inverse(&m).expect("primaries are linearly independent"),
            &white.to_xyz(),
        );
        mul_mat3(&m, &diag(&s))
    }

    /// XYZ → linear RGB (under this space's own white point).
    pub fn xyz_to_rgb(&self) -> Mat3 {
        inverse(&self.rgb_to_xyz()).expect("rgb_to_xyz is invertible")
    }
}
```

Re-enable in `lib.rs`: `pub use working_space::WorkingSpace;`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-color working_space`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-color/src/working_space.rs ferrolite-color/src/lib.rs
git commit -m "feat(color): WorkingSpace enum + primaries-derived RGB<->XYZ matrices"
```

---

### Task 3: Bradford chromatic adaptation

**Files:**
- Modify: `ferrolite-color/src/adapt.rs`
- Modify: `ferrolite-color/src/lib.rs` (re-enable the `adapt` re-export)

**Interfaces:**
- Consumes: `Mat3`, `Xy`, `mul_mat3`, `mul_vec3`, `diag`, `inverse` (Task 1).
- Produces: `pub fn chromatic_adaptation(src: Xy, dst: Xy) -> Mat3` — the XYZ→XYZ matrix adapting from white point `src` to white point `dst` (Bradford cone response).

- [ ] **Step 1: Write the failing tests**

`ferrolite-color/src/adapt.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::{approx_eq_mat3, identity, mul_mat3, mul_vec3};

    const D65: Xy = Xy { x: 0.31271, y: 0.32902 };
    const D50: Xy = Xy { x: 0.34567, y: 0.35850 };

    #[test]
    fn same_white_is_identity() {
        assert!(approx_eq_mat3(&chromatic_adaptation(D65, D65), &identity(), 1e-5));
    }

    #[test]
    fn adaptation_is_invertible_round_trip() {
        let there = chromatic_adaptation(D50, D65);
        let back = chromatic_adaptation(D65, D50);
        assert!(approx_eq_mat3(&mul_mat3(&back, &there), &identity(), 1e-4));
    }

    #[test]
    fn maps_source_white_onto_destination_white() {
        // Adapting src-white XYZ must yield dst-white XYZ.
        let a = chromatic_adaptation(D50, D65);
        let got = mul_vec3(&a, &D50.to_xyz());
        let want = D65.to_xyz();
        assert!((0..3).all(|i| (got[i] - want[i]).abs() < 1e-4), "got {got:?} want {want:?}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-color adapt`
Expected: FAIL — `chromatic_adaptation` not defined.

- [ ] **Step 3: Implement `adapt.rs`**

Prepend above the test module:

```rust
//! Bradford chromatic adaptation between two white points.

use crate::matrix::{diag, inverse, mul_mat3, mul_vec3, Mat3, Xy};

/// The Bradford cone-response matrix (XYZ → LMS-ish cone space).
const BRADFORD: Mat3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// XYZ→XYZ matrix adapting a color measured under white point `src` to its
/// appearance under white point `dst` (Bradford transform).
pub fn chromatic_adaptation(src: Xy, dst: Xy) -> Mat3 {
    let cone_src = mul_vec3(&BRADFORD, &src.to_xyz());
    let cone_dst = mul_vec3(&BRADFORD, &dst.to_xyz());
    let ratio = [
        cone_dst[0] / cone_src[0],
        cone_dst[1] / cone_src[1],
        cone_dst[2] / cone_src[2],
    ];
    let b_inv = inverse(&BRADFORD).expect("Bradford matrix is invertible");
    mul_mat3(&b_inv, &mul_mat3(&diag(&ratio), &BRADFORD))
}
```

Re-enable in `lib.rs`: `pub use adapt::chromatic_adaptation;`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-color adapt`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-color/src/adapt.rs ferrolite-color/src/lib.rs
git commit -m "feat(color): Bradford chromatic adaptation between white points"
```

---

### Task 4: `camera_to_working` transform

**Files:**
- Modify: `ferrolite-color/src/camera.rs`
- Modify: `ferrolite-color/src/lib.rs` (re-enable the `camera` re-export)

**Interfaces:**
- Consumes: `Mat3`, `Xy`, `mul_mat3`, `inverse`, `identity` (Task 1); `chromatic_adaptation` (Task 3); `WorkingSpace` (Task 2).
- Produces: `pub fn camera_to_working(xyz_to_cam: Mat3, cam_white: Xy, working: WorkingSpace) -> Mat3` — composes camera→XYZ (inverse of the DNG-style XYZ→camera matrix) → Bradford adapt (camera reference white → working white) → XYZ→working-RGB into a single 3×3. Pragmatic single-illuminant (spec §4.2). Never panics: a singular input matrix falls back to `identity()` for the inverse.

- [ ] **Step 1: Write the failing tests**

`ferrolite-color/src/camera.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::approx_eq_mat3;
    use crate::working_space::WorkingSpace;

    const D65: Xy = Xy { x: 0.31271, y: 0.32902 };

    // rawler-0.7.2/src/imgop/xyz.rs XYZ_TO_SRGB_D65 — an "sRGB camera".
    const XYZ_TO_SRGB_D65: Mat3 = [
        [3.2404542, -1.5371385, -0.4985314],
        [-0.9692660, 1.8760108, 0.0415560],
        [0.0556434, -0.2040259, 1.0572252],
    ];

    #[test]
    fn srgb_camera_into_srgb_working_is_identity() {
        // A camera whose XYZ→cam == XYZ→sRGB, into the sRGB working space
        // under the same white, must reduce to identity.
        let m = camera_to_working(XYZ_TO_SRGB_D65, D65, WorkingSpace::Srgb);
        assert!(approx_eq_mat3(&m, &crate::matrix::identity(), 1e-3), "{m:?}");
    }

    #[test]
    fn output_is_finite_for_all_working_spaces() {
        for space in WorkingSpace::ALL {
            let m = camera_to_working(XYZ_TO_SRGB_D65, D65, space);
            assert!(m.iter().flatten().all(|v| v.is_finite()), "{space:?} produced non-finite");
        }
    }

    #[test]
    fn singular_matrix_does_not_panic() {
        let singular: Mat3 = [[0.0; 3]; 3];
        let m = camera_to_working(singular, D65, WorkingSpace::Rec2020);
        assert!(m.iter().flatten().all(|v| v.is_finite()));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-color camera`
Expected: FAIL — `camera_to_working` not defined.

- [ ] **Step 3: Implement `camera.rs`**

Prepend above the test module:

```rust
//! Camera-native RGB → working-space RGB, composed as a single 3×3.

use crate::adapt::chromatic_adaptation;
use crate::matrix::{identity, inverse, mul_mat3, Mat3, Xy};
use crate::working_space::WorkingSpace;

/// Compose `xyz_to_working · adapt(cam_white → working_white) · cam_to_xyz`.
///
/// `xyz_to_cam` is the DNG-style XYZ→camera matrix (as surfaced by
/// `ferrolite-decode`'s `ColorProfile`); `cam_white` is the matrix's reference
/// illuminant white point. Pragmatic single-illuminant transform (spec §4.2);
/// quality is secondary to architecture. A singular `xyz_to_cam` degrades to an
/// identity camera→XYZ rather than panicking.
pub fn camera_to_working(xyz_to_cam: Mat3, cam_white: Xy, working: WorkingSpace) -> Mat3 {
    let cam_to_xyz = inverse(&xyz_to_cam).unwrap_or_else(identity);
    let adapt = chromatic_adaptation(cam_white, working.white_point());
    let xyz_to_working = working.xyz_to_rgb();
    mul_mat3(&xyz_to_working, &mul_mat3(&adapt, &cam_to_xyz))
}
```

Re-enable in `lib.rs`: `pub use camera::camera_to_working;`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-color camera`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-color/src/camera.rs ferrolite-color/src/lib.rs
git commit -m "feat(color): camera->working transform (invert + Bradford + working)"
```

---

### Task 5: Tail transforms (`working→display`, `working→output`) + sRGB≡identity invariant

**Files:**
- Modify: `ferrolite-color/src/tail.rs`
- Modify: `ferrolite-color/src/lib.rs` (re-enable the `tail` re-export)

**Interfaces:**
- Consumes: `Mat3`, `mul_mat3` (Task 1); `chromatic_adaptation` (Task 3); `WorkingSpace` (Task 2).
- Produces: `pub fn working_to_output(working: WorkingSpace, output: WorkingSpace) -> Mat3` — `output.xyz_to_rgb · adapt(working_white → output_white) · working.rgb_to_xyz`; and `pub fn working_to_display(working: WorkingSpace) -> Mat3` = `working_to_output(working, WorkingSpace::Srgb)`. **Load-bearing invariant:** `working_to_display(Srgb)` is the identity 3×3 (this is the seed for Plan 2's "sRGB ≡ old `linear_to_srgb`" GPU regression golden — spec §4.3).

- [ ] **Step 1: Write the failing tests**

`ferrolite-color/src/tail.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::{approx_eq_mat3, identity};
    use crate::working_space::WorkingSpace;

    #[test]
    fn srgb_working_to_display_is_identity() {
        // The regression invariant (spec §4.3): with sRGB working space the tail
        // matrix is identity, so the shader reduces to plain sRGB OETF.
        assert!(approx_eq_mat3(
            &working_to_display(WorkingSpace::Srgb),
            &identity(),
            1e-4
        ));
    }

    #[test]
    fn output_to_same_space_is_identity() {
        for space in WorkingSpace::ALL {
            assert!(
                approx_eq_mat3(&working_to_output(space, space), &identity(), 1e-4),
                "{space:?} -> {space:?} not identity"
            );
        }
    }

    #[test]
    fn all_tails_are_finite() {
        for space in WorkingSpace::ALL {
            assert!(working_to_display(space).iter().flatten().all(|v| v.is_finite()));
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-color tail`
Expected: FAIL — `working_to_display` / `working_to_output` not defined.

- [ ] **Step 3: Implement `tail.rs`**

Prepend above the test module:

```rust
//! Tail transforms: working-space RGB → display or output RGB (the 3×3 matrix;
//! the OETF is applied separately, in-shader for display / at encode for output).

use crate::adapt::chromatic_adaptation;
use crate::matrix::{mul_mat3, Mat3};
use crate::working_space::WorkingSpace;

/// `working` linear RGB → `output` linear RGB, as a single 3×3.
pub fn working_to_output(working: WorkingSpace, output: WorkingSpace) -> Mat3 {
    let adapt = chromatic_adaptation(working.white_point(), output.white_point());
    mul_mat3(&output.xyz_to_rgb(), &mul_mat3(&adapt, &working.rgb_to_xyz()))
}

/// `working` linear RGB → sRGB (D65) display linear RGB, as a single 3×3.
/// The sRGB OETF is applied after this matrix (in-shader). With
/// `WorkingSpace::Srgb` this is exactly the identity (spec §4.3 invariant).
pub fn working_to_display(working: WorkingSpace) -> Mat3 {
    working_to_output(working, WorkingSpace::Srgb)
}
```

Re-enable in `lib.rs`: `pub use tail::{working_to_display, working_to_output};`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-color tail`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-color/src/tail.rs ferrolite-color/src/lib.rs
git commit -m "feat(color): working->display/output tails; sRGB tail == identity invariant"
```

---

### Task 6: sRGB transfer functions (OETF/EOTF)

**Files:**
- Modify: `ferrolite-color/src/oetf.rs`
- Modify: `ferrolite-color/src/lib.rs` (re-enable the `oetf` re-export)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn srgb_oetf(linear: f32) -> f32` (linear → sRGB-encoded) and `pub fn srgb_eotf(encoded: f32) -> f32` (sRGB-encoded → linear), the standard IEC 61966-2.1 piecewise curve. These are the display/output-tail transfer functions; per-space output OETFs (Adobe/ProPhoto gamma, Rec.2020) are deferred to Plan 4's encode path.

- [ ] **Step 1: Write the failing tests**

`ferrolite-color/src/oetf.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_fixed() {
        assert!((srgb_oetf(0.0) - 0.0).abs() < 1e-6);
        assert!((srgb_oetf(1.0) - 1.0).abs() < 1e-5);
        assert!((srgb_eotf(0.0) - 0.0).abs() < 1e-6);
        assert!((srgb_eotf(1.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn oetf_eotf_round_trip() {
        for i in 0..=20 {
            let l = i as f32 / 20.0;
            let round = srgb_eotf(srgb_oetf(l));
            assert!((round - l).abs() < 1e-4, "l={l} round={round}");
        }
    }

    #[test]
    fn linear_segment_near_zero() {
        // Below the knee the curve is exactly 12.92 * linear.
        assert!((srgb_oetf(0.002) - 12.92 * 0.002).abs() < 1e-6);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-color oetf`
Expected: FAIL — `srgb_oetf` / `srgb_eotf` not defined.

- [ ] **Step 3: Implement `oetf.rs`**

Prepend above the test module:

```rust
//! sRGB (IEC 61966-2.1) transfer functions. The display/output tail applies the
//! 3×3 matrix (see `tail`) and then one of these; for the sRGB display path this
//! is the only OETF needed in Plan 1.

/// Linear → sRGB-encoded.
pub fn srgb_oetf(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB-encoded → linear.
pub fn srgb_eotf(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}
```

Re-enable in `lib.rs`: `pub use oetf::{srgb_eotf, srgb_oetf};`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-color oetf`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-color/src/oetf.rs ferrolite-color/src/lib.rs
git commit -m "feat(color): sRGB OETF/EOTF transfer functions"
```

---

### Task 7: ICC emit/parse via moxcms + `ColorError`

**Files:**
- Modify: `ferrolite-color/src/error.rs`
- Modify: `ferrolite-color/src/icc.rs`
- Modify: `ferrolite-color/src/lib.rs` (re-enable the `error` + `icc` re-exports)

**Interfaces:**
- Consumes: `WorkingSpace` (Task 2); `moxcms::ColorProfile`.
- Produces: `pub enum ColorError { Icc(String) }` (`thiserror`); `pub fn emit_icc(space: WorkingSpace) -> Result<Vec<u8>, ColorError>` (standard ICC bytes for the space, via `moxcms`); `pub fn parse_icc(bytes: &[u8]) -> Result<(), ColorError>` (validates that `moxcms` accepts the profile — used to parse an embedded ICC if ever present, spec §4.4).
- **moxcms mapping (verified against moxcms 0.8.1):** `Srgb → ColorProfile::new_srgb()`, `AdobeRgb → new_adobe_rgb()`, `DisplayP3 → new_display_p3()`, `Rec2020 → new_bt2020()`, `ProPhoto → new_pro_photo_rgb()`; `ColorProfile::encode() -> Result<Vec<u8>, moxcms::CmsError>`; `ColorProfile::new_from_slice(&[u8]) -> Result<ColorProfile, moxcms::CmsError>`.

- [ ] **Step 1: Implement `error.rs` (no separate test — exercised by Task 7's icc tests)**

`ferrolite-color/src/error.rs`:

```rust
//! Error type for the color crate.

/// Errors from ICC emit/parse. Pure color math (matrices, adaptation) is
/// infallible and returns values directly.
#[derive(Debug, thiserror::Error)]
pub enum ColorError {
    #[error("ICC profile error: {0}")]
    Icc(String),
}
```

Re-enable in `lib.rs`: `pub use error::ColorError;`.

- [ ] **Step 2: Write the failing tests**

`ferrolite-color/src/icc.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::working_space::WorkingSpace;

    #[test]
    fn emits_valid_icc_for_every_space() {
        for space in WorkingSpace::ALL {
            let bytes = emit_icc(space).unwrap_or_else(|e| panic!("{space:?}: {e}"));
            assert!(bytes.len() > 128, "{space:?}: profile too small ({} bytes)", bytes.len());
            // ICC signature 'acsp' lives at header offset 36..40.
            assert_eq!(&bytes[36..40], b"acsp", "{space:?}: missing ICC 'acsp' signature");
        }
    }

    #[test]
    fn emitted_profile_round_trips_through_parse() {
        for space in WorkingSpace::ALL {
            let bytes = emit_icc(space).expect("emit");
            assert!(parse_icc(&bytes).is_ok(), "{space:?} failed to parse back");
        }
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_icc(&[0u8; 8]).is_err());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ferrolite-color icc`
Expected: FAIL — `emit_icc` / `parse_icc` not defined.

- [ ] **Step 4: Implement `icc.rs`**

Prepend above the test module:

```rust
//! ICC profile emit/parse via `moxcms` (pure Rust). Profiles are emitted for
//! embedding on export; parse validates an embedded profile if one is present.
//! moxcms-only for Plan 1 (the lcms2 fallback is deferred — see the plan's
//! Global Constraints).

use crate::error::ColorError;
use crate::working_space::WorkingSpace;
use moxcms::ColorProfile;

/// Standard ICC profile bytes for `space`, for embedding on export.
pub fn emit_icc(space: WorkingSpace) -> Result<Vec<u8>, ColorError> {
    let profile = match space {
        WorkingSpace::Srgb => ColorProfile::new_srgb(),
        WorkingSpace::AdobeRgb => ColorProfile::new_adobe_rgb(),
        WorkingSpace::DisplayP3 => ColorProfile::new_display_p3(),
        WorkingSpace::Rec2020 => ColorProfile::new_bt2020(),
        WorkingSpace::ProPhoto => ColorProfile::new_pro_photo_rgb(),
    };
    profile.encode().map_err(|e| ColorError::Icc(e.to_string()))
}

/// Validate that `bytes` is a parseable ICC profile (spec §4.4 — parse an
/// embedded ICC if ever present).
pub fn parse_icc(bytes: &[u8]) -> Result<(), ColorError> {
    ColorProfile::new_from_slice(bytes)
        .map(|_| ())
        .map_err(|e| ColorError::Icc(e.to_string()))
}
```

Re-enable in `lib.rs`: `pub use icc::{emit_icc, parse_icc};`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ferrolite-color icc`
Expected: PASS (3 tests). If `new_from_slice` is stricter than `encode` for some space and the round-trip fails, treat it as a real finding: log the failing space and fall back that space's `parse` assertion — but first confirm against moxcms 0.8.1 source (`src/profile.rs:978`, `src/writer.rs:646`) that the emitted tags are readable. Do not weaken the `acsp`/size assertions.

- [ ] **Step 6: Full-crate check + commit**

Run: `cargo test -p ferrolite-color` (all modules) and `cargo clippy -p ferrolite-color --all-targets -- -D warnings`.
Expected: PASS, no warnings.

```bash
git add ferrolite-color/src/error.rs ferrolite-color/src/icc.rs ferrolite-color/src/lib.rs
git commit -m "feat(color): ICC emit/parse for the 5 spaces via moxcms + ColorError"
```

---

### Task 8: `ferrolite-decode` surfaces a camera `ColorProfile`

**Files:**
- Create: `ferrolite-decode/src/color.rs`
- Modify: `ferrolite-decode/src/raw.rs` (add `color_profile` field + populate it)
- Modify: `ferrolite-decode/src/lib.rs` (`mod color;` + `pub use color::ColorProfile;`)

**Interfaces:**
- Consumes: `rawler::imgop::xyz::{Illuminant, FlatColorMatrix}` (public — verified: `rawler/src/lib.rs` `pub mod imgop`, `imgop/mod.rs` `pub mod xyz`); `rawler::rawimage::RawImage.color_matrix: HashMap<Illuminant, FlatColorMatrix>` (XYZ→camera per illuminant).
- Produces: `pub struct ColorProfile { pub xyz_to_cam: [[f32; 3]; 3], pub white_xy: [f32; 2], pub is_fallback: bool }` deriving `Debug, Clone, PartialEq`; `ColorProfile::srgb_fallback() -> ColorProfile`; `ColorProfile::from_color_matrix(&HashMap<Illuminant, FlatColorMatrix>) -> ColorProfile`; `pub fn illuminant_to_xy(illum: Illuminant) -> [f32; 2]`. New field `RawDecoded.color_profile: ColorProfile`. **Downstream contract (Plan 2 glue):** `ferrolite_color::camera_to_working(profile.xyz_to_cam, ferrolite_color::Xy { x: profile.white_xy[0], y: profile.white_xy[1] }, working)`.

- [ ] **Step 1: Write the failing tests**

`ferrolite-decode/src/color.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn srgb_fallback_is_flagged_d65() {
        let p = ColorProfile::srgb_fallback();
        assert!(p.is_fallback);
        assert_eq!(p.white_xy, [0.31271, 0.32902]);
        // First row of XYZ->sRGB(D65).
        assert!((p.xyz_to_cam[0][0] - 3.2404542).abs() < 1e-5);
    }

    #[test]
    fn empty_matrix_map_falls_back() {
        let empty: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        let p = ColorProfile::from_color_matrix(&empty);
        assert!(p.is_fallback);
    }

    #[test]
    fn too_short_matrix_falls_back() {
        let mut m: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        m.insert(Illuminant::D65, vec![1.0, 0.0, 0.0]); // only 3 values
        let p = ColorProfile::from_color_matrix(&m);
        assert!(p.is_fallback);
    }

    #[test]
    fn prefers_d65_and_reshapes_to_3x3() {
        let mut m: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        m.insert(Illuminant::A, vec![9.0; 9]);
        m.insert(Illuminant::D65, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
        let p = ColorProfile::from_color_matrix(&m);
        assert!(!p.is_fallback);
        assert_eq!(p.xyz_to_cam, [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]]);
        assert_eq!(p.white_xy, [0.31271, 0.32902]);
    }

    #[test]
    fn illuminant_to_xy_covers_common_illuminants() {
        assert_eq!(illuminant_to_xy(Illuminant::D50), [0.34567, 0.35850]);
        assert_eq!(illuminant_to_xy(Illuminant::D65), [0.31271, 0.32902]);
        // Unknown illuminants default to D65.
        assert_eq!(illuminant_to_xy(Illuminant::Unknown), [0.31271, 0.32902]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-decode color`
Expected: FAIL — module `color` / `ColorProfile` not defined.

- [ ] **Step 3: Implement `color.rs`**

Prepend above the test module:

```rust
//! Camera color calibration surfaced from `rawler` as a decode product.
//!
//! Additive to the existing `{ PreviewImage, RawImage, Metadata }` products
//! (architecture map §3): `ferrolite-pipeline` (Spec 3 Plan 2) feeds this into
//! `ferrolite-color` to build the camera→working matrix. Never panics — a
//! missing/short matrix logs and falls back to sRGB primaries (spec §6, §10).

use rawler::imgop::xyz::{FlatColorMatrix, Illuminant};
use std::collections::HashMap;

/// Camera color calibration: the DNG-style XYZ→camera 3×3 matrix and the
/// reference illuminant it was calibrated for.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorProfile {
    /// XYZ (reference illuminant) → camera-native linear RGB, row-major 3×3
    /// (DNG `ColorMatrix` convention, as provided by rawler).
    pub xyz_to_cam: [[f32; 3]; 3],
    /// Reference illuminant white point, CIE 1931 xy.
    pub white_xy: [f32; 2],
    /// True when this is the synthetic sRGB fallback (no usable camera matrix).
    pub is_fallback: bool,
}

impl ColorProfile {
    /// sRGB-primaries fallback (XYZ→sRGB, D65) for cameras lacking a usable
    /// matrix. With an sRGB working space this composes to identity downstream.
    pub fn srgb_fallback() -> Self {
        Self {
            xyz_to_cam: [
                [3.2404542, -1.5371385, -0.4985314],
                [-0.9692660, 1.8760108, 0.0415560],
                [0.0556434, -0.2040259, 1.0572252],
            ],
            white_xy: [0.31271, 0.32902], // D65
            is_fallback: true,
        }
    }

    /// Build from rawler's per-illuminant color matrices, preferring D65, then
    /// any present matrix. Falls back to sRGB (logged) when none is usable.
    pub fn from_color_matrix(matrices: &HashMap<Illuminant, FlatColorMatrix>) -> Self {
        let picked = matrices
            .get(&Illuminant::D65)
            .map(|flat| (Illuminant::D65, flat))
            .or_else(|| matrices.iter().next().map(|(illum, flat)| (*illum, flat)));

        match picked {
            Some((illum, flat)) if flat.len() >= 9 => Self {
                xyz_to_cam: [
                    [flat[0], flat[1], flat[2]],
                    [flat[3], flat[4], flat[5]],
                    [flat[6], flat[7], flat[8]],
                ],
                white_xy: illuminant_to_xy(illum),
                is_fallback: false,
            },
            _ => {
                eprintln!(
                    "ferrolite-decode: no usable camera color matrix; using sRGB fallback"
                );
                Self::srgb_fallback()
            }
        }
    }
}

/// Map a rawler illuminant to a CIE 1931 xy white point. Unknown → D65.
pub fn illuminant_to_xy(illum: Illuminant) -> [f32; 2] {
    match illum {
        Illuminant::D50 => [0.34567, 0.35850],
        Illuminant::D55 => [0.33242, 0.34743],
        Illuminant::D75 => [0.29902, 0.31485],
        Illuminant::A | Illuminant::Tungsten => [0.44757, 0.40745],
        Illuminant::B => [0.34842, 0.35161],
        Illuminant::C => [0.31006, 0.31616],
        // D65 and daylight-like illuminants (and anything unmapped) → D65.
        _ => [0.31271, 0.32902],
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-decode color`
Expected: PASS (5 tests).

- [ ] **Step 5: Wire `ColorProfile` into `RawDecoded`**

In `ferrolite-decode/src/lib.rs`, add the module + re-export near the other `mod`/`pub use` lines:

```rust
mod color;
```
```rust
pub use color::ColorProfile;
```

In `ferrolite-decode/src/raw.rs`, add the import at the top:

```rust
use crate::color::ColorProfile;
```

Add the field to the `RawDecoded` struct (after `wb_coeffs`):

```rust
    /// Camera color calibration (XYZ→camera matrix + reference white). Additive
    /// decode product; consumed by `ferrolite-pipeline` via `ferrolite-color`.
    pub color_profile: ColorProfile,
```

Populate it in `decode_full`'s returned `Ok(RawDecoded { ... })` (after `wb_coeffs`):

```rust
        color_profile: ColorProfile::from_color_matrix(&img.color_matrix),
```

Note: `img` is the `rawler` `RawImage` already bound in `decode_full`; `img.color_matrix` is the `HashMap<Illuminant, FlatColorMatrix>` field (verified `rawler/src/rawimage.rs:239`).

- [ ] **Step 6: Extend the fixture-gated integration test**

In `ferrolite-decode/src/raw.rs`, inside the existing `decode_full_surfaces_cfa_and_levels` test (which already early-returns when the fixture is absent), add after the existing assertions:

```rust
        // Color profile is always present (real matrix or sRGB fallback), finite.
        assert!(
            d.color_profile
                .xyz_to_cam
                .iter()
                .flatten()
                .all(|v| v.is_finite()),
            "color profile matrix must be finite"
        );
        assert!(d.color_profile.white_xy.iter().all(|v| *v > 0.0));
```

- [ ] **Step 7: Run the decode crate tests + clippy**

Run: `cargo test -p ferrolite-decode` and `cargo clippy -p ferrolite-decode --all-targets -- -D warnings`.
Expected: PASS, no warnings. (The fixture-gated test skips or runs depending on whether `../fixtures/raw/sample.rw2` exists — either is green.)

- [ ] **Step 8: Commit**

```bash
git add ferrolite-decode/src/color.rs ferrolite-decode/src/raw.rs ferrolite-decode/src/lib.rs
git commit -m "feat(decode): surface camera ColorProfile (XYZ->cam + white) with sRGB fallback"
```

---

### Task 9: Workspace gate + hold for review

**Files:** none (verification only).

- [ ] **Step 1: Format check**

Run: `cargo fmt --check`
Expected: no output (clean). If it reports diffs, run `cargo fmt` and commit as `chore: cargo fmt`.

- [ ] **Step 2: Clippy across the workspace**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings, exit 0.

- [ ] **Step 3: Full test suite**

Run: `cargo test --workspace`
Expected: all green. `ferrolite-color` runs ~21 pure CPU tests; `ferrolite-decode` color tests run everywhere, the RAW-fixture test skips when the fixture is absent.

- [ ] **Step 4: STOP — hold for the author's review**

Per CLAUDE.md, a green gate is necessary but not sufficient. This plan ships no runnable UI, so present the two crates (`ferrolite-color` public API + the decode `ColorProfile` seam) and **wait for Jann's review** before finishing the branch or starting Plan 2. Do not merge; do not proceed to Plan 2 without the go-ahead.

---

## Self-Review

**Spec coverage (spec §4 + §6 + §11 + §12 item 1):**
- §4.1 working spaces (curated 5, default Rec.2020, primaries+white → RGB→XYZ + inverse) → Task 2. ✅
- §4.2 camera→working (invert matrix, Bradford to working white, single 3×3; sRGB-primaries fallback) → Task 4 + Task 8 (`srgb_fallback`). ✅
- §4.3 tail transforms (`working→display`, `working→output`; sRGB≡identity invariant) → Task 5. ✅ (The GPU golden proving sRGB ≡ `linear_to_srgb` is Plan 2; Task 5 asserts the matrix-level identity that seeds it.)
- §4.4 ICC emit/parse via moxcms → Task 7. lcms2 fallback deferred (recorded in Global Constraints). ✅
- §6 decode `ColorProfile` (camera matrix + illuminant/white; additive; fallback, never panics) → Task 8. ✅
- §11 pure-CPU tests: RGB↔XYZ round-trips + known-value primaries (T2), Bradford (T3), camera→working for a known matrix (T4), `working→display` identity when sRGB (T5), ICC emit round-trip (T7), matrix-fallback selection present/absent (T8) → all covered. ✅
- Bradford (§4.2) as its own tested unit → Task 3. ✅
- sRGB OETF/EOTF (transfer half of the display/output tail, §4.3) → Task 6. ✅

**Explicitly out of Plan 1 (deferred to later Spec-3 plans, per §12):** `ColorMatrixNode`, display/blit shader tail, working-space UI selector, resizable panels (Plan 2); histogram + before/after (Plan 3); `ferrolite-export`, encoders, per-space output OETFs (Plan 4); `Module::Export`, `export_queue` (Plan 5). No GPU/UI here.

**Placeholder scan:** every code step contains complete, compiling code; every run step names an exact command + expected result. No TBD/TODO/"handle errors appropriately". ✅

**Type consistency:** `Mat3 = [[f32;3];3]` and `Xy { x, y }` are defined in Task 1 and used verbatim in Tasks 2–5. `WorkingSpace::ALL` (Task 2) is used in Tasks 2/4/5/7 tests. `camera_to_working(Mat3, Xy, WorkingSpace)` (Task 4) matches the decode→pipeline glue note in Task 8. `ColorProfile.xyz_to_cam: [[f32;3];3]` / `white_xy: [f32;2]` (Task 8) match that glue signature. moxcms method names (`new_srgb`/`new_adobe_rgb`/`new_display_p3`/`new_bt2020`/`new_pro_photo_rgb`/`encode`/`new_from_slice`) verified against moxcms 0.8.1 source. rawler `color_matrix: HashMap<Illuminant, FlatColorMatrix>` and the `rawler::imgop::xyz` path verified against rawler 0.7.2 source. ✅
