# P2 Plan 4 — RCD demosaic (CPU reference) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a full-resolution CPU demosaic (`Rcd`) behind the existing `DemosaicToRgb16f` trait that resolves more detail than the half-res `QuadBin`, falls back to `QuadBin` for non-RGGB sensors, carries values unclamped, and serves as the golden reference for Plan 5's GPU RCD pass.

**Architecture:** A compact "RCD-family" demosaic (ratio/gradient-corrected directional): **Hamilton-Adams directional green** interpolation (horizontal-vs-vertical estimate with a same-colour Laplacian correction) followed by **constant-hue (colour-difference) red/blue** interpolation. RGGB only; every other CFA pattern / non-Bayer sensor delegates to the existing `QuadBin` path (spec §5.2). Output is display-linear, white-balanced, and **unclamped** (P2 §5.3). Parallelised per output row with `rayon`, bit-identical to serial. Wired into the export full-res path as the reachable comparison surface (on-screen 1:1 RCD is Plan 5's GPU work).

**Tech Stack:** Rust, `rayon` (already a `ferrolite-decode` dep, used by `QuadBin`). Crates touched: `ferrolite-decode` (new demosaic), `ferrolite-app` (export wiring). No new dependencies. Depends on Plans 1–3 (merged): reuses the unclamped-carry convention (no `[0,1]` clamp) established in Plan 3.

## Global Constraints

- **RGGB only; fall back otherwise (spec §5.2 / §2).** `Rcd` demosaics the RGGB pattern `[0,1,1,2]`. Any other `cfa_pattern` (BGGR/GRBG/GBRG, X-Trans, non-Bayer) delegates to `QuadBin` — the existing path. (Handling the other three Bayer phases is a straightforward future extension, explicitly out of scope here.)
- **Unclamped carry (P2 §5.3).** RCD never clamps to `[0,1]`; highlights >1 and wide-gamut/negative overshoots flow to the RGBA16F working buffer. The only floor is the per-sample **black-level** floor `.max(0.0)` on the normalized CFA (a black point, matching `QuadBin`, NOT a gamut clip).
- **Golden reference for Plan 5.** This CPU impl *defines* "RCD" for the codebase; Plan 5's WGSL pass is validated against it within tolerance. Keep it deterministic and self-contained.
- **Serial ≡ parallel, bit-identical.** Each output pixel is a pure function of the normalized CFA + interpolated green; the rayon per-row path must be byte-for-byte identical to a serial computation (QuadBin precedent).
- **`wide` SIMD is deferred (spec §8).** Scalar + rayon only in this plan; SIMD is a later perf follow-up.
- **Never block the UI thread (CLAUDE.md §1).** RCD is a library function; the only call site added here is the export **worker thread** (background job), never the UI/update thread.
- **Photo tier only.** `ferrolite-decode`, `ferrolite-app`. No engine-tier (`ferrolite-vt`) changes. RCD reference (RawTherapee/darktable, GPL-3) ports into photo-tier (GPL-3 binary) — fine (map §3).
- **Gate (per branch):** `cargo fmt --check` + `cargo clippy --workspace --all-targets --all-features -- -D warnings` + `cargo test --workspace` green → **then STOP and hold for the author's (Jann's) visual test** (CLAUDE.md "Finishing a branch"). This plan HAS a visual test (below).

---

## File Structure

- `ferrolite-decode/src/rcd.rs` **(new)** — the `Rcd` struct + `DemosaicToRgb16f` impl, the private `demosaic_rggb` / `interpolate_green` / `reconstruct_rgb` / `sample` helpers, and the §10 unit tests. One responsibility: full-res RGGB demosaic.
- `ferrolite-decode/src/lib.rs` **(modify)** — declare `mod rcd;`, re-export `Rcd`.
- `ferrolite-app/src/export/batch.rs` **(modify)** — full-res RAW export uses `Rcd` instead of `QuadBin` (the reachable "force CPU RCD" surface).

---

## Task 1: The `Rcd` CPU demosaic (`ferrolite-decode`)

**Files:**
- Create: `ferrolite-decode/src/rcd.rs`
- Modify: `ferrolite-decode/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `ferrolite-decode/src/rcd.rs`

**Interfaces:**
- Consumes: `crate::demosaic::{DemosaicToRgb16f, DemosaicParams, QuadBin}`, `crate::raw::RawDecoded`, `ferrolite_image::LinearRgbaF32`, `rayon`.
- Produces: `pub struct Rcd;` implementing `DemosaicToRgb16f` → `to_linear_rgba_f32(&self, raw: &RawDecoded) -> LinearRgbaF32`. Full-res (W×H) for RGGB; delegates to `QuadBin` (half-res) otherwise. White-balanced, display-linear, unclamped.

- [ ] **Step 1: Write the failing tests**

Create `ferrolite-decode/src/rcd.rs`:

```rust
//! Full-resolution "RCD-family" Bayer demosaic (ratio/gradient-corrected
//! directional): Hamilton-Adams directional green interpolation + constant-hue
//! (colour-difference) red/blue interpolation. RGGB only; other CFA patterns and
//! non-Bayer sensors fall back to the half-res `QuadBin` path (spec §5.2). Output
//! is display-linear, white-balanced, and UNCLAMPED (carries highlights >1 and
//! wide-gamut negatives — P2 §5.3). This CPU impl is the golden reference the
//! Plan-5 WGSL RCD pass is validated against. Parallelised per output row with
//! rayon; bit-identical to serial. (`wide` SIMD is a deferred perf follow-up — §8.)

use crate::demosaic::{DemosaicParams, DemosaicToRgb16f, QuadBin};
use crate::raw::RawDecoded;
use ferrolite_image::LinearRgbaF32;
use rayon::prelude::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an RGGB `RawDecoded`: black 0, white 65535, given WB, `pixels` row-major.
    fn raw_rggb(w: u32, h: u32, pixels: Vec<u16>, wb: [f32; 4]) -> RawDecoded {
        assert_eq!(pixels.len(), (w * h) as usize);
        RawDecoded {
            width: w,
            height: h,
            cpp: 1,
            pixels,
            cfa_pattern: [0, 1, 1, 2],
            black_levels: [0.0; 4],
            white_level: 65535.0,
            wb_coeffs: wb,
            color_profile: crate::color::ColorProfile::srgb_fallback(),
            orientation: ferrolite_image::Orientation::Normal,
        }
    }

    #[test]
    fn rcd_is_full_resolution() {
        let raw = raw_rggb(8, 6, vec![1000u16; 48], [1.0; 4]);
        let out = Rcd.to_linear_rgba_f32(&raw);
        assert_eq!((out.width, out.height), (8, 6), "RCD is full-res (not half like QuadBin)");
        assert_eq!(out.pixels.len(), LinearRgbaF32::expected_len(8, 6));
    }

    #[test]
    fn rcd_flat_field_reconstructs_uniform_after_wb() {
        // A uniform sensor → every output pixel is the same WB'd value on each channel.
        let raw = raw_rggb(8, 8, vec![30000u16; 64], [1.0, 1.0, 1.0, 1.0]);
        let out = Rcd.to_linear_rgba_f32(&raw);
        let v = 30000.0 / 65535.0;
        for i in 0..64 {
            for c in 0..3 {
                assert!((out.pixels[i * 4 + c] - v).abs() < 1e-4, "px {i} ch {c} = {}", out.pixels[i * 4 + c]);
            }
        }
    }

    #[test]
    fn rcd_reconstructs_neutral_horizontal_ramp() {
        // Neutral scene ramp: every pixel samples the same underlying value s(x)=x*1000,
        // so a correct demosaic yields R≈G≈B≈s(x)/white at interior pixels (exact for a
        // linear ramp under Hamilton-Adams + constant-hue; borders excluded).
        let (w, h) = (16u32, 16u32);
        let pixels: Vec<u16> = (0..w * h).map(|i| ((i % w) as u16) * 1000).collect();
        let out = Rcd.to_linear_rgba_f32(&raw_rggb(w, h, pixels, [1.0; 4]));
        for y in 2..(h - 2) {
            for x in 2..(w - 2) {
                let want = (x as f32 * 1000.0) / 65535.0;
                let i = (y * w + x) as usize;
                for c in 0..3 {
                    assert!(
                        (out.pixels[i * 4 + c] - want).abs() < 1e-4,
                        "interior px ({x},{y}) ch {c}: want {want} got {}",
                        out.pixels[i * 4 + c]
                    );
                }
            }
        }
    }

    #[test]
    fn rcd_preserves_values_above_one() {
        // Bright field + WB > 1 pushes the red channel past 1.0; RCD must carry it.
        let raw = raw_rggb(6, 6, vec![65535u16; 36], [2.0, 1.0, 1.0, 1.0]);
        let out = Rcd.to_linear_rgba_f32(&raw);
        // Pixel 0 is an R site: R = 1.0 * wb_R(2.0) = 2.0, carried unclamped.
        assert!((out.pixels[0] - 2.0).abs() < 1e-4, "R must carry >1 (got {})", out.pixels[0]);
    }

    #[test]
    fn rcd_non_rggb_falls_back_to_quadbin() {
        // A BGGR sensor is not handled by RCD → delegates to QuadBin (half-res).
        let mut raw = raw_rggb(8, 8, (0..64).map(|i| (i * 100) as u16).collect(), [1.3, 1.0, 1.1, 1.0]);
        raw.cfa_pattern = [2, 1, 1, 0]; // BGGR
        let rcd_out = Rcd.to_linear_rgba_f32(&raw);
        let qb_out = QuadBin.to_linear_rgba_f32(&raw);
        assert_eq!(
            (rcd_out.width, rcd_out.height),
            (qb_out.width, qb_out.height),
            "non-RGGB falls back to half-res QuadBin (4x4, not 8x8)"
        );
        assert_eq!(rcd_out.pixels, qb_out.pixels, "fallback returns exactly QuadBin output");
    }

    #[test]
    fn rcd_parallel_matches_serial_reference() {
        // 256x256 output ≥ PARALLEL_MIN_PIXELS exercises the rayon path. Recompute
        // every pixel serially (same core helpers) and require bit-identical output,
        // proving per-row parallelism doesn't reorder/corrupt.
        let (w, h) = (256u32, 256u32);
        let pixels: Vec<u16> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                ((x.wrapping_mul(7) + y.wrapping_mul(13) + x * y) % 4001) as u16
            })
            .collect();
        let wb = [1.8, 1.0, 1.4, 1.0];
        let raw = raw_rggb(w, h, pixels, wb);
        let out = Rcd.to_linear_rgba_f32(&raw); // parallel (above threshold)

        let (wu, hu) = (w as usize, h as usize);
        let p = DemosaicParams::from_raw(&raw);
        let span = (p.white_level - p.black_levels[0]).max(1.0);
        let c: Vec<f32> = (0..wu * hu)
            .map(|i| {
                let (x, y) = (i % wu, i / wu);
                let pos = (y % 2) * 2 + (x % 2);
                ((raw.pixels[i] as f32 - p.black_levels[pos]) / span).max(0.0)
            })
            .collect();
        let green = interpolate_green(&c, wu, hu);
        let mut expected = vec![0.0f32; LinearRgbaF32::expected_len(w, h)];
        for y in 0..hu {
            for x in 0..wu {
                let (r, g, b) = reconstruct_rgb(&c, &green, wu, hu, x, y);
                let base = (y * wu + x) * 4;
                expected[base] = r * p.wb_coeffs[0];
                expected[base + 1] = g * p.wb_coeffs[1];
                expected[base + 2] = b * p.wb_coeffs[2];
                expected[base + 3] = 1.0;
            }
        }
        assert_eq!(out.pixels, expected, "parallel output must be bit-identical to serial");
    }
}
```

Add to `ferrolite-decode/src/lib.rs`: insert `mod rcd;` next to the other module declarations (e.g. after `mod raw;`), and change:

```rust
pub use demosaic::{DemosaicParams, DemosaicToRgb16f, QuadBin};
```
to:
```rust
pub use demosaic::{DemosaicParams, DemosaicToRgb16f, QuadBin};
pub use rcd::Rcd;
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-decode rcd::`
Expected: FAIL — `cannot find type/value Rcd` / `cannot find function interpolate_green` / `reconstruct_rgb` (nothing implemented yet).

- [ ] **Step 3: Write the implementation**

Insert into `ferrolite-decode/src/rcd.rs`, between the `use` block and the `#[cfg(test)]` module:

```rust
/// Below this output pixel count, run serially (rayon overhead not worth it).
/// Mirrors `QuadBin`'s threshold.
const PARALLEL_MIN_PIXELS: u64 = 65_536;

/// The RGGB CFA pattern (the only pattern RCD handles; others fall back).
const RGGB: [u8; 4] = [0, 1, 1, 2];

/// Full-res "RCD-family" demosaic; delegates to `QuadBin` for non-RGGB sensors.
pub struct Rcd;

impl DemosaicToRgb16f for Rcd {
    fn to_linear_rgba_f32(&self, raw: &RawDecoded) -> LinearRgbaF32 {
        if raw.cfa_pattern != RGGB {
            // Non-RGGB / X-Trans / non-Bayer: the existing half-res path (spec §5.2).
            return QuadBin.to_linear_rgba_f32(raw);
        }
        demosaic_rggb(raw)
    }
}

fn demosaic_rggb(raw: &RawDecoded) -> LinearRgbaF32 {
    let w = raw.width as usize;
    let h = raw.height as usize;
    let p = DemosaicParams::from_raw(raw);
    let span = (p.white_level - p.black_levels[0]).max(1.0);

    // Black-subtracted, normalized single-channel CFA (NOT white-balanced yet —
    // WB is applied to the interpolated output so interpolation runs on
    // sensor-linear values). Floor at 0 is the black point (not a gamut clip).
    let c: Vec<f32> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let pos = (y % 2) * 2 + (x % 2);
            ((raw.pixels[i] as f32 - p.black_levels[pos]) / span).max(0.0)
        })
        .collect();

    // Pass 1: green at every pixel (measured at G sites; directional at R/B).
    let green = interpolate_green(&c, w, h);

    // Pass 2: full RGB per pixel via colour-difference chroma, then apply WB.
    // Each output pixel is a pure function of `c` and `green`, so the per-row
    // rayon path is bit-identical to serial.
    let mut out = vec![0.0f32; LinearRgbaF32::expected_len(raw.width, raw.height)];
    let row_stride = w * 4;
    let fill_row = |y: usize, row: &mut [f32]| {
        for x in 0..w {
            let (r, g, b) = reconstruct_rgb(&c, &green, w, h, x, y);
            let base = x * 4;
            row[base] = r * p.wb_coeffs[0];
            row[base + 1] = g * p.wb_coeffs[1];
            row[base + 2] = b * p.wb_coeffs[2];
            row[base + 3] = 1.0;
        }
    };
    let total = (w as u64) * (h as u64);
    if total >= PARALLEL_MIN_PIXELS {
        out.par_chunks_mut(row_stride)
            .enumerate()
            .for_each(|(y, row)| fill_row(y, row));
    } else {
        for (y, row) in out.chunks_mut(row_stride).enumerate() {
            fill_row(y, row);
        }
    }
    LinearRgbaF32::new(raw.width, raw.height, out).expect("rcd length matches dims")
}

/// Read the normalized CFA at `(x, y)` with edge-replication clamping.
#[inline]
fn sample(c: &[f32], w: usize, h: usize, x: i32, y: i32) -> f32 {
    let xc = x.clamp(0, w as i32 - 1) as usize;
    let yc = y.clamp(0, h as i32 - 1) as usize;
    c[yc * w + xc]
}

/// Directional green at every pixel: measured at G sites; Hamilton-Adams
/// horizontal-vs-vertical estimate (bilinear green + same-colour Laplacian
/// correction) at R and B sites, choosing the lower-gradient direction.
fn interpolate_green(c: &[f32], w: usize, h: usize) -> Vec<f32> {
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            let pos = ((y as usize) % 2) * 2 + ((x as usize) % 2);
            if pos == 1 || pos == 2 {
                return c[i]; // G site: measured green
            }
            let s = |dx: i32, dy: i32| sample(c, w, h, x + dx, y + dy);
            let center = s(0, 0);
            let gh = (s(-1, 0) - s(1, 0)).abs() + (2.0 * center - s(-2, 0) - s(2, 0)).abs();
            let gv = (s(0, -1) - s(0, 1)).abs() + (2.0 * center - s(0, -2) - s(0, 2)).abs();
            let gh_est = 0.5 * (s(-1, 0) + s(1, 0)) + 0.25 * (2.0 * center - s(-2, 0) - s(2, 0));
            let gv_est = 0.5 * (s(0, -1) + s(0, 1)) + 0.25 * (2.0 * center - s(0, -2) - s(0, 2));
            if gh < gv {
                gh_est
            } else if gv < gh {
                gv_est
            } else {
                0.5 * (gh_est + gv_est)
            }
        })
        .collect()
}

/// Full sensor-linear (pre-WB) `(R, G, B)` at `(x, y)` from the normalized CFA
/// and the interpolated green, via constant-hue (colour-difference) chroma.
fn reconstruct_rgb(c: &[f32], green: &[f32], w: usize, h: usize, x: usize, y: usize) -> (f32, f32, f32) {
    let (xi, yi) = (x as i32, y as i32);
    let pos = (y % 2) * 2 + (x % 2);
    let cs = |dx: i32, dy: i32| sample(c, w, h, xi + dx, yi + dy);
    let gs = |dx: i32, dy: i32| sample(green, w, h, xi + dx, yi + dy);
    let g_here = green[y * w + x];
    match pos {
        0 => {
            // R site: R measured; B from the 4 diagonal B neighbours (colour diff).
            let r = cs(0, 0);
            let b = g_here
                + 0.25
                    * ((cs(-1, -1) - gs(-1, -1))
                        + (cs(1, -1) - gs(1, -1))
                        + (cs(-1, 1) - gs(-1, 1))
                        + (cs(1, 1) - gs(1, 1)));
            (r, g_here, b)
        }
        3 => {
            // B site: B measured; R from the 4 diagonal R neighbours (colour diff).
            let b = cs(0, 0);
            let r = g_here
                + 0.25
                    * ((cs(-1, -1) - gs(-1, -1))
                        + (cs(1, -1) - gs(1, -1))
                        + (cs(-1, 1) - gs(-1, 1))
                        + (cs(1, 1) - gs(1, 1)));
            (r, g_here, b)
        }
        1 => {
            // G site (even row, odd col): R horizontal neighbours, B vertical.
            let g = cs(0, 0);
            let r = g + 0.5 * ((cs(-1, 0) - gs(-1, 0)) + (cs(1, 0) - gs(1, 0)));
            let b = g + 0.5 * ((cs(0, -1) - gs(0, -1)) + (cs(0, 1) - gs(0, 1)));
            (r, g, b)
        }
        _ => {
            // pos == 2: G site (odd row, even col): B horizontal, R vertical.
            let g = cs(0, 0);
            let b = g + 0.5 * ((cs(-1, 0) - gs(-1, 0)) + (cs(1, 0) - gs(1, 0)));
            let r = g + 0.5 * ((cs(0, -1) - gs(0, -1)) + (cs(0, 1) - gs(0, 1)));
            (r, g, b)
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-decode rcd::`
Expected: PASS (6 tests: full-res dims, flat field, neutral ramp, >1 preservation, non-RGGB fallback, serial≡parallel).

- [ ] **Step 5: Confirm clippy is clean for the new code**

Run: `cargo clippy -p ferrolite-decode --all-targets --all-features -- -D warnings`
Expected: no warnings. (The repo CI gate is `-D warnings --all-features`; the multi-use `cs`/`gs`/`s` closures are called several times each, so no `redundant_closure` lint fires.)

- [ ] **Step 6: Commit**

```bash
git add ferrolite-decode/src/rcd.rs ferrolite-decode/src/lib.rs
git commit -m "feat(decode): RCD-family full-res CPU demosaic (RGGB; QuadBin fallback)"
```

---

## Task 2: Wire the export full-res path to `Rcd` (`ferrolite-app`)

**Files:**
- Modify: `ferrolite-app/src/export/batch.rs` (import line ~13; the RAW demosaic call ~140)

**Interfaces:**
- Consumes: `ferrolite_decode::Rcd` (Task 1), `DemosaicToRgb16f` (existing).
- Produces: full-res RAW exports now use `Rcd` (full resolution) instead of `QuadBin` (half resolution); non-RGGB sensors transparently fall back inside `Rcd`.

> **TDD note:** This is a one-line export wiring swap over the already-unit-tested `Rcd` (Task 1). It is not independently unit-testable (export runs a GPU render on a worker thread). Per CLAUDE.md its correctness is confirmed by `cargo build` + the workspace gate + the **author's visual test** (Task 3). Full before/after shown — no placeholders.

- [ ] **Step 1: Swap the RAW demosaic in `run_one`**

In `ferrolite-app/src/export/batch.rs`, replace the RAW branch's demosaic call (~line 140):

```rust
        FileKind::Raw => match ferrolite_decode::decode_full(&item.path) {
            Ok(raw) => {
                let profile = raw.color_profile.clone();
                (QuadBin.to_linear_rgba_f32(&raw), profile)
            }
```

with (full-res RCD for the export quality tier — spec §5.2; non-RGGB falls back to QuadBin inside `Rcd`):

```rust
        FileKind::Raw => match ferrolite_decode::decode_full(&item.path) {
            Ok(raw) => {
                let profile = raw.color_profile.clone();
                (Rcd.to_linear_rgba_f32(&raw), profile)
            }
```

- [ ] **Step 2: Update the import**

In `ferrolite-app/src/export/batch.rs`, change the import (~line 13):

```rust
use ferrolite_decode::{ColorProfile, DemosaicToRgb16f, QuadBin};
```
to (drop the now-unused `QuadBin`, add `Rcd`; `DemosaicToRgb16f` stays for the `.to_linear_rgba_f32` method):

```rust
use ferrolite_decode::{ColorProfile, DemosaicToRgb16f, Rcd};
```

- [ ] **Step 3: Verify it compiles with no unused-import warning**

Run: `cargo clippy -p ferrolite-app --all-targets --all-features -- -D warnings 2>&1 | tail -20`
Expected: clean (no `unused_imports` for `QuadBin`, no other warnings). If `QuadBin` is still referenced elsewhere in `batch.rs`, keep it in the import — but the grep in Step 4 confirms it is not.

- [ ] **Step 4: Confirm `QuadBin` is no longer used in this file**

Run: `grep -n 'QuadBin' ferrolite-app/src/export/batch.rs`
Expected: no matches (the only use was the one just swapped).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/export/batch.rs
git commit -m "feat(app): export full-res RAW via RCD demosaic (was half-res QuadBin)"
```

---

## Task 3: Workspace green gate + visual-test handoff

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all && cargo fmt --check`
Expected: no diff.

- [ ] **Step 2: Clippy (CI-equivalent gate)**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: no warnings/errors.

- [ ] **Step 3: Full workspace test**

Run: `cargo test --workspace`
Expected: PASS — the six new `rcd::` tests plus all existing tests (no existing test depends on export using QuadBin; the `QuadBin` demosaic itself is unchanged and still covered by its own tests).

- [ ] **Step 4: Commit any formatting fixups**

```bash
git add -A && git commit -m "chore: cargo fmt for P2 plan 4" || echo "nothing to format"
```

- [ ] **Step 5: STOP — hand the author the visual test**

Per CLAUDE.md "Finishing a branch", the gate is necessary but not sufficient. Present this and **hold** for Jann's hands-on results before merging/PR-ing:

**Visual test (real surface this plan):**
1. **Open a RGGB RAW** (e.g. the repo fixture `fixtures/raw/sample.rw2`, confirmed RGGB) in the app. Note the on-screen detail (the on-screen tiers are still QuadBin/half-res — RCD on screen is Plan 5).
2. **Export it** (File/Export → any format, e.g. PNG/JPEG, full size). The export now runs **full-res CPU RCD**.
3. **Open the exported file and zoom to 100%.** Compare its fine detail against the app's on-screen 1:1 view (which is still half-res QuadBin).
   - **Expected:** the exported RCD image resolves **more fine detail** and shows **fewer stair-step/zipper artifacts** on diagonal edges than the half-res QuadBin view; the exported image's **pixel dimensions are the full sensor resolution** (roughly 2× per side vs the QuadBin half-res path). Colour/brightness should be unchanged (same camera→working + op stack; only the demosaic changed).
   - **Failure signatures:** the export looks **soft/mushy or identical to half-res** (RCD not engaged); **maze/labyrinth or zipper artifacts** worse than QuadBin (directional green mis-selecting); a **colour cast or wrong colours** vs the on-screen view (chroma reconstruction bug); export **crash/hang** (should never — it runs on the export worker thread).
4. **Non-RGGB sanity (optional):** if you have a non-RGGB RAW (e.g. an X-Trans Fuji or a BGGR camera), export it — it should still succeed, transparently falling back to QuadBin (no crash, no regression).

Do NOT merge/PR until Jann confirms the exported RCD image shows more detail than QuadBin with no new artifacts or colour shift. Address any issue found, then re-run the gate.

---

## Self-Review

**Spec coverage (§5.2 + §10 + §2):**
- "CPU RCD behind `DemosaicToRgb16f`" → Task 1 (`Rcd` impl of the trait).
- "rayon; `wide` SIMD optional/deferrable" → Task 1 uses rayon per-row; SIMD explicitly deferred (Global Constraints + module doc).
- "non-RGGB/X-Trans fall back to the existing path" → Task 1 `Rcd::to_linear_rgba_f32` delegates to `QuadBin` for `cfa_pattern != [0,1,1,2]`; `rcd_non_rggb_falls_back_to_quadbin` test.
- "golden reference for Plan 5" → deterministic, self-contained; the private `interpolate_green`/`reconstruct_rgb` are the exact math Plan 5's WGSL reproduces.
- §10 CPU tests: correctness on synthetic CFA (`rcd_reconstructs_neutral_horizontal_ramp`, `rcd_flat_field_reconstructs_uniform_after_wb`); preserves values >1 (`rcd_preserves_values_above_one`); non-RGGB fallback (`rcd_non_rggb_falls_back_to_quadbin`); serial-vs-parallel bit-identity (`rcd_parallel_matches_serial_reference`). ✓
- §5.3 carry-unclamped preserved (no `[0,1]` clamp; only the black-level `.max(0.0)` floor, as `QuadBin`). ✓
- Visual test "force CPU RCD → full-res detail vs QuadBin" → Task 2 wires export to `Rcd`; Task 3 Step 5 checklist. ✓
- Out of scope (correct): X-Trans/Markesteijn, other Bayer phases, GPU/two-tier/1:1-on-screen (Plan 5), SIMD.

**Placeholder scan:** No TBD/TODO/"handle edge cases"; every code step shows complete code; every run step states expected output; the one non-TDD task (Task 2) is a shown one-line swap, justified as export glue, gated by build + visual test.

**Type consistency:** `Rcd` implements `DemosaicToRgb16f::to_linear_rgba_f32(&self, &RawDecoded) -> LinearRgbaF32` (matches the trait and the `QuadBin` precedent). `interpolate_green(&[f32], usize, usize) -> Vec<f32>` and `reconstruct_rgb(&[f32], &[f32], usize, usize, usize, usize) -> (f32,f32,f32)` are used identically in `demosaic_rggb` and the serial-reference test. `DemosaicParams::from_raw` + `.black_levels`/`.white_level`/`.wb_coeffs` match the existing `QuadBin` usage. Export call site uses `Rcd.to_linear_rgba_f32(&raw)` matching the trait method.
