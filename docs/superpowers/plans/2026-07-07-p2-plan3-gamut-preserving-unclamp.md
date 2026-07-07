# P2 Plan 3 — Gamut-preserving unclamp Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop destroying out-of-range values before the display/output tail — remove the demosaic `[0,1]` clamp and fix the downstream ops that crush highlights (>1) or wide-gamut channels (<0), so a blown highlight can be recovered with detail and the tail stays the sole clip point.

**Architecture:** This is **plumbing, NOT gamut correction** (spec §5.3): no mapping/compression/warnings (those are P8). Remove QuadBin's per-channel `.clamp(0,1)` (upper clip); replace the two color-value `[0,1]` clamps in the `tone_curve` and `hsl` WGSL shaders with out-of-range-preserving math; confirm (audit) the remaining ops don't clip color; and make the export encode tail sanitize non-finite → a defined value. The RGBA16Float working buffer already holds >1 and negative values; the display tail (unorm write + sRGB OETF) and the export tail (`output_oetf` clamp) remain the only places values are gamut-clipped.

**Tech Stack:** Rust + WGSL, `cargo` workspace. Crates touched: `ferrolite-decode` (CPU demosaic), `ferrolite-pipeline` (2 WGSL shaders + a GPU golden), `ferrolite-export` (encode tail). No new dependencies. Depends on **Plans 1–2** (merged); this plan is orthogonal to color but sequenced after so color is correct when highlights return.

## Global Constraints

- **Plumbing, not correction (S4 / §5.3):** P2 performs **no** gamut mapping, compression, out-of-gamut warnings, or soft-proofing. Only *remove* premature clamping. Correction is P8.
- **The tail is the sole clip point (§5.3):** the Spec-3 `working→display` (+sRGB OETF, GPU) and `working→output` (+OETF, at encode) transforms are the only places values are gamut-clipped. They are **unchanged by P2** except for defensive non-finite sanitization at the export tail (§6).
- **Non-finite handling (§6):** unclamped values must not yield NaN/Inf downstream. After the fixes no photo op produces NaN/Inf from finite input; the export tail additionally maps **NaN → 0** (a defined value). Positive/negative Inf are handled by the existing `output_oetf` clamp (→ white/black).
- **Never panic; preserve in-gamut behavior exactly.** For values already in `[0,1]`, every changed op must be byte-for-byte equivalent to today (existing goldens must stay green). Only out-of-range values change.
- **Pipeline built once (CLAUDE.md §2); no UI-thread block (CLAUDE.md §1).** Shader edits are source-only (pipelines still built once); the demosaic runs as a job as before.
- **Photo tier only.** `ferrolite-decode`, `ferrolite-pipeline`, `ferrolite-export`. **No engine-tier (`ferrolite-vt`) changes** — the on-screen display tail and the display-referred histogram live there and are correct as-is (see Audit findings).
- **Gate (per branch):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → **then STOP and hold for the author's (Jann's) visual test** (CLAUDE.md "Finishing a branch"). This plan HAS a visual test (below).

## Audit findings (spec §5.3 "audit downstream ops for hidden [0,1] assumptions")

Every color-value clamp in the pipeline was inspected. Only two crush out-of-range color; the rest are correct and unchanged:

| Op / file | `[0,1]` clamp? | Verdict |
|---|---|---|
| **QuadBin demosaic** (`ferrolite-decode/src/demosaic.rs`) | `.clamp(0,1)` per channel (upper clip) | **FIX (Task 1):** remove — crushes highlights >1. Keep the `.max(0.0)` black-level floor on raw samples (a black point, not a gamut clip). |
| **tone_curve.wgsl** | `clamp(v,0,1)` before LUT index | **FIX (Task 2):** LUT-index math clamps, so identity maps 1.5→1.0. Replace with unit-slope extrapolation outside `[0,1]`. |
| **hsl.wgsl** | `clamp(c,0,1)` input + `max(rgb,0)` output | **FIX (Task 2):** the HSL round-trip needs `[0,1]`, but clamping crushes >1 / negatives even at identity. Apply HSL to the in-gamut part and re-add the out-of-range excess. |
| **contrast.wgsl** | none — `(c-pivot)*gain+pivot` | **No change.** Linear about the pivot; correct for values >1 / <0. |
| **exposure / white_balance / color_matrix** | none — multiply | **No change.** |
| **vignette.wgsl** | `clamp(r,0,1)` on radius; gain clamped ≥0 | **No change.** Spatial radius + non-negative gain, not a color-range clip. |
| **sharpen.wgsl / geometry.wgsl** | `clamp` on sample **coordinates** | **No change.** Spatial addressing, not color. |
| **local_adjust.wgsl** | `max(c,0)` in `adjust()`; `clamp(m,0,1)` mask weight | **No change / out of scope.** Only active inside a user-added mask layer (an explicit local edit), not the base gamut path; not in the spec's audit list. |
| **histogram.wgsl** (`ferrolite-vt`, engine tier) | `clamp(v,0,1)` before binning | **No change.** Intentionally **display-referred**: it applies `working→display` + OETF and folds HDR overshoot into bins 0/255 — a correct read-only readout. Engine tier; do not touch. |
| **display.wgsl** (`ferrolite-vt`, engine tier) — on-screen tail | unorm write clamps | **No change (sole clip point).** Engine tier; unchanged by P2 per §5.3. |
| **export `convert.rs` / `output_oetf`** — encode tail | `output_oetf` clamps input to `[0,1]` | **FIX (Task 3):** already the clip point; add defensive **NaN → 0** so unclamped math can't emit a non-finite pixel (§6). |

---

## File Structure

- `ferrolite-decode/src/demosaic.rs` **(modify)** — remove the three per-channel `.clamp(0.0, 1.0)` in `compute_row`; update the two existing tests that assumed the clamp; add one `>1`-retention test.
- `ferrolite-pipeline/src/shaders/tone_curve.wgsl` **(modify)** — `apply_lut` extrapolates with unit slope outside `[0,1]` instead of clamping.
- `ferrolite-pipeline/src/shaders/hsl.wgsl` **(modify)** — operate on the in-gamut part, re-add the out-of-range excess (identity ⇒ exact pass-through).
- `ferrolite-pipeline/src/lib.rs` **(modify)** — re-export `blit_to_rgba8_with_matrix` (needed by the golden to read back scaled values).
- `ferrolite-pipeline/tests/gamut_golden.rs` **(new)** — GPU golden: an identity edit chain carries a `>1` highlight and a negative channel through to the tail (auto-skip headless).
- `ferrolite-export/src/convert.rs` **(modify)** — sanitize NaN → 0 before the OETF; add tests.

---

## Task 1: Remove the demosaic `[0,1]` clamp (`ferrolite-decode`)

**Files:**
- Modify: `ferrolite-decode/src/demosaic.rs` (`compute_row` ~75-86; tests ~146-159 and ~199-218)
- Test: inline `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Consumes: nothing new.
- Produces: `QuadBin::to_linear_rgba_f32` now emits channels `>1` when white balance / normalization pushes them past the white level (highlights are no longer crushed). Negatives are still floored per-sample by the existing black-level `.max(0.0)` (a black point, not a gamut clip).

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `ferrolite-decode/src/demosaic.rs`:

```rust
    #[test]
    fn quadbin_retains_values_above_one() {
        // White balance pushes a channel past the normalized white level: the
        // demosaic must carry the highlight (>1), not crush it to 1.0.
        // R = (100-0)/100 * wb(2.0) = 2.0 ; G = 50/100 * 1 = 0.5 ; B = 0.
        let mut raw = raw_2x2(100, 50, 50, 0);
        raw.wb_coeffs = [2.0, 1.0, 1.0, 1.0];
        let out = QuadBin.to_linear_rgba_f32(&raw);
        assert!((out.pixels[0] - 2.0).abs() < 1e-6, "R must carry >1 (got {})", out.pixels[0]);
        assert!((out.pixels[1] - 0.5).abs() < 1e-6);
        assert!((out.pixels[2] - 0.0).abs() < 1e-6);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-decode demosaic::tests::quadbin_retains_values_above_one`
Expected: FAIL — R comes back `1.0` (crushed by the current `.clamp(0.0, 1.0)`), assert fails "R must carry >1 (got 1)".

- [ ] **Step 3: Remove the clamp in `compute_row`**

In `ferrolite-decode/src/demosaic.rs`, replace the `compute_row` body's three clamped lines:

```rust
            for x in 0..out_w {
                let r = (sample(x, y, r_pos) * wb[0]).clamp(0.0, 1.0);
                let g = (((sample(x, y, g0) + sample(x, y, g1)) * 0.5) * wb[1]).clamp(0.0, 1.0);
                let b = (sample(x, y, b_pos) * wb[2]).clamp(0.0, 1.0);
```

with (clamp removed; the `.max(0.0)` black floor stays inside `sample`):

```rust
            for x in 0..out_w {
                // No [0,1] clamp: carry highlights >1 to the RGBA16F working buffer
                // (P2 §5.3, gamut-preserving). The black-level floor stays in `sample`.
                let r = sample(x, y, r_pos) * wb[0];
                let g = ((sample(x, y, g0) + sample(x, y, g1)) * 0.5) * wb[1];
                let b = sample(x, y, b_pos) * wb[2];
```

- [ ] **Step 4: Update the two existing tests that assumed the clamp**

In `quadbin_applies_black_level_and_wb`, replace:

```rust
        let out = QuadBin.to_linear_rgba_f32(&raw);
        assert!(
            (out.pixels[0] - 1.0).abs() < 1e-6,
            "R saturates to 1.0 after WB"
        );
```

with:

```rust
        let out = QuadBin.to_linear_rgba_f32(&raw);
        // R = (100-10)*2/(100-10) = 2.0 — carried unclamped now (no [0,1] clip).
        assert!(
            (out.pixels[0] - 2.0).abs() < 1e-6,
            "R carries >1 after WB (got {})",
            out.pixels[0]
        );
```

In `quadbin_parallel_matches_serial_reference_above_threshold`, drop the clamp from the reference so it matches the new `compute_row`. Replace:

```rust
                let r = (sample_ref(x, y, 0) * wb[0]).clamp(0.0, 1.0);
                let g =
                    (((sample_ref(x, y, 1) + sample_ref(x, y, 2)) * 0.5) * wb[1]).clamp(0.0, 1.0);
                let b = (sample_ref(x, y, 3) * wb[2]).clamp(0.0, 1.0);
```

with:

```rust
                let r = sample_ref(x, y, 0) * wb[0];
                let g = ((sample_ref(x, y, 1) + sample_ref(x, y, 2)) * 0.5) * wb[1];
                let b = sample_ref(x, y, 3) * wb[2];
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ferrolite-decode demosaic::`
Expected: PASS (all QuadBin tests, including the new `quadbin_retains_values_above_one` and the two updated tests).

- [ ] **Step 6: Commit**

```bash
git add ferrolite-decode/src/demosaic.rs
git commit -m "feat(decode): stop clamping QuadBin demosaic to [0,1] (carry highlights >1)"
```

---

## Task 2: Preserve out-of-range through tone curve + HSL (`ferrolite-pipeline`)

**Files:**
- Modify: `ferrolite-pipeline/src/shaders/tone_curve.wgsl` (`apply_lut`)
- Modify: `ferrolite-pipeline/src/shaders/hsl.wgsl` (`main`)
- Modify: `ferrolite-pipeline/src/lib.rs` (re-export `blit_to_rgba8_with_matrix`)
- Create: `ferrolite-pipeline/tests/gamut_golden.rs`

**Interfaces:**
- Consumes: `ferrolite_pipeline::{EditPipeline, OpStack, blit_to_rgba8_with_matrix}`, `ferrolite_gpu::GpuContext`, `ferrolite_image::LinearRgbaF32`.
- Produces: the identity edit chain now carries `>1` and negative channels unchanged to the tail (crushing removed). `blit_to_rgba8_with_matrix` is re-exported at the crate root.

- [ ] **Step 1: Write the failing golden**

Create `ferrolite-pipeline/tests/gamut_golden.rs`:

```rust
//! GPU goldens for the P2 gamut-preserving unclamp (spec §5.3): an identity edit
//! chain must carry out-of-range values (highlights >1 and negative wide-gamut
//! channels) through the tone-curve and HSL nodes to the tail, where they clip.
//! We read the working buffer through `blit_to_rgba8_with_matrix` with a probing
//! matrix (scale-down / channel-mix) so a still-out-of-range value maps to a
//! distinct in-[0,1] readback — distinguishing "preserved" from "crushed" without
//! a float readback. Auto-skip when no GPU adapter is present.

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_pipeline::{blit_to_rgba8_with_matrix, EditPipeline, OpStack};

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
const TOL: i32 = 4;

fn srgb_oetf(l: f32) -> f32 {
    if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

fn u8_of(lin: f32) -> i32 {
    (srgb_oetf(lin.clamp(0.0, 1.0)) * 255.0).round() as i32
}

#[test]
fn identity_chain_preserves_highlight_above_one() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // One texel, all channels 1.5 (a blown highlight).
    let src = LinearRgbaF32::new(1, 1, vec![1.5, 1.5, 1.5, 1.0]).unwrap();
    let mut ep = EditPipeline::new(std::sync::Arc::new(ctx), &src, OpStack::default(), IDENTITY);
    let img = ep.evaluate();
    let gpu = ep.gpu_context();
    // Read back through a 0.5x display matrix: preserved 1.5 -> 0.75; a value
    // crushed to 1.0 would read back 0.5. sRGB(0.75) vs sRGB(0.5) differ by ~37 codes.
    let half = [[0.5, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.5]];
    let out = blit_to_rgba8_with_matrix(&gpu, &img, half);
    let want = u8_of(0.75);
    for c in 0..3 {
        assert!(
            (out[c] as i32 - want).abs() <= TOL,
            "channel {c}: highlight crushed — want {want} (0.75 lin) got {} ; a crush would read {}",
            out[c],
            u8_of(0.5)
        );
    }
}

#[test]
fn identity_chain_preserves_negative_channel() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // R negative (wide-gamut), G=1, B=0.5.
    let src = LinearRgbaF32::new(1, 1, vec![-0.2, 1.0, 0.5, 1.0]).unwrap();
    let mut ep = EditPipeline::new(std::sync::Arc::new(ctx), &src, OpStack::default(), IDENTITY);
    let img = ep.evaluate();
    let gpu = ep.gpu_context();
    // Display matrix mixes G into R (row 0 = [1,1,0]): preserved R=-0.2 -> -0.2+1.0=0.8;
    // an R crushed to 0 would read back 0+1.0=1.0. sRGB(0.8) vs sRGB(1.0) differ clearly.
    let mix = [[1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let out = blit_to_rgba8_with_matrix(&gpu, &img, mix);
    let want = u8_of(0.8);
    assert!(
        (out[0] as i32 - want).abs() <= TOL,
        "R channel: negative crushed — want {want} (0.8 lin) got {} ; a crush would read {}",
        out[0],
        u8_of(1.0)
    );
}
```

Add the re-export to `ferrolite-pipeline/src/lib.rs` — change:

```rust
pub use pipeline::{blit_to_rgba8, EditPipeline};
```
to:
```rust
pub use pipeline::{blit_to_rgba8, blit_to_rgba8_with_matrix, EditPipeline};
```

- [ ] **Step 2: Run the golden to verify it fails**

Run: `cargo test -p ferrolite-pipeline --test gamut_golden`
Expected on a GPU box: FAIL — both tests read back the crushed value (`tone_curve` clamps 1.5→1.0 and −0.2→0; `hsl` clamps too), so channel reads ≈ `u8_of(0.5)` / `u8_of(1.0)` instead of the preserved `u8_of(0.75)` / `u8_of(0.8)`. On headless CI: prints "skipping" and passes trivially (does not prove the fix — the CPU Task 1 test and the author's visual test cover the headless case).

- [ ] **Step 3: Fix `tone_curve.wgsl` — extrapolate outside [0,1]**

In `ferrolite-pipeline/src/shaders/tone_curve.wgsl`, replace `apply_lut`:

```wgsl
fn apply_lut(v: f32) -> f32 {
    let x = clamp(v, 0.0, 1.0) * 255.0;
    let i0 = u32(floor(x));
    let i1 = min(i0 + 1u, 255u);
    let f = x - floor(x);
    return mix(lut[i0], lut[i1], f);
}
```

with (unit-slope extrapolation beyond the endpoints, so out-of-range detail survives and identity stays identity):

```wgsl
fn apply_lut(v: f32) -> f32 {
    // Preserve out-of-[0,1] values (P2 §5.3): extrapolate from the LUT endpoints
    // with unit slope so highlights >1 and negatives pass through (identity curve
    // ⇒ exact pass-through), instead of clamping them onto lut[0]/lut[255].
    if (v < 0.0) { return lut[0] + v; }
    if (v > 1.0) { return lut[255] + (v - 1.0); }
    let x = v * 255.0;
    let i0 = u32(floor(x));
    let i1 = min(i0 + 1u, 255u);
    let f = x - floor(x);
    return mix(lut[i0], lut[i1], f);
}
```

- [ ] **Step 4: Fix `hsl.wgsl` — apply to the in-gamut part, re-add the excess**

In `ferrolite-pipeline/src/shaders/hsl.wgsl`, replace the body of `main` from the `textureLoad` through the `textureStore`:

```wgsl
    let c = textureLoad(src, xy, 0);
    let hsl = rgb2hsl(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));

    var hue_acc = 0.0;
    var sat_acc = 0.0;
    var lum_acc = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let w = band_weight(hsl.x, band_center(i));
        hue_acc = hue_acc + w * p.bands[i].x;
        sat_acc = sat_acc + w * p.bands[i].y;
        lum_acc = lum_acc + w * p.bands[i].z;
    }

    var out_hsl = hsl;
    out_hsl.x = hsl.x + hue_acc * MAX_HUE_SHIFT;
    if (out_hsl.x < 0.0) { out_hsl.x = out_hsl.x + 360.0; }
    if (out_hsl.x >= 360.0) { out_hsl.x = out_hsl.x - 360.0; }
    out_hsl.y = clamp(hsl.y * (1.0 + sat_acc), 0.0, 1.0);
    out_hsl.z = clamp(hsl.z * (1.0 + lum_acc), 0.0, 1.0);

    let rgb = hsl2rgb(out_hsl);
    textureStore(dst, xy, vec4<f32>(max(rgb, vec3<f32>(0.0)), c.a));
```

with (the HSL round-trip needs `[0,1]`, so adjust the in-gamut part and carry the out-of-range excess additively — identity ⇒ exact pass-through, preserving >1 and negatives, P2 §5.3):

```wgsl
    let c = textureLoad(src, xy, 0);
    // The HSL round-trip is only defined on [0,1]; adjust the in-gamut part and
    // re-add the out-of-range excess so highlights >1 and negative wide-gamut
    // channels survive (identity bands ⇒ exact pass-through). P2 §5.3.
    let in_gamut = clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let excess = c.rgb - in_gamut;
    let hsl = rgb2hsl(in_gamut);

    var hue_acc = 0.0;
    var sat_acc = 0.0;
    var lum_acc = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let w = band_weight(hsl.x, band_center(i));
        hue_acc = hue_acc + w * p.bands[i].x;
        sat_acc = sat_acc + w * p.bands[i].y;
        lum_acc = lum_acc + w * p.bands[i].z;
    }

    var out_hsl = hsl;
    out_hsl.x = hsl.x + hue_acc * MAX_HUE_SHIFT;
    if (out_hsl.x < 0.0) { out_hsl.x = out_hsl.x + 360.0; }
    if (out_hsl.x >= 360.0) { out_hsl.x = out_hsl.x - 360.0; }
    out_hsl.y = clamp(hsl.y * (1.0 + sat_acc), 0.0, 1.0);
    out_hsl.z = clamp(hsl.z * (1.0 + lum_acc), 0.0, 1.0);

    let rgb = hsl2rgb(out_hsl);
    textureStore(dst, xy, vec4<f32>(rgb + excess, c.a));
```

Also update the file's top comment line 3 from "Display-linear input is clamped to [0,1] for the HSL round-trip (a documented Spec-3 placeholder)." to "Out-of-[0,1] channels bypass the HSL round-trip additively (P2 §5.3): HSL is applied to the in-gamut part and the excess is re-added, so highlights >1 and negatives are preserved."

- [ ] **Step 5: Run the golden to verify it passes**

Run: `cargo test -p ferrolite-pipeline --test gamut_golden`
Expected on a GPU box: PASS (both tests). On headless CI: "skipping" lines.

- [ ] **Step 6: Run the existing color goldens to confirm in-gamut is unchanged**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS — the Spec-3 color goldens and any HSL/tone-curve goldens stay green (in-gamut values are byte-identical: `excess=0` and the extrapolation branches are not taken for `[0,1]` inputs).

- [ ] **Step 7: Commit**

```bash
git add ferrolite-pipeline/src/shaders/tone_curve.wgsl ferrolite-pipeline/src/shaders/hsl.wgsl ferrolite-pipeline/src/lib.rs ferrolite-pipeline/tests/gamut_golden.rs
git commit -m "feat(pipeline): preserve out-of-range through tone curve + HSL (gamut plumbing)"
```

---

## Task 3: Sanitize non-finite at the export tail (`ferrolite-export`)

**Files:**
- Modify: `ferrolite-export/src/convert.rs` (`convert_pixel` + tests)

**Interfaces:**
- Consumes: `ferrolite_color::{mul_vec3, output_oetf, Mat3, WorkingSpace}` (unchanged).
- Produces: `convert_pixel` never emits NaN — a NaN working channel maps to `0.0` before the OETF; ±Inf continues to clamp to white/black via `output_oetf`. Signature unchanged.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `ferrolite-export/src/convert.rs`:

```rust
    #[test]
    fn sanitizes_non_finite_channels() {
        let m = identity();
        // NaN -> 0; +Inf -> clamps to white (1.0); -Inf -> clamps to black (0.0).
        let out = convert_pixel([f32::NAN, f32::INFINITY, f32::NEG_INFINITY], &m, WorkingSpace::Srgb);
        assert!(out.iter().all(|v| v.is_finite()), "output must be finite, got {out:?}");
        assert_eq!(to_u8(out), [0, 255, 0]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-export convert::tests::sanitizes_non_finite_channels`
Expected: FAIL — the NaN channel flows through `output_oetf` (NaN) so `out[0]` is NaN, failing `out.iter().all(is_finite)`.

- [ ] **Step 3: Add the NaN guard in `convert_pixel`**

In `ferrolite-export/src/convert.rs`, replace `convert_pixel`:

```rust
pub(crate) fn convert_pixel(rgb_lin: [f32; 3], m: &Mat3, out: WorkingSpace) -> [f32; 3] {
    let lin = mul_vec3(m, &rgb_lin);
    [
        output_oetf(out, lin[0]),
        output_oetf(out, lin[1]),
        output_oetf(out, lin[2]),
    ]
}
```

with (NaN → 0 before the OETF; ±Inf still handled by `output_oetf`'s clamp — spec §6):

```rust
pub(crate) fn convert_pixel(rgb_lin: [f32; 3], m: &Mat3, out: WorkingSpace) -> [f32; 3] {
    let lin = mul_vec3(m, &rgb_lin);
    // Unclamped working values must never encode a NaN pixel (§6): map NaN to a
    // defined 0.0 before the OETF. ±Inf is left to `output_oetf`'s clamp (→ 1/0).
    let nz = |v: f32| if v.is_nan() { 0.0 } else { v };
    [
        output_oetf(out, nz(lin[0])),
        output_oetf(out, nz(lin[1])),
        output_oetf(out, nz(lin[2])),
    ]
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-export convert::`
Expected: PASS — the new test plus the existing `clamps_out_of_gamut_before_oetf` / `quantizers_round_and_clamp` (unchanged for finite inputs).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-export/src/convert.rs
git commit -m "feat(export): sanitize NaN to 0 at the encode tail (gamut unclamp safety)"
```

---

## Task 4: Workspace green gate + audit doc + visual-test handoff

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all && cargo fmt --check`
Expected: no diff.

- [ ] **Step 2: Clippy (warnings as errors)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings/errors.

- [ ] **Step 3: Full workspace test**

Run: `cargo test --workspace`
Expected: PASS — new demosaic `>1` test, export NaN test, and (on a GPU box) the two gamut goldens; existing color/regression goldens stay green (in-gamut unchanged). The gamut goldens print "skipping" on headless CI.

- [ ] **Step 4: Commit any formatting fixups**

```bash
git add -A && git commit -m "chore: cargo fmt for P2 plan 3" || echo "nothing to format"
```

- [ ] **Step 5: STOP — hand the author the visual test**

Per CLAUDE.md "Finishing a branch", the gate is necessary but not sufficient. Present this and **hold** for Jann's hands-on results before merging/PR-ing:

**Visual test (real surface this plan):**
1. **Open a RAW with a clipped/blown highlight** (a bright sky, a specular, a light source — a region at or above sensor white). Go to Develop.
2. **Pull Exposure down (and/or the Highlights slider negative)** to recover the blown region.
   - **Expected:** *detail returns* in the highlight — texture/gradient that was previously flat white reappears as you recover, because the >1 values were carried through the working pipeline instead of being crushed to 1.0 at demosaic/tone-curve/HSL. The recovered highlight stays **neutral / correctly-hued** — no magenta/cyan cast appearing before the tail.
   - **Failure signatures:** the blown area stays flat white no matter how far you pull exposure/highlights down (detail was crushed upstream — unclamp didn't take); a **hue shift** (e.g. highlights go magenta/green) as you recover (a channel was clipped asymmetrically before the tail); NaN/black speckles in extreme highlights (non-finite leaked).
3. **Sanity — no regression on normal images:** open a well-exposed in-gamut image; it should look **identical** to before this branch (in-gamut values are unchanged). Check the histogram still reads normally (it remains a display-referred readout; clipped highlights still pile into the top bin).
4. **Export check (optional):** export the recovered image; the encoded file should show the recovered highlight detail (the export tail clips only at the very end).

Do NOT merge/PR until Jann confirms highlight detail returns with no hue shift and no regression on normal images. Address any issue found, then re-run the gate.

---

## Self-Review

**Spec coverage (§5.3 + §6 + §10):**
- "Remove the premature clamp … QuadBin `.clamp(0.0,1.0)`" → Task 1 (removed; `.max(0.0)` black floor kept, documented).
- "Audit downstream ops (contrast pivot, tone curve, HSL, histogram binning) … fix any that would crush" → Audit findings table + Task 2 (tone curve + HSL fixed; contrast/histogram audited → no change, with reasons).
- "Clip/convert only at the tail … the sole places values are gamut-clipped" → Audit table confirms display (`ferrolite-vt`, unchanged) and export (`output_oetf` clamp) are the clip points; goldens prove values survive *to* the tail.
- "No gamut correction here" → nothing maps/compresses/warns; only clamps removed (Global Constraints).
- §6 "Unclamped values must not produce NaN/Inf … clamp only NaN/Inf to a defined value at the tail" → the fixes keep HSL math on `[0,1]` (no Inf from `l=1` division) and tone-curve extrapolation is finite; Task 3 sanitizes NaN → 0 at the export tail (±Inf → white/black via `output_oetf`).
- §10 "Unclamp: demosaic output retains channels >1 and out-of-gamut values" → Task 1 CPU test `quadbin_retains_values_above_one`; Task 2 GPU goldens carry >1 and negative through the chain to the tail.
- "Regression golden: sRGB + single-illuminant + QuadBin ≡ today's output" → preserved: in-gamut values are byte-identical (Task 2 Step 6 re-runs existing goldens; clamp removal is a no-op for values ≤1).

**Placeholder scan:** No TBD/TODO/"handle edge cases"; every code step shows full before/after; every run step states expected output; the one skip-on-headless golden is called out (its headless behavior is trivial-pass, with CPU + visual coverage noted).

**Type consistency:** `blit_to_rgba8_with_matrix(&GpuContext, &PipelineImage, [[f32;3];3]) -> Vec<u8>` matches its use in the golden and the new re-export; `EditPipeline::{evaluate, gpu_context}` are existing pub methods; `convert_pixel([f32;3], &Mat3, WorkingSpace) -> [f32;3]` signature unchanged; `QuadBin::to_linear_rgba_f32` output contract (now unclamped) matches the new + updated tests.
