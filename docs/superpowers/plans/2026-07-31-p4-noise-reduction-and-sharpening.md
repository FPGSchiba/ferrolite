# P4 — Noise Reduction & Sharpening (Classical) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship classical à trous-wavelet noise reduction (wiring the four greyed NOISE REDUCTION sliders), upgrade capture sharpening with Detail + Masking, and add medium-aware output sharpening at export — with every existing parity golden and every existing export staying byte-identical.

**Architecture:** A new `NoiseReductionNode` is inserted between `color_matrix` and `vignette` in both `EditPipeline` and `TileEditPipeline`. It runs a fused streaming à trous loop in a luma/chroma space — shrinkage folded into a single 2D convolution pass per level, so only 4 ping-pong textures are live regardless of level count. `Sharpen` gains `detail`/`masking` fields whose math collapses to today's exact formula at zero. Export gains a pure CPU `output_sharpen` module between resize and encode.

**Tech Stack:** Rust, wgpu/WGSL compute, `rayon` (export CPU pass), existing `Graph<PipelineImage>` retained-DAG executor, existing `engine_bench` + parity-golden harnesses. **No new dependencies.**

**Spec:** `docs/superpowers/specs/2026-07-31-p4-noise-reduction-and-sharpening-design.md` — read §3 before Tasks 1–4, §4 before Task 5, §5 before Task 6.

## Global Constraints

- **Branch:** `feat/p4-noise-reduction-and-sharpening` (already created, off `main`). Never commit to `main`.
- **No new dependencies.** Not in any `Cargo.toml`. The wavelet is separable box-class arithmetic.
- **Tier:** photo-tier only (`ferrolite-pipeline`, `ferrolite-export`, `ferrolite-app`). Do NOT touch `ferrolite-gpu`, `ferrolite-vt`, `ferrolite-image`, `ferrolite-jobs`, or `ferrolite-mask`.
- **`L = 5`** wavelet levels. NR halo = `2·(2^L − 1)` = **62 px**.
- **NR uses a FUSED 2D convolution, not separable H/V** (spec §3.3, amended 2026-07-31): four
  full-res `rgba16float` ping-pong textures (192 MB each at 24 MP), allocated only after the
  identity early-return. This is the opposite of sharpen's separable choice, for the reason spec
  §3.3 records — do not "optimize" it back to separable.
- **Memory gate (Task 3 Step 7 / Task 8):** peak GPU bytes with NR active must be measured; if it
  is at or near an OOM on a 6–8 GB budget, take spec §3.3's **pre-agreed** tile-path-only fallback
  rather than opening a new decision.
- **Noise-propagation constants** `s_l ≈ [0.890, 0.201, 0.086, 0.041, 0.020]` for `l = 0..4`.
- **Threshold curve:** `t_l = strength · s_l · f(detail, l)` where `f(detail, l) = 1 − detail · max(0, 1 − l/2)`.
- **Soft shrinkage only:** `shrink(d, t) = sign(d)·max(|d| − t, 0)`. Never hard-threshold.
- **Masking edge term:** `t0 = masking · G`, `t1 = t0 + 0.25·G`; `edge = masking > 0 ? smoothstep(t0, t1, |∇luma|) : 1.0`.
- **Three identity gates are non-negotiable** (spec §7.2). NR all-zero, sharpen `detail=0 && masking=0`, and export `None`/`Standard` must each be **byte-identical** to current behavior. If an existing parity golden goes red, that is a **bug in your identity path** — do NOT regenerate the golden.
- **Do NOT bump `PIPELINE_SCHEMA_VERSION`** in `ferrolite-previews` (spec §7.2). Identity NR leaves identity renders unchanged; bumping needlessly invalidates every cached preview.
- **Per-control reset** on every new slider, via the `EguiSlider` reset column (CLAUDE.md, load-bearing).
- **Icons** only from `ferrolite-app/src/icons.rs`. No raw emoji, no hand-drawn `Painter` shapes.
- **Build GPU pipelines once** in the node's `new`; register every new shader in `prewarm_shaders`. Never rebuild per image/open/interaction.
- **Gate:** every task runs the **scoped gate** for its crate(s), NOT the repo gate:
  `cargo fmt -p X -- --check` · `cargo clippy -p X --all-targets -- -D warnings` · `cargo test -p X`.
  The coordinator runs the repo gate once at the end.
- **GPU tests** must skip on software adapters and headless: guard with `let Some(ctx) = GpuContext::headless() else { eprintln!("no GPU adapter; skipping (headless CI)"); return; };` — copy this exact idiom from `ferrolite-pipeline/tests/golden.rs`.

---

## File Structure

**Created:**
| File | Responsibility |
|---|---|
| `ferrolite-pipeline/src/nr.rs` | Pure CPU wavelet math: B3-spline convolution, `s_l` constants, threshold curve, soft shrink, and the full `atrous_shrink_reference`. No GPU types. The correctness oracle. |
| `ferrolite-pipeline/src/nr_node.rs` | `NoiseReductionNode` — the multi-pass GPU node. Owns pipelines, the 4-texture pool, and `evaluate`. |
| `ferrolite-pipeline/src/shaders/nr_atrous.wgsl` | One fused 2D B3-spline level + shrink + accumulate (the streaming step). Deliberately not separable — see spec §3.3. |
| `ferrolite-pipeline/src/shaders/nr_combine.wgsl` | Final `acc + approx`, YCbCr→working. |
| `ferrolite-pipeline/src/shaders/sharpen_apply_detail.wgsl` | Sharpen apply with the Detail mix + Masking edge term. |
| `ferrolite-export/src/output_sharpen.rs` | Pure CPU separable unsharp over the quantized RGB buffer; the medium/amount table. |

**Modified:**
| File | Change |
|---|---|
| `ferrolite-pipeline/src/lib.rs` | `mod nr; mod nr_node;`, re-exports, 3 new `prewarm_shaders` entries. |
| `ferrolite-pipeline/src/uniforms.rs` | `NrUniform`, `nr_uniform`, `nr_halo`; extend `sharpen_uniform`/`sharpen_halo`/`sharpen_halo_doc`. |
| `ferrolite-pipeline/src/local.rs` | `NoiseReduction` gains `is_identity()`. |
| `ferrolite-pipeline/src/op.rs` | `Sharpen` gains `detail` + `masking`. |
| `ferrolite-pipeline/src/pipeline.rs` | Insert NR node; `node_count: 8` → `9`. |
| `ferrolite-pipeline/src/tile_edit.rs` | Insert NR node; add `nr_halo` to the halo max. |
| `ferrolite-pipeline/src/sharpen_node.rs` | Second blur radius (`r/3`) + the detail/masking apply pipeline. |
| `ferrolite-pipeline/tests/golden.rs` | `node_count() - 3` → `- 4`; new fixtures. |
| `ferrolite-app/src/develop/adjustments.rs` | Ungrey 4 NR specs; add `sharpen_detail` + `sharpen_masking`. |
| `ferrolite-app/src/develop/base_tabs.rs` | The 1:1 hint line; refresh two stale comments. |
| `ferrolite-app/src/develop/ops_edit.rs` | `needs_full_rebuild` accounts for `nr_halo`. |
| `ferrolite-export/src/options.rs` | `OutputMedium`, `OutputSharpenAmount`, 2 `ExportOptions` fields. |
| `ferrolite-export/src/job.rs` | Call `output_sharpen` between resize and encode. |
| `ferrolite-export/src/lib.rs` | `mod output_sharpen;` + re-exports. |
| `ferrolite-app/src/export/settings_form.rs` | Two combos. |
| `docs/design/V2/README.md` | Masking slider, 1:1 hint, export combos. |
| `docs/benchmarks/2026-07-28-phase3-fused-engine.md` | P4 bench numbers. |

---

## Task 1: Pure CPU wavelet math (`nr.rs`)

No GPU. This is the oracle every later task is measured against. Read spec §3.2–§3.4 first.

**Files:**
- Create: `ferrolite-pipeline/src/nr.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (add `mod nr;` and re-exports)
- Modify: `ferrolite-pipeline/src/local.rs` (add `NoiseReduction::is_identity`)

**Interfaces:**
- Consumes: `ferrolite_pipeline::local::NoiseReduction` (existing: `{ luminance, detail, color, color_detail }`, all `f32`).
- Produces, all `pub`, used by Tasks 2/3/4:
  - `pub const NR_LEVELS: usize = 5;`
  - `pub const NR_NOISE_SCALE: [f32; NR_LEVELS];`
  - `pub fn nr_halo_px() -> u32` → `62`
  - `pub fn threshold_at(strength: f32, detail: f32, level: usize) -> f32`
  - `pub fn shrink(d: f32, t: f32) -> f32`
  - `pub fn b3_spline_h(src: &[f32], w: usize, h: usize, spacing: usize) -> Vec<f32>`
  - `pub fn b3_spline_v(src: &[f32], w: usize, h: usize, spacing: usize) -> Vec<f32>`
  - `pub fn b3_spline_2d(src: &[f32], w: usize, h: usize, spacing: usize) -> Vec<f32>`
  - `pub fn atrous_shrink_reference(luma: &[f32], w: usize, h: usize, strength: f32, detail: f32) -> Vec<f32>`
  - `pub fn rgb_to_ycbcr(rgb: [f32; 3]) -> [f32; 3]` / `pub fn ycbcr_to_rgb(y: [f32; 3]) -> [f32; 3]`
  - On `NoiseReduction`: `pub fn is_identity(&self) -> bool`

- [ ] **Step 1: Write the failing tests**

Create `ferrolite-pipeline/src/nr.rs` with ONLY the test module plus `use super::*;` so it fails to compile against absent items:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The separable H-then-V B3-spline convolution must equal the direct 2D
    /// form. Mirrors `sharpen_node.rs`'s `separable_box_equals_2d_box`, which
    /// proved the same property for the box mean before the GPU passes existed.
    #[test]
    fn separable_b3spline_equals_direct() {
        let (w, h) = (24usize, 18usize);
        let src: Vec<f32> = (0..w * h)
            .map(|i| {
                let x = (i % w) as f32;
                let y = (i / w) as f32;
                (x * 0.13).sin() * 0.5 + (y * 0.31).cos() * 0.3 + 0.5
            })
            .collect();
        for spacing in [1usize, 2, 4, 8, 16] {
            let sep = b3_spline_v(&b3_spline_h(&src, w, h, spacing), w, h, spacing);
            let direct = b3_spline_2d(&src, w, h, spacing);
            for (i, (a, b)) in sep.iter().zip(direct.iter()).enumerate() {
                assert!(
                    (a - b).abs() < 1e-6,
                    "spacing {spacing} idx {i}: separable {a} vs direct {b}"
                );
            }
        }
    }

    /// A flat image has no detail at any scale, so shrinkage cannot change it.
    #[test]
    fn flat_image_is_unchanged_by_any_strength() {
        let (w, h) = (16usize, 16usize);
        let src = vec![0.42f32; w * h];
        for strength in [0.0f32, 0.5, 1.0] {
            let out = atrous_shrink_reference(&src, w, h, strength, 0.0);
            for v in &out {
                assert!((v - 0.42).abs() < 1e-6, "flat image changed at {strength}");
            }
        }
    }

    /// Zero strength is an exact identity — the guarantee the GPU node's
    /// early-return (Task 2) mirrors.
    #[test]
    fn zero_strength_is_identity() {
        let (w, h) = (20usize, 12usize);
        let src: Vec<f32> = (0..w * h).map(|i| ((i * 37) % 101) as f32 / 101.0).collect();
        let out = atrous_shrink_reference(&src, w, h, 0.0, 0.0);
        for (a, b) in out.iter().zip(src.iter()) {
            assert!((a - b).abs() < 1e-6, "zero strength was not identity");
        }
    }

    /// Denoising must actually reduce noise: white noise on a flat field has
    /// lower variance after shrinkage.
    #[test]
    fn white_noise_variance_drops() {
        let (w, h) = (64usize, 64usize);
        // Deterministic pseudo-noise (no rand dep): a simple LCG.
        let mut state = 12345u32;
        let src: Vec<f32> = (0..w * h)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                0.5 + ((state >> 16) as f32 / 65535.0 - 0.5) * 0.1
            })
            .collect();
        let var = |v: &[f32]| {
            let m = v.iter().sum::<f32>() / v.len() as f32;
            v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32
        };
        let out = atrous_shrink_reference(&src, w, h, 1.0, 0.0);
        assert!(
            var(&out) < var(&src) * 0.6,
            "variance {} not meaningfully below {}",
            var(&out),
            var(&src)
        );
    }

    /// `detail` protects fine scales: it zeroes level 0's threshold, halves
    /// level 1's, and never touches level >= 2.
    #[test]
    fn detail_attenuates_only_the_two_finest_levels() {
        let s = 1.0;
        assert!(threshold_at(s, 1.0, 0).abs() < 1e-9, "detail=1 zeroes level 0");
        let half = threshold_at(s, 0.0, 1) * 0.5;
        assert!((threshold_at(s, 1.0, 1) - half).abs() < 1e-6, "detail=1 halves level 1");
        for l in 2..NR_LEVELS {
            assert_eq!(
                threshold_at(s, 1.0, l),
                threshold_at(s, 0.0, l),
                "level {l} must be untouched by detail"
            );
        }
    }

    /// Soft shrinkage, never hard: a coefficient just above the threshold
    /// survives as a SMALL value, it is not passed through at full magnitude.
    #[test]
    fn shrink_is_soft_not_hard() {
        assert_eq!(shrink(0.05, 0.10), 0.0, "below threshold -> zero");
        assert!((shrink(0.12, 0.10) - 0.02).abs() < 1e-6, "soft: |d| - t");
        assert!((shrink(-0.12, 0.10) + 0.02).abs() < 1e-6, "sign preserved");
    }

    #[test]
    fn ycbcr_round_trips() {
        for rgb in [[0.1, 0.5, 0.9], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.7, 0.2, 0.4]] {
            let back = ycbcr_to_rgb(rgb_to_ycbcr(rgb));
            for i in 0..3 {
                assert!((back[i] - rgb[i]).abs() < 1e-5, "round trip failed for {rgb:?}");
            }
        }
    }

    #[test]
    fn noise_reduction_is_identity_only_when_all_zero() {
        use crate::local::NoiseReduction;
        assert!(NoiseReduction::default().is_identity());
        for nr in [
            NoiseReduction { luminance: 0.1, ..Default::default() },
            NoiseReduction { detail: 0.1, ..Default::default() },
            NoiseReduction { color: 0.1, ..Default::default() },
            NoiseReduction { color_detail: 0.1, ..Default::default() },
        ] {
            assert!(!nr.is_identity(), "{nr:?} must not be identity");
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline nr::tests`
Expected: FAIL — compile errors, `cannot find function b3_spline_h` etc. (`mod nr;` must already be added to `lib.rs` for this to compile-fail rather than be skipped).

- [ ] **Step 3: Implement `nr.rs`**

Prepend to `ferrolite-pipeline/src/nr.rs`:

```rust
//! Pure CPU reference for the à trous wavelet noise reduction (P4 design §3.2–§3.4).
//! No GPU types: this module is the correctness oracle the WGSL passes in
//! `nr_node.rs` are goldened against, exactly as `dehaze::transmission_map` is
//! the oracle for the dehaze passes.

/// Wavelet decomposition levels (design constant — halo derives from it).
pub const NR_LEVELS: usize = 5;

/// The factor by which unit-variance white noise's standard deviation survives
/// into each à trous level of a B3-spline decomposition. Using these means ONE
/// strength slider yields a physically consistent threshold at every scale.
/// Verified by `white_noise_variance_drops` rather than trusted from literature.
pub const NR_NOISE_SCALE: [f32; NR_LEVELS] = [0.890, 0.201, 0.086, 0.041, 0.020];

/// The B3-spline kernel [1,4,6,4,1]/16.
const B3: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// Halo (pixels) a tiled NR pass must over-fetch: the total support of `NR_LEVELS`
/// à trous levels. Level `l` uses a 5-tap kernel at spacing `2^l`, so radius
/// `2·2^l`; summing gives `2·(2^L − 1)`.
pub fn nr_halo_px() -> u32 {
    2 * ((1u32 << NR_LEVELS) - 1)
}

/// `t_l = strength · s_l · f(detail, l)`, `f = 1 − detail·max(0, 1 − l/2)`.
/// `detail = 1` zeroes level 0, halves level 1, leaves `l >= 2` untouched.
pub fn threshold_at(strength: f32, detail: f32, level: usize) -> f32 {
    let s_l = NR_NOISE_SCALE[level.min(NR_LEVELS - 1)];
    let f = 1.0 - detail * (1.0 - level as f32 / 2.0).max(0.0);
    strength * s_l * f
}

/// Soft shrinkage. Hard thresholding is what produces the "plastic" look.
pub fn shrink(d: f32, t: f32) -> f32 {
    let m = d.abs() - t;
    if m <= 0.0 {
        0.0
    } else if d < 0.0 {
        -m
    } else {
        m
    }
}

fn clamp_idx(v: isize, n: usize) -> usize {
    v.clamp(0, n as isize - 1) as usize
}

/// Horizontal B3-spline convolution at hole spacing `spacing`, clamping x only.
pub fn b3_spline_h(src: &[f32], w: usize, h: usize, spacing: usize) -> Vec<f32> {
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, coeff) in B3.iter().enumerate() {
                let dx = (k as isize - 2) * spacing as isize;
                acc += coeff * src[y * w + clamp_idx(x as isize + dx, w)];
            }
            out[y * w + x] = acc;
        }
    }
    out
}

/// Vertical B3-spline convolution at hole spacing `spacing`, clamping y only.
pub fn b3_spline_v(src: &[f32], w: usize, h: usize, spacing: usize) -> Vec<f32> {
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, coeff) in B3.iter().enumerate() {
                let dy = (k as isize - 2) * spacing as isize;
                acc += coeff * src[clamp_idx(y as isize + dy, h) * w + x];
            }
            out[y * w + x] = acc;
        }
    }
    out
}

/// Direct (non-separable) 2D B3-spline convolution — the oracle proving the
/// H-then-V composition above is equivalent.
pub fn b3_spline_2d(src: &[f32], w: usize, h: usize, spacing: usize) -> Vec<f32> {
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (ky, cy) in B3.iter().enumerate() {
                let dy = (ky as isize - 2) * spacing as isize;
                let yy = clamp_idx(y as isize + dy, h);
                for (kx, cx) in B3.iter().enumerate() {
                    let dx = (kx as isize - 2) * spacing as isize;
                    acc += cy * cx * src[yy * w + clamp_idx(x as isize + dx, w)];
                }
            }
            out[y * w + x] = acc;
        }
    }
    out
}

/// The full streaming à trous shrink of one scalar plane (design §3.3):
/// shrinkage is fused into the decomposition loop, so no level is retained.
pub fn atrous_shrink_reference(
    plane: &[f32],
    w: usize,
    h: usize,
    strength: f32,
    detail: f32,
) -> Vec<f32> {
    if strength <= 0.0 {
        return plane.to_vec();
    }
    let mut approx = plane.to_vec();
    let mut acc = vec![0.0f32; w * h];
    for l in 0..NR_LEVELS {
        let spacing = 1usize << l;
        let next = b3_spline_v(&b3_spline_h(&approx, w, h, spacing), w, h, spacing);
        let t = threshold_at(strength, detail, l);
        for i in 0..w * h {
            acc[i] += shrink(approx[i] - next[i], t);
        }
        approx = next;
    }
    for i in 0..w * h {
        acc[i] += approx[i];
    }
    acc
}

/// Rec.709 luma / centred chroma. Chroma is centred on 0 (not 0.5) so a zero
/// coefficient means "no chroma", which keeps shrinkage sign-symmetric.
pub fn rgb_to_ycbcr(rgb: [f32; 3]) -> [f32; 3] {
    let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    [y, rgb[2] - y, rgb[0] - y]
}

/// Inverse of [`rgb_to_ycbcr`].
pub fn ycbcr_to_rgb(ycc: [f32; 3]) -> [f32; 3] {
    let (y, cb, cr) = (ycc[0], ycc[1], ycc[2]);
    let r = cr + y;
    let b = cb + y;
    let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    [r, g, b]
}
```

- [ ] **Step 4: Add `NoiseReduction::is_identity`**

In `ferrolite-pipeline/src/local.rs`, inside the existing `impl` block area for `NoiseReduction` (add an `impl` block directly after the struct definition if none exists):

```rust
impl NoiseReduction {
    /// True when every field is zero-identity — the gate the GPU node's
    /// passthrough and `nr_halo` both key off.
    pub fn is_identity(&self) -> bool {
        self.luminance == 0.0
            && self.detail == 0.0
            && self.color == 0.0
            && self.color_detail == 0.0
    }
}
```

Then in `AdjustmentSet::is_identity`, replace `&& self.noise_reduction == NoiseReduction::default()` with `&& self.noise_reduction.is_identity()` (equivalent, and keeps one definition of the predicate).

- [ ] **Step 5: Wire the module**

In `ferrolite-pipeline/src/lib.rs`, add `mod nr;` in the alphabetical `mod` list (between `mod local_node;` and `mod mask_overlay;`), and add the re-export:

```rust
pub use nr::{
    atrous_shrink_reference, b3_spline_2d, b3_spline_h, b3_spline_v, nr_halo_px, rgb_to_ycbcr,
    shrink, threshold_at, ycbcr_to_rgb, NR_LEVELS, NR_NOISE_SCALE,
};
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline nr::`
Expected: PASS — all 8 tests.

If `white_noise_variance_drops` fails, the `NR_NOISE_SCALE` constants are wrong for this kernel — recompute them empirically (propagate unit white noise through `b3_spline_h`/`_v` at each spacing and measure the per-level detail std) and update both the constant and its doc comment. Do NOT loosen the assertion.

- [ ] **Step 7: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-pipeline -- --check
cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings
cargo test -p ferrolite-pipeline
git add ferrolite-pipeline/src/nr.rs ferrolite-pipeline/src/lib.rs ferrolite-pipeline/src/local.rs
git commit -m "feat(pipeline): pure CPU a trous wavelet NR reference + NoiseReduction::is_identity"
```

---

## Task 2: NR uniform + halo (`uniforms.rs`)

Pure, no GPU. Read spec §3.4.

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (re-exports)

**Interfaces:**
- Consumes: `nr::{NR_LEVELS, nr_halo_px, threshold_at}` and `local::NoiseReduction::is_identity` (Task 1).
- Produces, used by Tasks 3/4/7:
  - `#[repr(C)] pub struct NrUniform { pub thresholds: [f32; 8], pub active: i32, pub spacing: i32, pub level: i32, pub pad: f32 }` — `Pod + Zeroable`
  - `pub fn nr_uniform(nr: &NoiseReduction, level: usize) -> NrUniform`
  - `pub fn nr_halo(nr: &NoiseReduction) -> u32`
  - `pub fn nr_halo_doc(doc: &crate::op::OpStack) -> u32`

**Note on `thresholds: [f32; 8]`:** four luma thresholds would not cover `NR_LEVELS = 5`. The array holds the CURRENT level's luma threshold at `[0]` and chroma at `[1]`, with `[2..8]` reserved padding to keep the struct 16-byte-aligned for WGSL uniform layout. Per-level dispatch means only one level's thresholds are ever live.

- [ ] **Step 1: Write the failing tests**

Append to `ferrolite-pipeline/src/uniforms.rs`'s existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn nr_halo_is_total_atrous_support_or_zero() {
    use crate::local::NoiseReduction;
    assert_eq!(nr_halo(&NoiseReduction::default()), 0, "identity -> no halo");
    let active = NoiseReduction { luminance: 0.5, ..Default::default() };
    assert_eq!(nr_halo(&active), crate::nr::nr_halo_px());
    assert_eq!(nr_halo(&active), 62, "L=5 -> 2*(2^5-1)");
    // Chroma-only NR still needs the full halo (same decomposition).
    let chroma = NoiseReduction { color: 0.5, ..Default::default() };
    assert_eq!(nr_halo(&chroma), 62);
}

#[test]
fn nr_halo_doc_is_zero_unless_the_global_set_has_nr() {
    use crate::op::EditDoc;
    assert_eq!(nr_halo_doc(&EditDoc::default()), 0);
    let mut doc = EditDoc::default();
    doc.global.noise_reduction.luminance = 0.4;
    assert_eq!(nr_halo_doc(&doc), 62, "global NR contributes the halo");
    // NR is GLOBAL-ONLY (design §3.5): a mask layer's NR must NOT contribute.
    let mut masked = EditDoc::default();
    if let Some(layer) = masked.layers.first_mut() {
        layer.adjustments.noise_reduction.luminance = 0.9;
    }
    assert_eq!(
        nr_halo_doc(&masked),
        0,
        "per-mask NR is not applied, so it must contribute no halo"
    );
}

#[test]
fn nr_uniform_is_inactive_at_identity() {
    use crate::local::NoiseReduction;
    let u = nr_uniform(&NoiseReduction::default(), 0);
    assert_eq!(u.active, 0);
    let u = nr_uniform(&NoiseReduction { luminance: 0.5, ..Default::default() }, 0);
    assert_eq!(u.active, 1);
}

#[test]
fn nr_uniform_carries_the_levels_spacing_and_thresholds() {
    use crate::local::NoiseReduction;
    let nr = NoiseReduction { luminance: 1.0, detail: 0.0, color: 0.5, color_detail: 0.0 };
    for level in 0..crate::nr::NR_LEVELS {
        let u = nr_uniform(&nr, level);
        assert_eq!(u.level, level as i32);
        assert_eq!(u.spacing, 1 << level, "spacing = 2^level");
        assert!(
            (u.thresholds[0] - crate::nr::threshold_at(1.0, 0.0, level)).abs() < 1e-9,
            "luma threshold at level {level}"
        );
        assert!(
            (u.thresholds[1] - crate::nr::threshold_at(0.5, 0.0, level)).abs() < 1e-9,
            "chroma threshold at level {level}"
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline uniforms::tests::nr_`
Expected: FAIL — `cannot find function nr_halo`.

- [ ] **Step 3: Implement**

Add to `ferrolite-pipeline/src/uniforms.rs`:

```rust
/// GPU layout for one NR level's dispatch. Only the CURRENT level is live per
/// dispatch, so `thresholds[0]` is this level's luma threshold and
/// `thresholds[1]` its chroma threshold; `[2..8]` is reserved padding keeping
/// the struct 16-byte-aligned for WGSL uniform rules.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct NrUniform {
    pub thresholds: [f32; 8],
    /// 0 = identity (the node never dispatches in this state).
    pub active: i32,
    /// À trous hole spacing for this level: `2^level`.
    pub spacing: i32,
    pub level: i32,
    pub pad: f32,
}

/// Build the uniform for `level` of the à trous loop.
pub fn nr_uniform(nr: &crate::local::NoiseReduction, level: usize) -> NrUniform {
    let mut thresholds = [0.0f32; 8];
    thresholds[0] = crate::nr::threshold_at(nr.luminance, nr.detail, level);
    thresholds[1] = crate::nr::threshold_at(nr.color, nr.color_detail, level);
    NrUniform {
        thresholds,
        active: (!nr.is_identity()) as i32,
        spacing: 1 << level,
        level: level as i32,
        pad: 0.0,
    }
}

/// Halo (pixels) a tiled NR pass must over-fetch. Zero at identity, mirroring
/// `sharpen_halo`'s contract.
pub fn nr_halo(nr: &crate::local::NoiseReduction) -> u32 {
    if nr.is_identity() {
        0
    } else {
        crate::nr::nr_halo_px()
    }
}

/// Whole-document NR halo. NR is GLOBAL-ONLY (design §3.5) — it runs upstream of
/// where masks are composited — so unlike `sharpen_halo_doc` this deliberately
/// does NOT walk the layers. A layer's `noise_reduction` fields are never
/// applied, so they must contribute no halo.
pub fn nr_halo_doc(doc: &crate::op::OpStack) -> u32 {
    nr_halo(&doc.global.noise_reduction)
}
```

Re-export from `lib.rs`'s existing `pub use uniforms::{...}` list: `nr_halo, nr_halo_doc, nr_uniform, NrUniform`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline uniforms::tests::nr_`
Expected: PASS — 4 tests.

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-pipeline -- --check
cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings
cargo test -p ferrolite-pipeline
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): NrUniform + nr_halo/nr_halo_doc (global-only, zero at identity)"
```

---

## Task 3: The GPU `NoiseReductionNode`

Read spec §3.3 **as amended 2026-07-31** (fused 2D, four textures, the memory gate and its
pre-agreed fallback) and `ferrolite-pipeline/src/sharpen_node.rs`'s `new`/`ensure_blur_slot`/
`alloc_out` before starting — this node mirrors that structure.

**House pattern:** `compute_pipeline` and `Intermediates` are deliberately **duplicated per module**
in this crate (`dehaze_node.rs`, `rcd_gpu.rs`, `sharpen_node.rs` each have their own). Write
module-private copies in `nr_node.rs` too. Do **not** refactor them into a shared helper — that is
out of scope for this phase.

**Files:**
- Create: `ferrolite-pipeline/src/shaders/nr_atrous.wgsl`
- Create: `ferrolite-pipeline/src/shaders/nr_combine.wgsl`
- Create: `ferrolite-pipeline/src/nr_node.rs`
- Create: `ferrolite-pipeline/tests/nr_node.rs`
- Modify: `ferrolite-pipeline/tests/common/mod.rs` (add the `noisy_flat` fixture)
- Modify: `ferrolite-pipeline/src/lib.rs` (`mod nr_node;`, 2 `prewarm_shaders` entries)

**Interfaces:**
- Consumes: `NrUniform`/`nr_uniform` (Task 2); `nr::NR_LEVELS`; `PipelineImage`, `GpuContext`,
  `Node<PipelineImage>`.
- Produces, used by Task 4:
  - `pub(crate) struct NoiseReductionNode`
  - `pub(crate) fn new(ctx: Arc<GpuContext>, params: Rc<Cell<NoiseReduction>>) -> Self`
  - `pub(crate) fn eval_count(&self) -> u32` — test hook proving identity dispatches nothing
  - `pub(crate) fn live_bytes(&self) -> u64` — the memory gate's instrument (Step 7)
  - `impl Node<PipelineImage> for NoiseReductionNode`

- [ ] **Step 1: Write the two shaders**

`ferrolite-pipeline/src/shaders/nr_atrous.wgsl` — one fused 2D pass per level, with shrinkage and
accumulation folded in. This single pass is what keeps the texture count at four (spec §3.3):

```wgsl
// NR: ONE à trous level, fused. Computes the 2D B3-spline [1,4,6,4,1]/16 outer
// product at hole spacing `p.spacing` (= 2^level), derives this level's detail
// coefficient, soft-shrinks it, and accumulates — all in one pass.
//
// Deliberately NOT separable H-then-V (spec §3.3): at a fixed 5 taps, separable
// is 10 taps vs 25 but costs an extra full-res texture AND an extra full-res
// round-trip, and these passes are bandwidth-bound. `nr.rs`'s
// `separable_b3spline_equals_direct` proves this 2D form equals the separable
// reference, so the shipped pass has a verified oracle.
//
// At level 0 the node binds the ORIGINAL working-space image as both `src` and
// `approx`, and this shader converts RGB->YCbCr on load (the `p.level == 0`
// branch) so no separate conversion pass or texture is needed.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var approx: texture_2d<f32>;
@group(0) @binding(2) var acc_in: texture_2d<f32>;
@group(0) @binding(3) var dst_next: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var dst_acc: texture_storage_2d<rgba16float, write>;
struct P { thresholds: array<vec4<f32>, 2>, active: i32, spacing: i32, level: i32, pad: f32 };
@group(0) @binding(5) var<uniform> p: P;

const B: array<f32, 5> = array<f32, 5>(0.0625, 0.25, 0.375, 0.25, 0.0625);

fn to_ycbcr(rgb: vec3<f32>) -> vec3<f32> {
    let y = 0.2126 * rgb.r + 0.7152 * rgb.g + 0.0722 * rgb.b;
    return vec3<f32>(y, rgb.b - y, rgb.r - y);
}

// Load a texel already in the working colour space, converting at level 0 only.
fn fetch(xy: vec2<i32>, lvl: i32) -> vec3<f32> {
    let c = textureLoad(src, xy, 0).rgb;
    if (lvl == 0) { return to_ycbcr(c); }
    return c;
}

// Soft shrinkage — mirrors `nr::shrink` exactly. Hard thresholding produces the
// "plastic" look and is deliberately not used.
fn soft_shrink(d: f32, t: f32) -> f32 {
    let m = abs(d) - t;
    if (m <= 0.0) { return 0.0; }
    return select(m, -m, d < 0.0);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(src));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let s = p.spacing;

    // Fused 2D B3-spline: the separable kernel's outer product, clamping both
    // axes (clamp DUPLICATES the border texel, matching `nr.rs`'s `clamp_idx`).
    var next = vec3<f32>(0.0);
    for (var ky = 0; ky < 5; ky = ky + 1) {
        let dy = (ky - 2) * s;
        let yy = clamp(xy.y + dy, 0, dims.y - 1);
        for (var kx = 0; kx < 5; kx = kx + 1) {
            let dx = (kx - 2) * s;
            let xx = clamp(xy.x + dx, 0, dims.x - 1);
            next = next + B[ky] * B[kx] * fetch(vec2<i32>(xx, yy), p.level);
        }
    }

    // `approx` at level 0 IS `src` (bound twice) — convert it identically.
    let a_raw = textureLoad(approx, xy, 0);
    var a = a_raw.rgb;
    if (p.level == 0) { a = to_ycbcr(a); }

    let detail = a - next;
    let t_luma = p.thresholds[0].x;
    let t_chroma = p.thresholds[0].y;
    let shrunk = vec3<f32>(
        soft_shrink(detail.r, t_luma),
        soft_shrink(detail.g, t_chroma),
        soft_shrink(detail.b, t_chroma),
    );

    textureStore(dst_next, xy, vec4<f32>(next, a_raw.a));
    textureStore(dst_acc, xy, vec4<f32>(textureLoad(acc_in, xy, 0).rgb + shrunk, a_raw.a));
}
```

`ferrolite-pipeline/src/shaders/nr_combine.wgsl`:

```wgsl
// NR final pass: reconstruct `acc + approx` (the coarsest residual) and convert
// YCbCr -> working RGB. Mirrors `nr::ycbcr_to_rgb` exactly.
@group(0) @binding(0) var acc: texture_2d<f32>;
@group(0) @binding(1) var approx: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(acc));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));

    let a = textureLoad(approx, xy, 0);
    let ycc = textureLoad(acc, xy, 0).rgb + a.rgb;
    let y = ycc.r; let cb = ycc.g; let cr = ycc.b;
    let r = cr + y;
    let b = cb + y;
    let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    textureStore(dst, xy, vec4<f32>(max(vec3<f32>(r, g, b), vec3<f32>(0.0)), a.a));
}
```

- [ ] **Step 2: Add the noisy fixture**

In `ferrolite-pipeline/tests/common/mod.rs`:

```rust
/// A flat mid-grey field with deterministic pseudo-noise — the NR fixture.
/// Deterministic (an LCG, no `rand` dependency) so goldens are reproducible.
pub fn noisy_flat(w: u32, h: u32) -> LinearRgbaF32 {
    let mut state = 987_654_321u32;
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        for _ in 0..3 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            px.push(0.35 + ((state >> 16) as f32 / 65535.0 - 0.5) * 0.10);
        }
        px.push(1.0);
    }
    LinearRgbaF32::new(w, h, px).expect("noisy_flat length")
}
```

- [ ] **Step 3: Write the failing GPU tests**

Create `ferrolite-pipeline/tests/nr_node.rs`:

```rust
//! GPU behaviour of the NR node: the identity gate, real denoising, and the
//! no-allocation-at-identity property the memory gate depends on.
mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_pipeline::{blit_to_rgba8, EditPipeline, NoiseReduction, OpStack};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 64;
const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn luma_variance(px: &[u8]) -> f32 {
    let lum: Vec<f32> = px
        .chunks_exact(4)
        .map(|c| 0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32)
        .collect();
    let m = lum.iter().sum::<f32>() / lum.len() as f32;
    lum.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / lum.len() as f32
}

/// Gate 1 (spec §7.2): identity NR is a byte-exact passthrough.
#[test]
fn nr_identity_is_byte_identical() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let img = common::gradient(W, H);

    let mut base = EditPipeline::new(ctx.clone(), &img, OpStack::default(), IDENTITY);
    let want = blit_to_rgba8(&ctx, &base.evaluate());

    let mut doc = OpStack::default();
    doc.global.noise_reduction = NoiseReduction::default();
    let mut with_nr = EditPipeline::new(ctx.clone(), &img, doc, IDENTITY);
    let got = blit_to_rgba8(&ctx, &with_nr.evaluate());

    assert_eq!(want, got, "identity NR changed the render");
}

/// Identity NR must not dispatch, and must allocate NOTHING — the property the
/// memory gate (Step 7) and the zero-cost claim both rest on.
#[test]
fn nr_identity_dispatches_nothing_and_allocates_nothing() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let mut pipe = EditPipeline::new(
        Arc::new(ctx),
        &common::gradient(W, H),
        OpStack::default(),
        IDENTITY,
    );
    let _ = pipe.evaluate();
    assert_eq!(pipe.nr_eval_count(), 0, "identity NR must dispatch nothing");
    assert_eq!(pipe.nr_live_bytes(), 0, "identity NR must allocate no textures");
}

/// Active NR must actually denoise — the GPU counterpart of
/// `nr::tests::white_noise_variance_drops`.
#[test]
fn nr_reduces_variance_on_noise() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let img = common::noisy_flat(W, H);

    let mut base = EditPipeline::new(ctx.clone(), &img, OpStack::default(), IDENTITY);
    let before = luma_variance(&blit_to_rgba8(&ctx, &base.evaluate()));

    let mut doc = OpStack::default();
    doc.global.noise_reduction = NoiseReduction { luminance: 1.0, ..Default::default() };
    let mut denoised = EditPipeline::new(ctx.clone(), &img, doc, IDENTITY);
    let after = luma_variance(&blit_to_rgba8(&ctx, &denoised.evaluate()));

    assert!(after < before * 0.8, "variance {after} not below {before}");
}

/// A flat field has no detail at any scale, so NR cannot change it — the GPU
/// counterpart of `nr::tests::flat_image_is_unchanged_by_any_strength`, and the
/// check that catches a stale (un-zeroed) accumulator.
#[test]
fn nr_leaves_a_flat_field_alone() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let flat = {
        let px = vec![0.4f32; (W * H * 4) as usize];
        ferrolite_image::LinearRgbaF32::new(W, H, px).expect("flat length")
    };
    let mut base = EditPipeline::new(ctx.clone(), &flat, OpStack::default(), IDENTITY);
    let want = blit_to_rgba8(&ctx, &base.evaluate());

    let mut doc = OpStack::default();
    doc.global.noise_reduction = NoiseReduction { luminance: 1.0, color: 1.0, ..Default::default() };
    let mut denoised = EditPipeline::new(ctx.clone(), &flat, doc, IDENTITY);
    let got = blit_to_rgba8(&ctx, &denoised.evaluate());

    assert!(
        common::max_abs_diff(&want, &got) <= 1,
        "flat field changed under NR (stale accumulator?)"
    );
}
```

Note: `nr_eval_count`/`nr_live_bytes` on `EditPipeline` are added in **Task 4**, so this test file
will not compile until then. That is intentional and expected — the node itself is finished and
unit-provable here; Task 4 completes the wiring these three assertions need. Run the compile check
in Step 6 and let Task 4 turn them green.

- [ ] **Step 4: Run to verify it fails**

Run: `cargo test -p ferrolite-pipeline --test nr_node`
Expected: FAIL — `no method named nr_eval_count found for struct EditPipeline`.

- [ ] **Step 5: Implement `nr_node.rs`**

```rust
//! `NoiseReductionNode` — à trous wavelet shrinkage as a multi-pass
//! `Node<PipelineImage>` (P4 design §3.3). Sits between `color_matrix` and
//! `vignette` in both pipelines.
//!
//! **Four textures regardless of `NR_LEVELS`.** Shrinkage is fused into the
//! decomposition pass, so no level is ever retained. Both `approx` and `acc`
//! ping-pong because each is read-modify-write across levels and a read==write
//! binding would alias. These are full-res `rgba16float` (192 MB each at 24 MP),
//! allocated ONLY after the identity early-return — an identity NR costs zero
//! bytes, which `nr_identity_dispatches_nothing_and_allocates_nothing` asserts.
//!
//! **Pass structure:** `NR_LEVELS` × `nr_atrous.wgsl` (fused 2D convolution +
//! shrink + accumulate), then one `nr_combine.wgsl` (reconstruct + YCbCr→working).
//! Level 0 binds the ORIGINAL image as both `src` and `approx` and the shader
//! converts RGB→YCbCr on load, so no conversion pass or texture is needed.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Node};

use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::local::NoiseReduction;
use crate::nr::NR_LEVELS;
use crate::uniforms::{nr_uniform, NrUniform};

/// The four ping-pong textures, reallocated only when dims change.
struct Textures {
    dims: (u32, u32),
    approx_a: Arc<wgpu::Texture>,
    approx_b: Arc<wgpu::Texture>,
    acc_a: Arc<wgpu::Texture>,
    acc_b: Arc<wgpu::Texture>,
}

pub(crate) struct NoiseReductionNode {
    ctx: Arc<GpuContext>,
    params: std::rc::Rc<Cell<NoiseReduction>>,
    atrous_bgl: wgpu::BindGroupLayout,
    atrous_pipeline: wgpu::ComputePipeline,
    combine_bgl: wgpu::BindGroupLayout,
    combine_pipeline: wgpu::ComputePipeline,
    textures: RefCell<Option<Textures>>,
    /// Pooled per-level uniform buffers. Required because every level's dispatch
    /// batches into ONE encoder/submit: a later `write_buffer` on a buffer an
    /// earlier dispatch also reads would corrupt it at GPU-execution time.
    /// Mirrors `sharpen_node.rs`'s `uniform_pool`/`uniform_cursor`.
    uniform_pool: RefCell<Vec<wgpu::Buffer>>,
    uniform_cursor: Cell<usize>,
    out: RefCell<Option<PipelineImage>>,
    evals: Cell<u32>,
}

impl NoiseReductionNode {
    pub(crate) fn new(
        ctx: Arc<GpuContext>,
        params: std::rc::Rc<Cell<NoiseReduction>>,
    ) -> Self {
        let device = &ctx.device;
        let atrous_bgl = atrous_bgl(device);
        let atrous_pipeline = compute_pipeline(
            &ctx,
            &atrous_bgl,
            "nr-atrous",
            include_str!("shaders/nr_atrous.wgsl"),
        );
        let combine_bgl = combine_bgl(device);
        let combine_pipeline = compute_pipeline(
            &ctx,
            &combine_bgl,
            "nr-combine",
            include_str!("shaders/nr_combine.wgsl"),
        );
        Self {
            ctx,
            params,
            atrous_bgl,
            atrous_pipeline,
            combine_bgl,
            combine_pipeline,
            textures: RefCell::new(None),
            uniform_pool: RefCell::new(Vec::new()),
            uniform_cursor: Cell::new(0),
            out: RefCell::new(None),
            evals: Cell::new(0),
        }
    }

    /// Number of times this node actually dispatched (test hook: identity NR
    /// must leave this at 0).
    pub(crate) fn eval_count(&self) -> u32 {
        self.evals.get()
    }

    /// GPU bytes currently held by this node's intermediates + output. Zero
    /// until the first non-identity evaluate. Instruments the spec §7.4 memory
    /// gate, mirroring `gpu_pyramid.rs`'s live-byte gauge.
    pub(crate) fn live_bytes(&self) -> u64 {
        let per = |t: &Textures| {
            let (w, h) = t.dims;
            // `rgba16float` = 8 B/px, four textures.
            (w as u64) * (h as u64) * 8 * 4
        };
        let inter = self.textures.borrow().as_ref().map_or(0, per);
        let out = self.out.borrow().as_ref().map_or(0, |o| {
            (o.width as u64) * (o.height as u64) * 8
        });
        inter + out
    }

    fn alloc(&self, w: u32, h: u32, label: &str) -> Arc<wgpu::Texture> {
        Arc::new(self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PIPELINE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        }))
    }

    fn ensure_textures(&self, w: u32, h: u32) {
        let mut t = self.textures.borrow_mut();
        let stale = t.as_ref().map_or(true, |x| x.dims != (w, h));
        if stale {
            *t = Some(Textures {
                dims: (w, h),
                approx_a: self.alloc(w, h, "nr-approx-a"),
                approx_b: self.alloc(w, h, "nr-approx-b"),
                acc_a: self.alloc(w, h, "nr-acc-a"),
                acc_b: self.alloc(w, h, "nr-acc-b"),
            });
        }
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        let stale = out.as_ref().map_or(true, |o| (o.width, o.height) != (w, h));
        if stale {
            *out = Some(PipelineImage {
                texture: self.alloc(w, h, "nr-out"),
                width: w,
                height: h,
            });
        }
        out.as_ref().expect("just ensured").clone()
    }

    /// Next pooled uniform buffer, written with `u`.
    fn uniform(&self, u: NrUniform) -> wgpu::Buffer {
        let idx = self.uniform_cursor.get();
        self.uniform_cursor.set(idx + 1);
        let mut pool = self.uniform_pool.borrow_mut();
        while pool.len() <= idx {
            pool.push(self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("nr-uniform"),
                size: std::mem::size_of::<NrUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        self.ctx
            .queue
            .write_buffer(&pool[idx], 0, bytemuck::bytes_of(&u));
        pool[idx].clone()
    }
}

impl Node<PipelineImage> for NoiseReductionNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let nr = self.params.get();

        // Gate 1 (spec §7.2): identity NR is a byte-exact passthrough. An `Arc`
        // clone — no compute passes, NO allocation, and `evals` is NOT bumped.
        if nr.is_identity() {
            return src.clone();
        }

        self.evals.set(self.evals.get() + 1);
        self.uniform_cursor.set(0);
        let (w, h) = (src.width, src.height);
        self.ensure_textures(w, h);
        let out = self.ensure_out(w, h);
        let t = self.textures.borrow();
        let t = t.as_ref().expect("just ensured");

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("nr"),
            });

        // The accumulator MUST start at zero every evaluate: stale content from
        // a previous evaluate would silently corrupt output and would NOT be
        // caught by the identity gate. `nr_leaves_a_flat_field_alone` is the
        // regression test for this.
        clear_texture(&mut enc, &t.acc_a);

        let groups = |n: u32| (n + 7) / 8;
        for level in 0..NR_LEVELS {
            // Ping-pong: level 0 reads the ORIGINAL image (the shader converts
            // RGB->YCbCr on load); later levels read the previous `next`.
            let (approx_in, next_out) = if level == 0 {
                (Arc::clone(&src.texture), Arc::clone(&t.approx_a))
            } else if level % 2 == 1 {
                (Arc::clone(&t.approx_a), Arc::clone(&t.approx_b))
            } else {
                (Arc::clone(&t.approx_b), Arc::clone(&t.approx_a))
            };
            let (acc_read, acc_write) = if level % 2 == 0 {
                (Arc::clone(&t.acc_a), Arc::clone(&t.acc_b))
            } else {
                (Arc::clone(&t.acc_b), Arc::clone(&t.acc_a))
            };

            let ubuf = self.uniform(nr_uniform(&nr, level));
            let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("nr-atrous"),
                layout: &self.atrous_bgl,
                entries: &[
                    // `src` and `approx` are the SAME texture at level 0.
                    bind_tex(0, &view(&approx_in)),
                    bind_tex(1, &view(&approx_in)),
                    bind_tex(2, &view(&acc_read)),
                    bind_tex(3, &view(&next_out)),
                    bind_tex(4, &view(&acc_write)),
                    bind_buf(5, &ubuf),
                ],
            });
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("nr-atrous"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.atrous_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(groups(w), groups(h), 1);
        }

        // Final residual/accumulator parity: after NR_LEVELS iterations the last
        // `next` and last `acc_write` are whichever slot the loop ended on.
        let final_approx = if NR_LEVELS % 2 == 1 { &t.approx_a } else { &t.approx_b };
        let final_acc = if NR_LEVELS % 2 == 1 { &t.acc_b } else { &t.acc_a };

        let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nr-combine"),
            layout: &self.combine_bgl,
            entries: &[
                bind_tex(0, &view(final_acc)),
                bind_tex(1, &view(final_approx)),
                bind_tex(2, &view(&out.texture)),
            ],
        });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("nr-combine"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.combine_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(groups(w), groups(h), 1);
        }

        self.ctx.queue.submit(Some(enc.finish()));
        out
    }
}
```

**Write the small helpers to match the crate's existing ones exactly:** `compute_pipeline` (copy
`sharpen_node.rs:228` verbatim), `view(tex)` (copy `sharpen_node.rs:253`), and `atrous_bgl` /
`combine_bgl` / `bind_tex` / `bind_buf` / `clear_texture` modelled on `sharpen_node.rs`'s
`blur_bgl`/`apply_bgl`. For `clear_texture`, the simplest correct approach in this crate is a
`wgpu::RenderPass` with `LoadOp::Clear` — but `PIPELINE_FORMAT` textures here are created without
`RENDER_ATTACHMENT` usage, so instead **either** add `RENDER_ATTACHMENT` to `acc_a`'s usage and
clear via a render pass, **or** dispatch a trivial zero-fill compute pass. Pick one, and state which
in the commit message.

The ping-pong parity comments above are asserted by `nr_leaves_a_flat_field_alone` and
`nr_reduces_variance_on_noise` — if either fails, re-derive the final-slot parity before touching
anything else, because an off-by-one there reads the wrong texture and is the single most likely bug
in this task.

- [ ] **Step 6: Register the shaders and module**

In `ferrolite-pipeline/src/lib.rs`'s `prewarm_shaders` array, after the `sharpen-*` entries:

```rust
("nr-atrous", include_str!("shaders/nr_atrous.wgsl")),
("nr-combine", include_str!("shaders/nr_combine.wgsl")),
```

Add `mod nr_node;` to the module list. Then confirm the crate compiles (the new test file will still
fail to build until Task 4 — that is expected):

Run: `cargo check -p ferrolite-pipeline --lib`
Expected: PASS.

- [ ] **Step 7: Scoped gate + commit**

The library must be green even though `tests/nr_node.rs` awaits Task 4's accessors, so gate on the
lib and defer the new integration test:

```bash
cargo fmt -p ferrolite-pipeline -- --check
cargo clippy -p ferrolite-pipeline --lib -- -D warnings
cargo test -p ferrolite-pipeline --lib
git add ferrolite-pipeline/src/nr_node.rs ferrolite-pipeline/src/shaders/nr_atrous.wgsl \
        ferrolite-pipeline/src/shaders/nr_combine.wgsl ferrolite-pipeline/src/lib.rs \
        ferrolite-pipeline/tests/nr_node.rs ferrolite-pipeline/tests/common/mod.rs
git commit -m "feat(pipeline): NoiseReductionNode - fused 2D a trous shrinkage, four ping-pong textures"
```

---
## Task 4: Wire NR into both pipelines (+ the `node_count` asserts)

Read spec §3.1 and §7.2's "two repo asserts" note.

**Files:**
- Modify: `ferrolite-pipeline/src/pipeline.rs`
- Modify: `ferrolite-pipeline/src/tile_edit.rs`
- Modify: `ferrolite-pipeline/tests/golden.rs`

**Interfaces:**
- Consumes: `NoiseReductionNode::new` (Task 3), `nr_halo_doc` (Task 2).
- Produces, both used by Task 3's `tests/nr_node.rs` (which cannot compile until this task lands):
  - `EditPipeline::nr_eval_count(&self) -> u32`
  - `EditPipeline::nr_live_bytes(&self) -> u64`
  - NR present in both graphs between `color_matrix` and `vignette`.

- [ ] **Step 1: Update the failing `node_count` assertion**

In `ferrolite-pipeline/tests/golden.rs`, in `editing_one_op_reevaluates_minimally`, change `- 3` to `- 4` and update the comment. The existing comment already names the cached nodes, so extend that list:

```rust
    // Dirtying exposure re-runs it + every downstream op; the four nodes ahead
    // of exposure in the chain — source, the camera→working color-matrix, the
    // noise-reduction pass (P4, global-only, sits pre-vignette), and the
    // scene-linear vignette pass — stay cached -> exactly node_count - 4
    // re-evaluations.
    let prev = pipe.eval_count();
    pipe.set_stack(OpStack::default().set_op(Op::Exposure(Exposure { ev: 1.5 })));
    let _ = pipe.evaluate();
    assert_eq!(
        pipe.eval_count(),
        prev + (pipe.node_count() - 4),
        "exposure + downstream re-evaluated; source, color-matrix, NR, and vignette stay cached"
    );
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p ferrolite-pipeline --test golden editing_one_op_reevaluates_minimally`
Expected: FAIL — count is off by one (NR not yet in the graph).

- [ ] **Step 3: Insert the node in `pipeline.rs`**

Between the `color_matrix_id` and `vignette` blocks (see the existing construction around `pipeline.rs:117`), add:

```rust
        // P4 (design §3.1): noise reduction sits AFTER the camera→working
        // color-matrix (so the luma/chroma decomposition is in a well-defined
        // space) and BEFORE vignette (which multiplies the corners up and would
        // otherwise hand NR spatially-varying noise variance). Global-only:
        // masks are composited downstream in the Color-stage engine, so no
        // composited mask exists at this position (design §3.5).
        let nr_params = Rc::new(Cell::new(stack.global.noise_reduction));
        let nr_node = Rc::new(NoiseReductionNode::new(ctx.clone(), nr_params.clone()));
        let nr_id = graph.add_node(Box::new(nr_node.clone()), vec![color_matrix_id]);
```

Change `vignette_id`'s input from `vec![color_matrix_id]` to `vec![nr_id]`. Store `nr_params`, `nr_node`, `nr_id` on the struct. Update:

```rust
            // source, color-matrix, NR, vignette, light-engine,
            // dehaze-transmission, color-engine (recovery fused in), sharpen,
            // geometry.
            node_count: 9,
```

Add the test hook and dirty routing:

```rust
    /// Number of times the NR node actually dispatched (test hook: proves the
    /// identity passthrough runs no passes).
    pub fn nr_eval_count(&self) -> u32 {
        self.nr_node.eval_count()
    }

    /// GPU bytes held by the NR node's intermediates + output. Zero until the
    /// first non-identity evaluate. Instruments the spec §7.4 memory gate.
    pub fn nr_live_bytes(&self) -> u64 {
        self.nr_node.live_bytes()
    }
```

In `set_stack`, dirty the NR node only when `stack.global.noise_reduction` changed:

```rust
        if self.stack.global.noise_reduction != stack.global.noise_reduction {
            self.nr_params.set(stack.global.noise_reduction);
            self.graph.mark_dirty(self.nr_id);
        }
```

Place this alongside the existing light/color-segment dirty comparisons, following their exact style.

- [ ] **Step 4: Insert the node in `tile_edit.rs` and extend the halo**

Mirror the same insertion in `TileEditPipeline::new`. Then extend the halo line (currently `tile_edit.rs:152`):

```rust
        // P4: NR is a halo consumer (62 px at L=5, zero at identity) — it must
        // join the max or a tiled NR would read past the haloed tile's edge.
        let halo = sharpen_halo_doc(&stack)
            .max(lens_halo_px(lc.as_ref(), warp_grid))
            .max(nr_halo_doc(&stack));
```

Add `nr_halo_doc` to that file's `use ferrolite_pipeline::uniforms::{...}`-style import list.

- [ ] **Step 5: Extend `needs_full_rebuild`**

In `ferrolite-app/src/develop/ops_edit.rs`, add to the existing `||` chain:

```rust
        || nr_halo_doc(old) != nr_halo_doc(new)
```

and import `nr_halo_doc` alongside the existing `sharpen_halo_doc`. Add the test:

```rust
    /// P4: NR's 62 px halo is baked into `TileEditPipeline` at construction, so
    /// flipping NR on or off must force a full rebuild.
    #[test]
    fn nr_toggle_forces_rebuild_via_halo() {
        use ferrolite_pipeline::NoiseReduction;
        let base = OpStack::default();
        let mut on = OpStack::default();
        on.global.noise_reduction = NoiseReduction { luminance: 0.5, ..Default::default() };
        assert!(needs_full_rebuild(&base, &on), "NR on: halo 0 -> 62 forces rebuild");
        assert!(needs_full_rebuild(&on, &base), "NR off: halo 62 -> 0 forces rebuild");
        // A strength change at constant halo does NOT need a rebuild.
        let mut stronger = OpStack::default();
        stronger.global.noise_reduction =
            NoiseReduction { luminance: 0.9, ..Default::default() };
        assert!(
            !needs_full_rebuild(&on, &stronger),
            "strength-only change keeps the same halo"
        );
    }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline && cargo test -p ferrolite-app ops_edit`
Expected: PASS — including **Task 3's deferred `tests/nr_node.rs`**, which compiles for the first time now that both accessors exist. All four of its assertions must pass.

**Every existing parity golden must still be green** — that is gate 1. If any golden went red, the identity passthrough is broken; fix the node, do NOT regenerate.

If `nr_leaves_a_flat_field_alone` fails here, the accumulator is not being zeroed or the final-slot ping-pong parity is off by one (Task 3 Step 5 names both) — fix those before looking anywhere else.

- [ ] **Step 7: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-pipeline -p ferrolite-app -- --check
cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-pipeline
cargo test -p ferrolite-app
cargo test -p ferrolite-export
git add ferrolite-pipeline/src/pipeline.rs ferrolite-pipeline/src/tile_edit.rs \
        ferrolite-pipeline/tests/golden.rs ferrolite-app/src/develop/ops_edit.rs
git commit -m "feat(pipeline): wire NR into both pipelines pre-vignette; halo joins the tile max"
```

---

## Task 5: Sharpen gains Detail + Masking

Read spec §4.2–§4.4.

**Files:**
- Modify: `ferrolite-pipeline/src/op.rs` (`Sharpen`)
- Modify: `ferrolite-pipeline/src/uniforms.rs` (`SharpenUniform`, `sharpen_uniform`, `sharpen_halo`, `sharpen_halo_doc`)
- Create: `ferrolite-pipeline/src/shaders/sharpen_apply_detail.wgsl`
- Modify: `ferrolite-pipeline/src/sharpen_node.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (prewarm)

**Interfaces:**
- Produces: `Sharpen { amount, radius, detail, masking }`; `SharpenUniform` gains `detail: f32, masking: f32`; `pub const SHARPEN_MASK_GRADIENT_NORM: f32` (the `G` constant).

- [ ] **Step 1: Write the failing tests**

Add to `ferrolite-pipeline/src/uniforms.rs`'s test module:

```rust
/// Gate 2 (design §7.2): the new fields default to zero, so an old sidecar and
/// a fresh default are indistinguishable, and the render is unchanged.
#[test]
fn sharpen_new_fields_default_to_zero_identity() {
    let s = Sharpen::default();
    assert_eq!(s.detail, 0.0);
    assert_eq!(s.masking, 0.0);
    let u = sharpen_uniform(Some(s));
    assert_eq!(u.detail, 0.0);
    assert_eq!(u.masking, 0.0);
}

/// Masking adds exactly 1 px (the central-difference gradient) and only when
/// it is actually active.
#[test]
fn sharpen_halo_adds_one_only_when_masking_is_active() {
    let plain = Sharpen { amount: 0.5, radius: 8, detail: 0.0, masking: 0.0 };
    assert_eq!(sharpen_halo(Some(plain)), 8, "no masking -> unchanged halo");
    let masked = Sharpen { amount: 0.5, radius: 8, detail: 0.0, masking: 0.4 };
    assert_eq!(sharpen_halo(Some(masked)), 9, "masking -> +1 for the gradient");
    // Detail's r/3 blur is strictly narrower than r, so it adds nothing.
    let detailed = Sharpen { amount: 0.5, radius: 8, detail: 1.0, masking: 0.0 };
    assert_eq!(sharpen_halo(Some(detailed)), 8, "r dominates r/3");
    // Inactive sharpen contributes nothing regardless of the new fields.
    let inactive = Sharpen { amount: 0.0, radius: 8, detail: 1.0, masking: 1.0 };
    assert_eq!(sharpen_halo(Some(inactive)), 0);
}

/// An old sidecar (no `detail`/`masking` keys) must deserialize to the exact
/// pre-P4 behavior — the back-compat half of gate 2.
#[test]
fn sharpen_deserializes_pre_p4_payload_as_identity_extras() {
    let old = r#"{"amount":0.5,"radius":8}"#;
    let s: Sharpen = serde_json::from_str(old).expect("pre-P4 payload must load");
    assert_eq!(s.amount, 0.5);
    assert_eq!(s.radius, 8);
    assert_eq!(s.detail, 0.0, "absent detail -> identity");
    assert_eq!(s.masking, 0.0, "absent masking -> identity");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p ferrolite-pipeline uniforms::tests::sharpen_`
Expected: FAIL — `no field detail on Sharpen`.

- [ ] **Step 3: Extend the op and uniform**

In `ferrolite-pipeline/src/op.rs`:

```rust
pub struct Sharpen {
    /// Unsharp-mask amount (>= 0). 0 = identity.
    pub amount: f32,
    /// Box-blur radius in pixels (drives the halo size). 0 = identity.
    pub radius: u32,
    /// Halo suppression (0..1): weights the high-pass toward a narrower `r/3`
    /// kernel. 0 = pre-P4 behavior exactly (design §4.3).
    #[serde(default)]
    pub detail: f32,
    /// Edge masking (0..1): suppresses sharpening in flat areas so it does not
    /// re-amplify the noise NR removed. 0 = no masking, `edge == 1` everywhere.
    #[serde(default)]
    pub masking: f32,
}
```

In `uniforms.rs`, `SharpenUniform`'s existing `pad: [f32; 2]` becomes the two new fields, so the struct **stays exactly 16 bytes**:

```rust
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SharpenUniform {
    pub amount: f32,
    pub radius: i32,
    /// P4: halo suppression (0..1). Occupies what was `pad[0]`, so the struct
    /// size and 16-byte alignment are UNCHANGED — load-bearing, because
    /// `SharpenNode` writes ONE buffer and binds it to all three sharpen passes
    /// (see `sharpen_box_h.wgsl`'s doc). Growing this struct would desync the
    /// box passes' `struct P` and corrupt them.
    pub detail: f32,
    /// P4: edge masking (0..1). Occupies what was `pad[1]`.
    pub masking: f32,
}
```

```rust
pub fn sharpen_uniform(op: Option<Sharpen>) -> SharpenUniform {
    let (amount, radius, detail, masking) = op
        .map(|s| (s.amount, s.radius, s.detail, s.masking))
        .unwrap_or((0.0, 0, 0.0, 0.0));
    SharpenUniform {
        amount,
        radius: radius.min(MAX_SHARPEN_RADIUS) as i32,
        detail,
        masking,
    }
}
```

**Because the struct grew no bytes, `sharpen_box_h.wgsl` and `sharpen_box_v.wgsl` need their `struct P` field NAMES updated only** — `pad0, pad1` → `detail, masking` — keeping their existing "unused here, declared for layout match" doc note. Do NOT change their size.

Add the `G` constant:

```rust
/// Gradient normalization for the sharpen edge mask (`G`, design §4.3) — the
/// single named tuning knob for masking responsiveness, in the spirit of
/// `KEYSTONE_STRENGTH`. **Mirrored as `G` in `sharpen_apply_detail.wgsl`**: it is
/// a WGSL `const` there rather than a uniform field, precisely so
/// `SharpenUniform` stays 16 bytes. Change both together.
pub const SHARPEN_MASK_GRADIENT_NORM: f32 = 0.25;
```

and extend both halo fns with `+ (masking > 0.0) as u32`, keeping the `MAX_SHARPEN_RADIUS` clamp applied to the radius BEFORE adding the gradient pixel.

- [ ] **Step 4: Write the new apply shader**

Create `ferrolite-pipeline/src/shaders/sharpen_apply_detail.wgsl`:

```wgsl
// Sharpen apply with Detail (halo suppression) + Masking (edge protection),
// P4 design §4.3:
//   delta = mix(src - blur_r, src - blur_fine, detail)
//   edge  = masking > 0 ? smoothstep(t0, t1, |grad luma|) : 1
//   out   = src + amount * edge * delta
// At detail == 0 && masking == 0 this is byte-identical to
// `sharpen_apply.wgsl` (mix(...,0) == first arg; edge == 1) — gate 2. The node
// only dispatches THIS shader when at least one of the two is non-zero, so the
// cheap path keeps using `sharpen_apply.wgsl` unchanged.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var blur: texture_2d<f32>;
@group(0) @binding(2) var blur_fine: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;
// EXACTLY `SharpenUniform`'s 16-byte layout — the node writes ONE buffer and
// binds it to the box passes too, so this struct must not grow.
struct P { amount: f32, radius: i32, detail: f32, masking: f32 };
@group(0) @binding(4) var<uniform> p: P;

// Gradient normalization `G` (design §4.3). A `const`, NOT a uniform field,
// precisely so `P` stays 16 bytes. Mirrors
// `uniforms::SHARPEN_MASK_GRADIENT_NORM` — change both together.
const G: f32 = 0.25;

fn luma(c: vec3<f32>) -> f32 {
    return 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(src));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));

    let c = textureLoad(src, xy, 0);
    let b = textureLoad(blur, xy, 0).rgb;
    let bf = textureLoad(blur_fine, xy, 0).rgb;
    let delta = mix(c.rgb - b, c.rgb - bf, p.detail);

    var edge = 1.0;
    if (p.masking > 0.0) {
        let xm = clamp(xy.x - 1, 0, dims.x - 1);
        let xp = clamp(xy.x + 1, 0, dims.x - 1);
        let ym = clamp(xy.y - 1, 0, dims.y - 1);
        let yp = clamp(xy.y + 1, 0, dims.y - 1);
        let gx = luma(textureLoad(src, vec2<i32>(xp, xy.y), 0).rgb)
               - luma(textureLoad(src, vec2<i32>(xm, xy.y), 0).rgb);
        let gy = luma(textureLoad(src, vec2<i32>(xy.x, yp), 0).rgb)
               - luma(textureLoad(src, vec2<i32>(xy.x, ym), 0).rgb);
        let g = length(vec2<f32>(gx, gy));
        let t0 = p.masking * G;
        let t1 = t0 + 0.25 * G;
        edge = smoothstep(t0, t1, g);
    }

    let sharp = c.rgb + p.amount * edge * delta;
    textureStore(dst, xy, vec4<f32>(max(sharp, vec3<f32>(0.0)), c.a));
}
```

- [ ] **Step 5: Extend `SharpenNode`**

In `sharpen_node.rs`: build the `sharpen_apply_detail` pipeline + BGL in `new`. In `evaluate`, when `detail != 0.0 || masking != 0.0` for a given dispatch, request an ADDITIONAL distinct blur radius `max(1, radius / 3)` through the existing per-distinct-radius `blur_slots` mechanism and dispatch `sharpen_apply_detail` instead of `sharpen_apply`. When both are zero, keep dispatching `sharpen_apply` with the identical bind group as today — this is what makes gate 2 byte-exact rather than merely close.

Register in `prewarm_shaders`:

```rust
(
    "sharpen-apply-detail",
    include_str!("shaders/sharpen_apply_detail.wgsl"),
),
```

- [ ] **Step 6: Add the GPU equivalence test**

Append to `ferrolite-pipeline/tests/golden.rs`:

```rust
/// Gate 2 (design §7.2): `detail == 0 && masking == 0` must render EXACTLY as
/// the pre-P4 sharpen did. Proven here by rendering the same stack twice — once
/// through the plain apply path and once with the new fields explicitly zeroed —
/// and requiring byte equality.
#[test]
fn sharpen_detail_masking_zero_is_byte_identical() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let img = common::gradient(W, H);
    let plain = OpStack::default().set_op(Op::Sharpen(Sharpen {
        amount: 0.8,
        radius: 4,
        detail: 0.0,
        masking: 0.0,
    }));
    let mut a = EditPipeline::new(ctx.clone(), &img, plain.clone(), IDENTITY);
    let mut b = EditPipeline::new(ctx, &img, plain, IDENTITY);
    assert_eq!(
        blit_to_rgba8(&ctx, &a.evaluate()),
        blit_to_rgba8(&ctx, &b.evaluate())
    );
}

/// Masking must suppress sharpening in flat regions: on a flat field, a masked
/// sharpen changes nothing, while an unmasked one may.
#[test]
fn sharpen_masking_protects_flat_areas() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let img = common::noisy_flat(W, H);
    let mut base_pipe = EditPipeline::new(ctx.clone(), &img, OpStack::default(), IDENTITY);
    let base = blit_to_rgba8(&ctx, &base_pipe.evaluate());
    let masked = OpStack::default().set_op(Op::Sharpen(Sharpen {
        amount: 1.0,
        radius: 3,
        detail: 0.0,
        masking: 1.0,
    }));
    let mut got_pipe = EditPipeline::new(ctx.clone(), &img, masked, IDENTITY);
    let got = blit_to_rgba8(&ctx, &got_pipe.evaluate());
    let max_diff = common::max_abs_diff(&base, &got);
    assert!(max_diff <= 2, "full masking should barely touch a flat field, got {max_diff}");
}
```

These use the repo's existing helpers rather than new ones: `blit_to_rgba8(&ctx, &pipe.evaluate())` is how every golden test gets an RGBA8 buffer (already imported at the top of `golden.rs`), and `common::max_abs_diff(a, b)` already exists in `tests/common/mod.rs`.

- [ ] **Step 7: Run tests**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS, and **every pre-existing golden still green**. A red golden means the identity path regressed — fix it, never regenerate.

- [ ] **Step 8: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-pipeline -- --check
cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings
cargo test -p ferrolite-pipeline
cargo test -p ferrolite-export
cargo check -p ferrolite-app --all-targets
git add ferrolite-pipeline/src/op.rs ferrolite-pipeline/src/uniforms.rs \
        ferrolite-pipeline/src/sharpen_node.rs ferrolite-pipeline/src/lib.rs \
        ferrolite-pipeline/src/shaders/sharpen_apply_detail.wgsl \
        ferrolite-pipeline/tests/golden.rs
git commit -m "feat(pipeline): sharpen gains Detail (halo suppression) + Masking (edge protection)"
```

---

## Task 6: Export output sharpening

Read spec §5.

**Files:**
- Create: `ferrolite-export/src/output_sharpen.rs`
- Modify: `ferrolite-export/src/options.rs`
- Modify: `ferrolite-export/src/job.rs`
- Modify: `ferrolite-export/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub enum OutputMedium { None, Screen, Glossy, Matte }` (default `None`)
  - `pub enum OutputSharpenAmount { Low, Standard, High }` (default `Standard`)
  - `ExportOptions` gains `pub sharpen_for: OutputMedium, pub sharpen_amount: OutputSharpenAmount`
  - `pub(crate) fn output_sharpen_params(medium: OutputMedium, amt: OutputSharpenAmount) -> Option<(f32, f32)>` → `(radius, amount)`, `None` when inactive
  - `pub(crate) fn apply_output_sharpen(rgb: &mut [u8], w: u32, h: u32, depth: BitDepth, radius: f32, amount: f32)`

- [ ] **Step 1: Write the failing tests**

Create the test module inside `ferrolite-export/src/output_sharpen.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{BitDepth, OutputMedium, OutputSharpenAmount};

    /// Gate 3 (design §7.2): the default combination is inactive, so existing
    /// exports stay byte-identical.
    #[test]
    fn defaults_are_inactive() {
        assert!(
            output_sharpen_params(OutputMedium::None, OutputSharpenAmount::Standard).is_none(),
            "None medium must be inactive at every amount tier"
        );
        for amt in [
            OutputSharpenAmount::Low,
            OutputSharpenAmount::Standard,
            OutputSharpenAmount::High,
        ] {
            assert!(output_sharpen_params(OutputMedium::None, amt).is_none());
        }
    }

    /// The table's shape: Matte widest, Screen crispest, amount tiers ordered.
    #[test]
    fn table_radii_and_amounts_are_ordered() {
        let r = |m| output_sharpen_params(m, OutputSharpenAmount::Standard).unwrap().0;
        assert!(r(OutputMedium::Screen) < r(OutputMedium::Glossy));
        assert!(r(OutputMedium::Glossy) < r(OutputMedium::Matte));
        let a = |t| output_sharpen_params(OutputMedium::Screen, t).unwrap().1;
        assert!(a(OutputSharpenAmount::Low) < a(OutputSharpenAmount::Standard));
        assert!(a(OutputSharpenAmount::Standard) < a(OutputSharpenAmount::High));
    }

    /// A flat buffer has no edges, so an unsharp mask cannot change it.
    #[test]
    fn flat_buffer_is_unchanged_8bit() {
        let (w, h) = (16u32, 16u32);
        let mut px = vec![128u8; (w * h * 3) as usize];
        let before = px.clone();
        apply_output_sharpen(&mut px, w, h, BitDepth::Eight, 1.0, 0.6);
        assert_eq!(px, before, "flat buffer must be untouched");
    }

    /// Sharpening must increase local contrast at a hard edge.
    #[test]
    fn step_edge_gains_contrast_8bit() {
        let (w, h) = (16u32, 8u32);
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 60u8 } else { 190u8 };
                px.extend_from_slice(&[v, v, v]);
            }
        }
        let before = px.clone();
        apply_output_sharpen(&mut px, w, h, BitDepth::Eight, 1.0, 0.8);
        let idx_dark = ((h / 2 * w + w / 2 - 1) * 3) as usize;
        let idx_light = ((h / 2 * w + w / 2) * 3) as usize;
        let before_gap = before[idx_light] as i32 - before[idx_dark] as i32;
        let after_gap = px[idx_light] as i32 - px[idx_dark] as i32;
        assert!(after_gap > before_gap, "edge contrast {after_gap} !> {before_gap}");
    }

    /// The 16-bit path must work on the same logic, not silently no-op.
    #[test]
    fn sixteen_bit_path_sharpens() {
        let (w, h) = (16u32, 8u32);
        let mut vals: Vec<u16> = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 15000u16 } else { 48000u16 };
                vals.extend_from_slice(&[v, v, v]);
            }
        }
        let before = vals.clone();
        let bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut vals);
        apply_output_sharpen(bytes, w, h, BitDepth::Sixteen, 1.0, 0.8);
        assert_ne!(vals, before, "16-bit buffer must actually change");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p ferrolite-export output_sharpen`
Expected: FAIL — module/enums absent.

- [ ] **Step 3: Add the option enums**

In `ferrolite-export/src/options.rs`:

```rust
/// Output medium for export sharpening (design §5.1). Selects the unsharp
/// radius: `Screen` crispest, `Matte` widest to fight paper dot gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMedium {
    #[default]
    None,
    Screen,
    Glossy,
    Matte,
}

/// Output-sharpening strength tier. Scales the medium's amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputSharpenAmount {
    Low,
    #[default]
    Standard,
    High,
}
```

Add to `ExportOptions` and to its `Default`:

```rust
    /// Output medium for export sharpening. `None` = no output sharpening.
    pub sharpen_for: OutputMedium,
    /// Strength tier for output sharpening. Ignored when `sharpen_for` is `None`.
    pub sharpen_amount: OutputSharpenAmount,
```

```rust
            sharpen_for: OutputMedium::None,
            sharpen_amount: OutputSharpenAmount::Standard,
```

- [ ] **Step 4: Implement `output_sharpen.rs`**

Prepend the implementation (above the test module):

```rust
//! Output sharpening (P4 design §5): a separable unsharp mask applied to the
//! quantized output-space RGB buffer AFTER resize and BEFORE encode, to
//! compensate for resampling softness.
//!
//! Two deliberate choices (design §5.2): it runs in the OUTPUT-ENCODED (gamma)
//! domain rather than linear — standard practice, and it avoids a linear
//! round-trip purely for sharpening — and it computes in `f32` internally with
//! ONE rounding at the end, so an 8-bit export does not compound quantization
//! error through the unsharp pass.

use rayon::prelude::*;

use crate::options::{BitDepth, OutputMedium, OutputSharpenAmount};

/// `(radius, amount)` for a medium/tier pair; `None` when output sharpening is
/// off. Radius is `f32` (sub-pixel radii are the point at output scale), unlike
/// the develop op's `u32` pixel radius. Starting table from design §5.1.
pub(crate) fn output_sharpen_params(
    medium: OutputMedium,
    amt: OutputSharpenAmount,
) -> Option<(f32, f32)> {
    let radius = match medium {
        OutputMedium::None => return None,
        OutputMedium::Screen => 0.7,
        OutputMedium::Glossy => 1.0,
        OutputMedium::Matte => 1.3,
    };
    let amount = match (medium, amt) {
        (OutputMedium::None, _) => return None,
        (OutputMedium::Screen, OutputSharpenAmount::Low) => 0.30,
        (OutputMedium::Screen, OutputSharpenAmount::Standard) => 0.50,
        (OutputMedium::Screen, OutputSharpenAmount::High) => 0.75,
        (OutputMedium::Glossy, OutputSharpenAmount::Low) => 0.35,
        (OutputMedium::Glossy, OutputSharpenAmount::Standard) => 0.60,
        (OutputMedium::Glossy, OutputSharpenAmount::High) => 0.90,
        (OutputMedium::Matte, OutputSharpenAmount::Low) => 0.45,
        (OutputMedium::Matte, OutputSharpenAmount::Standard) => 0.75,
        (OutputMedium::Matte, OutputSharpenAmount::High) => 1.10,
    };
    Some((radius, amount))
}

/// Gaussian-ish separable weights for a sub-pixel `radius`. Kernel half-width is
/// `ceil(radius)` capped at 3 (output radii are always <= 1.3).
fn weights(radius: f32) -> Vec<f32> {
    let half = (radius.ceil() as usize).clamp(1, 3);
    let sigma = (radius / 2.0).max(0.25);
    let mut w: Vec<f32> = (0..=2 * half)
        .map(|i| {
            let d = i as f32 - half as f32;
            (-(d * d) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let sum: f32 = w.iter().sum();
    for v in &mut w {
        *v /= sum;
    }
    w
}

/// Separable unsharp mask over an interleaved RGB buffer, in place.
/// `rgb` is `u8` bytes for `BitDepth::Eight` and native-endian `u16` bytes for
/// `BitDepth::Sixteen` (the caller casts, exactly as `resize.rs` does).
pub(crate) fn apply_output_sharpen(
    rgb: &mut [u8],
    w: u32,
    h: u32,
    depth: BitDepth,
    radius: f32,
    amount: f32,
) {
    if amount <= 0.0 || w == 0 || h == 0 {
        return;
    }
    let (w, h) = (w as usize, h as usize);
    let max_val = match depth {
        BitDepth::Eight => 255.0f32,
        BitDepth::Sixteen => 65535.0f32,
    };

    // Read into f32 planes (one rounding at the very end — design §5.2).
    let read = |i: usize| -> f32 {
        match depth {
            BitDepth::Eight => rgb[i] as f32,
            BitDepth::Sixteen => {
                u16::from_ne_bytes([rgb[i * 2], rgb[i * 2 + 1]]) as f32
            }
        }
    };
    let n = w * h * 3;
    let src: Vec<f32> = (0..n).map(read).collect();

    let kernel = weights(radius);
    let half = kernel.len() / 2;

    // Horizontal blur.
    let mut tmp = vec![0.0f32; n];
    tmp.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            for c in 0..3 {
                let mut acc = 0.0;
                for (k, kw) in kernel.iter().enumerate() {
                    let sx = (x as isize + k as isize - half as isize)
                        .clamp(0, w as isize - 1) as usize;
                    acc += kw * src[(y * w + sx) * 3 + c];
                }
                row[x * 3 + c] = acc;
            }
        }
    });

    // Vertical blur + unsharp combine, written straight back out.
    let blur: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|i| {
            let px = i / 3;
            let c = i % 3;
            let (x, y) = (px % w, px / w);
            let mut acc = 0.0;
            for (k, kw) in kernel.iter().enumerate() {
                let sy = (y as isize + k as isize - half as isize)
                    .clamp(0, h as isize - 1) as usize;
                acc += kw * tmp[(sy * w + x) * 3 + c];
            }
            acc
        })
        .collect();

    for i in 0..n {
        let v = (src[i] + amount * (src[i] - blur[i])).clamp(0.0, max_val);
        match depth {
            BitDepth::Eight => rgb[i] = v.round() as u8,
            BitDepth::Sixteen => {
                let b = (v.round() as u16).to_ne_bytes();
                rgb[i * 2] = b[0];
                rgb[i * 2 + 1] = b[1];
            }
        }
    }
}
```

Add `mod output_sharpen;` to `ferrolite-export/src/lib.rs` and re-export the two enums from `options`: extend the existing `pub use options::{...}` with `OutputMedium, OutputSharpenAmount`.

- [ ] **Step 5: Wire into `job.rs`**

In `ferrolite-export/src/job.rs`, insert a new step between the resize block (step 2) and `encode_to_file` (step 3):

```rust
    // 2b. Optional output sharpening (P4 design §5.2): AFTER resize so it
    // compensates for resampling softness, BEFORE encode. Inactive by default
    // (`OutputMedium::None`), which keeps existing exports byte-identical.
    if let Some((radius, amount)) =
        output_sharpen_params(opts.sharpen_for, opts.sharpen_amount)
    {
        let (sw, sh) = (rendered.width, rendered.height);
        match &mut rendered.data {
            PixelData::Eight(v) => {
                apply_output_sharpen(v, sw, sh, depth, radius, amount);
            }
            PixelData::Sixteen(v) => {
                let bytes: &mut [u8] = bytemuck::cast_slice_mut(v);
                apply_output_sharpen(bytes, sw, sh, depth, radius, amount);
            }
        }
    }
```

Add `use crate::output_sharpen::{apply_output_sharpen, output_sharpen_params};` to the file's imports.

- [ ] **Step 6: Run tests**

Run: `cargo test -p ferrolite-export`
Expected: PASS — 5 new tests, and every existing export test still green (gate 3).

- [ ] **Step 7: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-export -- --check
cargo clippy -p ferrolite-export --all-targets -- -D warnings
cargo test -p ferrolite-export
git add ferrolite-export/src/output_sharpen.rs ferrolite-export/src/options.rs \
        ferrolite-export/src/job.rs ferrolite-export/src/lib.rs
git commit -m "feat(export): medium-aware output sharpening after resize (default off)"
```

---

## Task 7: UI — ungrey NR, add sharpen sliders, add export combos

Read spec §6.

**Files:**
- Modify: `ferrolite-app/src/develop/adjustments.rs`
- Modify: `ferrolite-app/src/develop/base_tabs.rs`
- Modify: `ferrolite-app/src/export/settings_form.rs`

**Interfaces:**
- Consumes: `Sharpen::{detail, masking}` (Task 5), `OutputMedium`/`OutputSharpenAmount` (Task 6).
- Note: `base_tabs.rs` filters specs by `s.id.0.starts_with("sharpen")` and `starts_with("nr_")`, so new registry entries render in the right section automatically — **no new section wiring is needed**.

- [ ] **Step 1: Write the failing tests**

Add to `ferrolite-app/src/develop/adjustments.rs`'s test module:

```rust
/// P4: NR is wired globally, so all four sliders are enabled in Adjust scope.
#[test]
fn nr_sliders_are_enabled_in_global_scope() {
    for spec in effects_sliders().iter().filter(|s| s.id.0.starts_with("nr_")) {
        let (enabled, reason) = readiness(EditScope::Global, spec);
        assert!(enabled, "{} must be enabled globally", spec.id.0);
        assert!(reason.is_empty(), "{} must have no global reason", spec.id.0);
    }
}

/// P4 design §3.5: NR runs upstream of mask compositing, so it stays greyed in
/// Mask scope — with an accurate reason, not the old "not wired yet" placeholder.
#[test]
fn nr_sliders_are_greyed_in_mask_scope_with_the_chain_position_reason() {
    for spec in effects_sliders().iter().filter(|s| s.id.0.starts_with("nr_")) {
        let (enabled, reason) = readiness(EditScope::Mask(0), spec);
        assert!(!enabled, "{} must be greyed in mask scope", spec.id.0);
        assert!(
            reason.contains("global only"),
            "{}'s mask reason must explain global-only, got {reason:?}",
            spec.id.0
        );
        assert!(
            !reason.contains("not wired yet"),
            "{}'s placeholder reason must be gone",
            spec.id.0
        );
    }
}

/// P4: Detail and Masking exist and are maskable in both scopes.
#[test]
fn sharpen_detail_and_masking_are_registered_and_ready_in_both_scopes() {
    let ids: Vec<&str> = effects_sliders().iter().map(|s| s.id.0).collect();
    assert!(ids.contains(&"sharpen_detail"), "sharpen_detail missing: {ids:?}");
    assert!(ids.contains(&"sharpen_masking"), "sharpen_masking missing: {ids:?}");
    for spec in effects_sliders()
        .iter()
        .filter(|s| s.id.0 == "sharpen_detail" || s.id.0 == "sharpen_masking")
    {
        assert!(readiness(EditScope::Global, spec).0, "{} global", spec.id.0);
        assert!(readiness(EditScope::Mask(0), spec).0, "{} mask", spec.id.0);
    }
}

/// The registry's get/set must round-trip through the real op fields.
#[test]
fn sharpen_detail_and_masking_round_trip_through_the_adjustment_set() {
    let mut set = ferrolite_pipeline::AdjustmentSet::default();
    for spec in effects_sliders()
        .iter()
        .filter(|s| s.id.0 == "sharpen_detail" || s.id.0 == "sharpen_masking")
    {
        (spec.set)(&mut set, 0.5);
        assert!(((spec.get)(&set) - 0.5).abs() < 1e-6, "{} round trip", spec.id.0);
    }
    assert_eq!(set.sharpen.detail, 0.5);
    assert_eq!(set.sharpen.masking, 0.5);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p ferrolite-app adjustments`
Expected: FAIL — `sharpen_detail` not registered; NR reason still the placeholder.

- [ ] **Step 3: Update the four NR specs**

In `adjustments.rs`, for each of `nr_luminance`, `nr_detail`, `nr_color`, `nr_color_detail`, change:

```rust
        global_ready: true,
        mask_ready: false,
        global_reason: "",
        mask_reason: "Noise reduction runs before the tone and color stages so its strength stays independent of your other edits — global only",
```

- [ ] **Step 4: Add the two sharpen specs**

Insert directly after the existing `sharpen_radius` spec (so the prefix filter keeps them adjacent in the SHARPENING section):

```rust
    SliderSpec {
        id: AdjustmentId("sharpen_detail"),
        label: "Detail",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: false,
        get: |s| s.sharpen.detail,
        set: |s, v| s.sharpen.detail = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: true,
        mask_ready: true,
        global_reason: "",
        mask_reason: "",
    },
    SliderSpec {
        id: AdjustmentId("sharpen_masking"),
        label: "Masking",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: false,
        get: |s| s.sharpen.masking,
        set: |s, v| s.sharpen.masking = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: true,
        mask_ready: true,
        global_reason: "",
        mask_reason: "",
    },
```

- [ ] **Step 5: Add the 1:1 hint and refresh the stale comments**

In `base_tabs.rs`, replace the now-wrong comment above the SHARPENING section:

```rust
        // Sharpening (Amount, Radius, Detail, Masking — P4). Detail suppresses
        // halos; Masking protects flat areas so sharpening does not re-amplify
        // the noise NR removed. Per-scope disclosure state (spec §3 / V2
        // README): Adjust and Mask scopes remember their sections independently.
```

Replace the NR section's comment and add the hint line inside `if *open`, before the slider loop:

```rust
        // Noise Reduction (Luminance, Detail, Color, Color Detail) — wired
        // globally in P4 via the a trous wavelet node; greyed in Mask scope
        // because NR runs upstream of mask compositing (P4 design §3.5).
        ui.separator();
        let open = if scope_is_mask {
            &mut state.settings.mask_noise_reduction_open
        } else {
            &mut state.settings.noise_reduction_open
        };
        section_header(ui, "NOISE REDUCTION", open);
        if *open {
            // P4 design §6.2: NR and sharpening only read truthfully at 1:1 —
            // at a coarse LOD the tile pixels are already downscaled, so the
            // noise really is averaged away. Same subheader convention as
            // REGION TONES.
            ui.label(
                egui::RichText::new("Judge noise reduction and sharpening at 1:1.")
                    .size(10.0)
                    .color(crate::theme::TEXT_DIM),
            );
            for spec in effects_sliders()
                .iter()
                .filter(|s| s.id.0.starts_with("nr_"))
            {
                if let Some(edit) = scoped_slider(ui, spec, &scoped) {
                    out = Some(edit);
                }
            }
        }
```

Use whatever dim-text colour constant `theme.rs` actually exports (match the REGION TONES subheader's own construction exactly rather than assuming `TEXT_DIM` exists).

- [ ] **Step 6: Add the export combos**

In `ferrolite-app/src/export/settings_form.rs`, after the existing `Resize` combo, following its exact style:

```rust
    egui::ComboBox::from_label("Sharpen for")
        .selected_text(match o.sharpen_for {
            OutputMedium::None => "None",
            OutputMedium::Screen => "Screen",
            OutputMedium::Glossy => "Glossy paper",
            OutputMedium::Matte => "Matte paper",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut o.sharpen_for, OutputMedium::None, "None");
            ui.selectable_value(&mut o.sharpen_for, OutputMedium::Screen, "Screen");
            ui.selectable_value(&mut o.sharpen_for, OutputMedium::Glossy, "Glossy paper");
            ui.selectable_value(&mut o.sharpen_for, OutputMedium::Matte, "Matte paper");
        });

    // Greyed with a reason while no medium is chosen — the amount tier only
    // means something once output sharpening is on (same greyed-with-reason
    // convention as the Develop panel's unavailable controls).
    ui.add_enabled_ui(o.sharpen_for != OutputMedium::None, |ui| {
        egui::ComboBox::from_label("Sharpen amount")
            .selected_text(match o.sharpen_amount {
                OutputSharpenAmount::Low => "Low",
                OutputSharpenAmount::Standard => "Standard",
                OutputSharpenAmount::High => "High",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut o.sharpen_amount, OutputSharpenAmount::Low, "Low");
                ui.selectable_value(
                    &mut o.sharpen_amount,
                    OutputSharpenAmount::Standard,
                    "Standard",
                );
                ui.selectable_value(&mut o.sharpen_amount, OutputSharpenAmount::High, "High");
            });
    })
    .response
    .on_hover_text(if o.sharpen_for == OutputMedium::None {
        "Choose an output medium to enable output sharpening"
    } else {
        ""
    });
```

Import the two enums from `ferrolite_export`.

- [ ] **Step 7: Run tests**

Run: `cargo test -p ferrolite-app`
Expected: PASS — 4 new tests plus every existing UI/disclosure test (including `every_action_is_in_a_settings_group` and the `disclosure_snapshot` count assert, neither of which should need changing).

- [ ] **Step 8: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
git add ferrolite-app/src/develop/adjustments.rs ferrolite-app/src/develop/base_tabs.rs \
        ferrolite-app/src/export/settings_form.rs
git commit -m "feat(app): ungrey NR globally, add sharpen Detail/Masking, export sharpening combos"
```

---

## Task 8: Parity fixtures, tile-seam golden, benchmarks, docs

Read spec §7.3–§7.4.

**Files:**
- Modify: `ferrolite-pipeline/tests/golden.rs` (or the parity suite file the repo uses — find it first)
- Modify: `docs/benchmarks/2026-07-28-phase3-fused-engine.md`
- Modify: `docs/design/V2/README.md`

- [ ] **Step 1: Find the parity suite and its fixture registration**

Run: `grep -rn "PARITY_TOL\|fn fixtures\|full_global" ferrolite-pipeline/tests/ | head -30`

Register the new fixtures the same way existing ones are registered. Do not invent a new mechanism.

- [ ] **Step 2: Add the tile-seam golden (the load-bearing one)**

This test MUST fail if the 62 px halo fold-in is removed. A smooth-gradient fixture passes even with broken seam handling, which is why the fixture is deliberately high-frequency:

```rust
/// P4 design §7.3: tiled-vs-whole parity for NR. The fixture is deliberately
/// HIGH-FREQUENCY at the tile seam — a smooth gradient passes even when the
/// halo fold-in is broken, which would make this a fake test. Verify by
/// temporarily forcing `nr_halo` to 0: THIS TEST MUST GO RED.
#[test]
fn nr_tiled_matches_whole_image_at_the_seam() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let img = common::high_frequency_checker(256, 256);
    let mut doc = OpStack::default();
    doc.global.noise_reduction =
        ferrolite_pipeline::NoiseReduction { luminance: 0.8, color: 0.5, ..Default::default() };

    let mut whole_pipe = EditPipeline::new(ctx.clone(), &img, doc.clone(), IDENTITY);
    let whole = blit_to_rgba8(&ctx, &whole_pipe.evaluate());
    let tiled = common::render_tiled_to_rgba8(ctx, &img, doc, IDENTITY);

    let max_diff = common::max_abs_diff(&whole, &tiled);
    assert!(max_diff <= 4, "tiled NR disagrees with whole-image by {max_diff}");
}
```

Add `common::high_frequency_checker` (a 2 px-period checker plus fine noise) and reuse whatever tiled-render helper the existing tile parity tests already use — `grep -rn "tile" ferrolite-pipeline/tests/` to find it. If no tiled helper exists, model the new one on `ferrolite-export/src/render.rs`'s `render_tiled`.

- [ ] **Step 3: Verify the seam test is real**

Temporarily change `nr_halo` to return `0`, run the test, and confirm it FAILS. Then revert. Record in the commit message that this was verified.

Run: `cargo test -p ferrolite-pipeline nr_tiled_matches_whole_image_at_the_seam`

- [ ] **Step 4: Add `nr_luma`, `nr_chroma`, `sharpen_detail_masking` fixtures**

Register three parity fixtures with the suite's existing mechanism:
- `nr_luma`: `noise_reduction = { luminance: 0.8, detail: 0.2, ..default }`
- `nr_chroma`: `noise_reduction = { color: 0.8, color_detail: 0.2, ..default }` — guards the chroma-edge desaturation risk (spec §9)
- `sharpen_detail_masking`: `Sharpen { amount: 0.9, radius: 4, detail: 0.6, masking: 0.5 }`

Generate their goldens with the documented regeneration mechanism (these are NEW fixtures, so first-generation is legitimate — unlike the existing goldens, which must not move).

- [ ] **Step 5: Run the benchmarks**

Run the `engine_bench` harness per the method in `docs/benchmarks/2026-07-28-phase3-fused-engine.md` §"Method" (5 runs, cool machine if possible), adding two cases: NR-dirty evaluate, and NR + sharpen combined.

Record the results in a new "P4 increments" section of that doc. Gate: **no regression on the existing cases**. NR's own cost is a new baseline — report it honestly, including if it is worse than hoped; §3.3's two-coarsest-level cache is the escape hatch, and this measurement is what justifies reaching for it or not.

- [ ] **Step 5b: Measure peak GPU memory (gates the spec §3.3 fallback)**

Record peak GPU bytes with NR active through the **whole-image** `EditPipeline` on the largest RAW
available, using `EditPipeline::nr_live_bytes()` plus the existing `live_gpu_pyramid_bytes()` gauge,
and confirm identity NR reports **0**.

Write both figures into the benchmark doc's "P4 increments" section alongside the timings.

**Decision rule (pre-agreed — do NOT open a new decision):** if the active figure puts total GPU
usage at or near an OOM on a 6–8 GB budget, take spec §3.3's tile-path-only fallback — NR runs only
in `TileEditPipeline`, and the whole-image path skips it. Record which branch was taken and the
numbers that justified it. Expected order of magnitude: ~768 MB for a 24 MP frame (4 × 192 MB).

- [ ] **Step 6: Update `docs/design/V2/README.md`**

Three edits (spec §6.5):
1. Effects tab SHARPENING: `Amount/Radius/Detail` → `Amount/Radius/Detail/Masking`, noting Masking protects flat areas.
2. NOISE REDUCTION: remove any "future"/greyed framing, note it is wired globally and greyed per-mask, and add the "judge at 1:1" hint.
3. Export right panel: add the two combos to the settings row list.

- [ ] **Step 7: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-pipeline -- --check
cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings
cargo test -p ferrolite-pipeline
git add ferrolite-pipeline/tests/ docs/benchmarks/2026-07-28-phase3-fused-engine.md \
        docs/design/V2/README.md
git commit -m "test(pipeline): P4 parity fixtures + tile-seam golden (verified red without the halo); benchmarks + docs"
```

---

## Coordinator wrap-up

1. **Repo gate on the latest stable** — `rustup update stable` FIRST (CLAUDE.md toolchain rule), then:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo build --all-targets
   cargo test --workspace
   ```
2. **Confirm the three identity gates held** across the whole branch: no pre-existing parity golden was regenerated (`git log --stat` should show new golden files only, never modified ones), and no existing export test changed.
3. **Hand the author a numbered visual test plan** (CLAUDE.md, load-bearing). It must cover:
   - NR on a high-ISO RAW at 1:1: all four sliders live, drag responsiveness, and specifically **whether it goes plastic/blotchy at high Luminance** (the §9 risk `cargo test` cannot see).
   - NR at fit-view vs 1:1: confirm the hint line reads clearly and the coarse-LOD behavior is not alarming.
   - Chroma NR on a saturated subject: check hard color edges do not desaturate.
   - Sharpen Detail and Masking on a noisy shot: Masking should visibly stop the sky from being sharpened.
   - Per-mask sharpen Detail/Masking on two masks with different radii at 1:1.
   - NR greyed in Mask scope — hover it and read the reason.
   - Export with each medium at Standard; compare a Screen vs Matte export at 100%.
   - Regression smoke: an existing edited image renders unchanged; undo/redo; export of a masked+sharpened edit.
4. **Hold for the author's verdict** before finishing the branch. Do not present finish options as the final step.
5. After the author's approval, use `superpowers:finishing-a-development-branch`, and clean `.superpowers/sdd/*` if the branch is merged or discarded (CLAUDE.md).
