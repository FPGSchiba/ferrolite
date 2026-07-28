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

## Baseline (post-fusion) — Task 5 appends here

_(filled in by Task 5 once the fused engine lands, using the identical method
above so the two rows are directly comparable)_
