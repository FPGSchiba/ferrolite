# Fused-Layer-Engine Phase — Parity + Perf Baselines

**Date:** 2026-07-28
**Phase:** `.superpowers/sdd/2026-07-28-unified-engine-phase3-fused-layers/`
**Status:** Task 1 (safety net) — pre-fusion baseline recorded; later tasks must
reproduce the committed goldens and beat the medians below.

---

## Overview

Before any fused-layer-engine code changes land, Task 1 commits:

1. **Parity goldens** — 8 fixture docs (`ferrolite-pipeline/tests/common/layer_engine.rs::fixture_docs`)
   rendered through the CURRENT (pre-fusion) `EditPipeline` chain on a deterministic
   512×512 HSV-sweep synthetic source, saved as 16-bit PNGs under
   `ferrolite-pipeline/tests/golden/layer_engine/`. The parity test
   (`ferrolite-pipeline/tests/layer_engine_parity.rs`) compares future runs against
   these within `PARITY_TOL = 2e-3` (scene-linear f32, RGB channels).
2. **Perf baselines** (this document) — median wall-clock ms for three
   re-evaluate-on-a-dirty-node cases on a synthetic 6000×4000 source, via
   `ferrolite-pipeline/tests/engine_bench.rs` (`#[ignore]`d; run explicitly).

Later fusion tasks must not regress either: goldens stay within tolerance, and
the fused engine's medians must be **at or below** the pre-fusion numbers
recorded here (that is the entire point of fusing the per-op node chain — fewer
GPU passes per edit).

---

## Parity Goldens

- **Source:** deterministic 512×512 HSV sweep, generated in-code (no committed
  source asset) — see `hsv_sweep_source()`. Top half sweeps value 0→1 at full
  saturation; bottom half sweeps saturation 1→0 at full value; hue sweeps 0→360°
  across every row.
- **Tolerance:** `PARITY_TOL = 2e-3` (scene-linear f32, RGB channels; alpha not compared).
- **Golden format:** 16-bit PNG, `image` crate, signed linear-light encoding
  `[GOLDEN_MIN, GOLDEN_MAX] = [-1.0, 8.0]` mapped to `u16` (quantization step
  ≈1.37e-4, well below `PARITY_TOL`). The signed range matters: `light_trio`,
  `curve_hsl_grade`, and `wb_contrast_both` all legitimately render pixels
  slightly below 0.0 in scene-linear space (contrast/white-balance/HSL/grade can
  push a channel negative before any downstream tone-curve floor) — an
  unsigned `[0, N]` encoding would silently clip that and produce a false
  "parity regression" on every subsequent run even though the render is fully
  deterministic (caught and fixed during this task; see the task report).
- **Fixtures (8):** `identity`, `light_trio`, `curve_hsl_grade`, `full_global`,
  `one_mask`, `two_masks`, `mask_only`, `wb_contrast_both` — see
  `fixture_docs()` for exact parameters. Every non-identity fixture is
  sanity-checked to differ from the `identity` golden by more than
  `PARITY_TOL` somewhere (catches a fixture that accidentally no-ops).
- **Regenerate:** `UPDATE_GOLDENS=1 cargo test -p ferrolite-pipeline --test layer_engine_parity`
- **Compare:** `cargo test -p ferrolite-pipeline --test layer_engine_parity` (skips
  cleanly with no GPU adapter — primarily a local/author gate, not a CI gate).

---

## Perf Baselines

### Method

- **Source:** synthetic 6000×4000 gradient, generated once (generation cost
  excluded from the timed region).
- **Doc:** `full_global` fixture (light_trio + curve_hsl_grade + sharpen 0.8/r2 +
  dehaze 0.3/r8), built once, then warmed up with one untimed `evaluate()` (pays
  first-dispatch driver pipeline compilation).
- **Timing:** for each case, 20 iterations of `set_stack` + `evaluate`, each
  followed by `ctx.device.poll(Maintain::Wait)` so the timer captures actual GPU
  execution (not just command-buffer recording/submission). Median of 20 reported.
- **Cases:**
  - **(a) exposure-dirty** — `full_global` with `Exposure.ev` alternating
    `base ± 0.01` each iteration (only the exposure node + everything
    downstream re-runs).
  - **(b) grade-dirty** — `full_global` with `ColorGrade.global.lum` alternating
    `±0.01` (a later position in the chain than exposure).
  - **(c) exposure-dirty + masks** — same as (a), but with `two_masks`' two
    `LocalAdjustments` layers layered on top of `full_global` (tests whether an
    upstream-of-`LocalAdjustments` dirty still reuses the mask-composite cache,
    keyed on mask definitions only, instead of re-compositing).
- **Run:** `cargo test -p ferrolite-pipeline --test engine_bench --release -- --ignored --nocapture`
  (release profile — debug-profile numbers are not representative).

### Baseline (pre-fusion, commit b3b40f6)

- **Machine:** Windows 11 Pro (dev laptop)
- **GPU adapter:** Intel(R) Iris(R) Xe Graphics
- **Profile:** release
- **Source:** 6000×4000 synthetic gradient
- **N:** 20 iterations/case

| Case | Median (ms) |
|------|-------------|
| (a) exposure-dirty evaluate | 73.910 |
| (b) grade-dirty evaluate | 36.489 |
| (c) exposure-dirty evaluate + two_masks' layers | 109.309 |

Raw output:

```
GPU adapter: Intel(R) Iris(R) Xe Graphics
=== engine_bench (pre-fusion) medians over 20 iterations, 6000x4000 ===
(a) exposure-dirty evaluate:              73.910 ms
(b) grade-dirty evaluate:                 36.489 ms
(c) exposure-dirty + two_masks' layers:    109.309 ms
```

---

## Accepted rendering deltas vs the pre-fusion chain

**Date:** 2026-07-29. **Disposition:** author-approved. The `layer_engine_parity`
goldens were regenerated FROM the fused engine on 2026-07-29 (the old-vs-new
parity job below is what justified that regeneration, not a rubber stamp).

Task 3 wired both `EditPipeline`/`TileEditPipeline` onto the two-segment
engine (Light-stage + Color-stage `LocalAdjustmentsNode`s replacing the six
standalone exposure/white-balance/contrast/tone-curve/hsl/color-grade
passes). Comparing the fused engine's render against the pre-fusion chain
(the original goldens, before regeneration) surfaced two categories of
finding:

1. **One real bug**, fixed before acceptance: `local_adjust.wgsl`'s shared
   `adjust()` floor clamp (correct, pre-existing per-mask/mask-order
   behavior) was also — wrongly — hitting the new global pseudo-layer
   dispatches, which the pre-fusion standalone passes never clamped. Fixed by
   gating the clamp on `order_and_coverage.x` (`global_order`) in both the
   WGSL and its `light_color_apply` CPU mirror. This alone took
   `curve_hsl_grade` from failing to passing outright and materially reduced
   every other fixture's diff.
2. **Residual deltas**, accepted as inherent precision improvement (this
   section) rather than a defect, after exhaustive root-causing.

### Per-fixture max diff (old pre-fusion chain vs new fused engine, before goldens were regenerated)

| fixture | max diff | vs `PARITY_TOL` (2e-3) |
|---|---|---|
| `identity` | 0 | pass |
| `mask_only` | 0 | pass |
| `curve_hsl_grade` | 0 | pass (after the clamp-gating fix) |
| `wb_contrast_both` | 0.0020 | ~1.0× |
| `one_mask` | 0.0030 | 1.5× |
| `light_trio` | 0.0040 | 2.0× |
| `color_range_mask` | 0.0073 | 3.65× |
| `luma_range_mask` | 0.0083 | 4.2× |
| `full_global` | 0.0585 | 29× |
| `two_masks` | 0.6000 | 300× |

### Spatial extent of the disagreement (512×512 pixels)

| fixture | pixels >2e-3 | pixels >1e-2 | pixels >0.1 | max count in any 8×8 block (of 64) |
|---|---|---|---|---|
| `light_trio` | 20397 | 0 | 0 | 0 |
| `wb_contrast_both` | 332 | 0 | 0 | 0 |
| `one_mask` | 4937 | 0 | 0 | 0 |
| `luma_range_mask` | 7713 | 0 | 0 | 0 |
| `color_range_mask` | 728 | 0 | 0 | 0 |
| `full_global` | 129494 | 27604 | 0 | 64 (contiguous) |
| `two_masks` | 2873 | 183 | 183 | 5 (loosely clustered) |

Five of the seven fixtures have ZERO pixels above 1e-2 — their disagreement
is diffuse sub-1e-2 noise spread across a sizeable fraction of pixels, not a
concentrated defect. `full_global`'s >1e-2 pixels saturate whole 8×8 blocks
(64/64) — a contiguous region, not scattered noise. `two_masks`'s >1e-2
pixels (all also >0.1) cluster loosely (max 5/64 per block) — isolated
hue-critical pixels, not a solid region.

### Ablation: dehaze/sharpen are what turn `full_global`'s diffuse noise into a contiguous region

Rendering three variants of `full_global` (dehaze+sharpen both present; sharpen
kept but dehaze removed; both removed) through old vs new confirmed the
mechanism directly:

| variant | max diff (new vs old) | pixels >1e-2 | max 8×8 block count |
|---|---|---|---|
| `full_global_no_neighborhood` (no dehaze, no sharpen) | 0.0059 | 0 | 0 |
| `full_global_no_dehaze` (sharpen kept, dehaze removed) | 0.0098 | 0 | 0 |
| `full_global` (dehaze + sharpen, both present) | 0.0586 | 27604 | 64 |

With neighborhood ops removed, `full_global` collapses to the SAME diffuse,
sub-1e-2 scale as `light_trio` — confirming the wiring swap itself
(`dehaze_transmission`/`dehaze_recovery`'s graph input moved from the old
`contrast_id` to the new `light_engine_id`, a like-for-like swap; params and
dirty routing unchanged) introduces nothing beyond the already-characterized
f16-removal noise. Re-enabling dehaze (with sharpen already present and
unchanged) is what produces the 27,604-pixel, full-block-filling divergence.
This is the signature of dehaze recovery's `(I−A)/t + A` regionally
amplifying an already-tiny upstream perturbation wherever the transmission
`t` is locally small — not a hookup defect.

### `two_masks`: hue-boundary pixel trace

A git worktree at the pre-Task-3 commit (`d569572`) rendered the real old
chain to get ground truth at the worst pixel (342, 256):

- Old real Light-stage-only output: `(-0.040955, -0.062988, 1.815430)`.
- CPU-simulated old (3 separate f16-quantized exposure/wb/contrast
  dispatches) vs new (1 fused, unclamped dispatch) light-only math for this
  pixel: **bit-identical**, `(-0.04095, -0.06299, 1.8174)` — the Light engine
  itself introduces no discrepancy at this pixel.
- Old real output after mask layer 1 only (`one_mask`): `(0.000000,
  0.078552, 1.384766)`.
- Feeding that REAL intermediate through mask layer 2's CPU reference
  (`light_color_apply`, unmodified by Task 2/3) reproduces the committed
  `two_masks` golden **exactly**: `(1.0957031, 2.7e-5, 1.1992188)` vs golden
  `(1.095703, 0.000013, 1.199219)`.
- Feeding a CPU-simulated after-layer-1 value differing from the real one by
  only ~6e-5–2e-3 (a couple of f16 ULPs) through the SAME unmodified
  layer-2 reference gives `(1.0010, 0.6006, 0.6006)` — matching the new
  engine's actual GPU output, and wildly different from the golden.

**Conclusion:** a ~2e-3 difference in the value feeding an UNMODIFIED
per-mask HSL/hue-rotation/saturation step flips its output completely. This
is chaotic sensitivity inherent to HSL/hue math near specific critical points
(this pixel's R and G both floor-clamp to the same achromatic corner before
the per-mask HSL step, and its ~240° hue sits close enough to a `hue2rgb`
piecewise boundary that a few-ULP perturbation changes which branch — and by
how much — a downstream hue rotation lands in), not a defect introduced by
this task.

### Why this was accepted

Fusing the six standalone point-op passes into two engine-stage dispatches
removes 1-2 intermediate `rgba16float` quantization round-trips per pixel —
that is a **higher-precision** render, not a lower-precision one; the
pre-fusion chain's extra quantization steps were incidental cost, never a
correctness requirement. The resulting few-ULP shift is unavoidable in any
engine that achieves the fusion's stated goal (fewer GPU passes, less
incidental quantization) and, per the ablation and pixel-trace evidence
above, provably does not originate from a wiring or logic defect in this
task's changes — it is amplified, sometimes dramatically, by pre-existing,
unmodified HSL/hue and divide-by-transmission math whenever a pixel happens
to sit near one of those functions' inherent numerical critical points. No
reasonable `PARITY_TOL` covers the `two_masks` outlier (0.6) without gutting
the check's value, so continuing to gate on old-chain reproduction would
have meant either permanently disabling this suite or blocking the fusion
indefinitely for a precision GAIN. Author-approved 2026-07-29: goldens
regenerated from the fused engine; `PARITY_TOL` stays at `2e-3` unchanged;
the suite's job going forward is pinning the fused engine against future
regressions, not re-litigating this comparison.

---

## Baseline (post-fusion) — Task 5 appends here

_(filled in by Task 5 once the fused engine lands, using the identical method
above so the two rows are directly comparable)_
