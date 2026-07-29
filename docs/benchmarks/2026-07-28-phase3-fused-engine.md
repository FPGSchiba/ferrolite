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

## Baseline (post-fusion, commit 970ca4c)

- **Machine:** Windows 11 Pro (dev laptop) — same machine as the pre-fusion baseline
- **GPU adapter:** Intel(R) Iris(R) Xe Graphics
- **Profile:** release (`cargo test --release -p ferrolite-pipeline --test engine_bench -- --ignored --nocapture`)
- **Source:** 6000×4000 synthetic gradient
- **N:** 20 iterations/case, **5 independent process runs** to gauge run-to-run noise (the
  brief asked for 2-3; a clear bimodal split across the first 3 runs made 2 more necessary
  before trusting a median)

### Per-run medians (ms)

| Run | (a) exposure-dirty | (b) grade-dirty | (c) exposure-dirty + two_masks |
|---|---|---|---|
| 1 | 70.954 | 43.857 | 96.391 |
| 2 | 55.935 | 32.792 | 93.985 |
| 3 | 76.079 | 45.338 | 87.722 |
| 4 | 55.764 | 32.430 | 71.596 |
| 5 | 54.264 | 32.007 | 93.890 |

Runs clustered bimodally: runs 1 and 3 ran ~20-40% slower across all three cases than
runs 2, 4, 5 (visible in both (a) and (b) simultaneously, so it is not case-specific
noise — most likely first-dispatch-after-build warmup for run 1, and an unexplained
transient — possibly thermal or background contention on the dev laptop — for run 3).
Reported below as **median of the 5 per-run medians**, with the full min-max range as
the stated noise band.

| Case | Median (ms) | Range (ms) |
|------|-------------|------------|
| (a) exposure-dirty evaluate | 55.935 | 54.264 – 76.079 |
| (b) grade-dirty evaluate | 32.792 | 32.007 – 45.338 |
| (c) exposure-dirty evaluate + two_masks' layers | 93.890 | 71.596 – 96.391 |

Raw output (run 5, representative of the fast cluster):

```
GPU adapter: Intel(R) Iris(R) Xe Graphics
=== engine_bench (pre-fusion) medians over 20 iterations, 6000x4000 ===
(a) exposure-dirty evaluate:              54.264 ms
(b) grade-dirty evaluate:                 32.007 ms
(c) exposure-dirty + two_masks' layers:    93.890 ms
```

(The harness's own printed label still reads "pre-fusion" — that string is a stale
literal in `engine_bench.rs` from Task 1, unrelated to which engine is actually running;
the binary was rebuilt fresh from commit `970ca4c`, i.e. after Task 4, which is the fused
engine.)

### Verdict table

| Case | Baseline (ms) | After (ms) | Ratio (baseline/after) | ≤ baseline? |
|---|---|---|---|---|
| (a) exposure-dirty (early-op) | 73.910 | 55.935 | 1.32× | yes |
| (b) grade-dirty | 36.489 | 32.792 | 1.11× | yes |
| (c) exposure-dirty + two_masks | 109.309 | 93.890 | 1.16× | yes |

### Gate result: FAILED

Both gate conditions were checked:

1. **Every case ≤ baseline, within the noise band.** PASSES for all three cases — even
   the after-medians alone beat baseline; the noise band doesn't change this call.
2. **Case (a) (early-op) ratio ≥ 2×.** FAILS. The measured ratio is 1.32× on the median,
   and even the single fastest observed sample across all 5 runs (54.264 ms) only reaches
   73.910 / 54.264 = 1.36× — nowhere near the 2× floor. This is not a noise artifact: the
   whole 5-run range for case (a) (54.264-76.079 ms) sits well above the 36.955 ms a 2×
   speedup over the 73.910 ms baseline would require.

**Disposition:** gate FAILED on the primary (early-op) criterion. Per the task
instructions this is reported as BLOCKED with the numbers above, not rationalized or
massaged. The fusion delivered a real, consistent ~1.1-1.3× improvement on all three
cases (fewer point-op passes is a genuine win), but did not reach the 2× bar this task
gates on. A plausible (unverified) explanation: cases (a)/(c) still pay for the
dehaze/sharpen neighborhood passes on every dirty re-evaluate regardless of the light/color
fusion, and those neighborhood passes likely dominate total wall time — diluting the
visible savings from collapsing six point-op dispatches into two. This is a hypothesis for
follow-up profiling, not a justification to pass the gate.

---

## Task 5b — Profiled breakdown of the exposure-dirty evaluate (2026-07-29)

The Task-5 disposition above left "neighborhood passes dominate" as an unverified
hypothesis. Task 5b profiled the fused engine's exposure-dirty `full_global`
evaluate per node and confirms it — with one correction: it is not only the
neighborhood passes, but the per-pass memory-bandwidth floor of EVERY full-res
pass on this GPU.

### Method

Every pipeline node encodes its own command buffer and `queue.submit`s at the end
of its `evaluate`, so per-node **submit + `poll(Maintain::Wait)` serialization**
is exact at node granularity (the headless test device requests
`Features::empty()`, so `TIMESTAMP_QUERY` was not available without changing the
device request). A temporary probe hook on the `Graph` executor timed each node's
CPU encode and then polled the device to attribute GPU time (instrumentation
removed after profiling, not committed). Known distortion: serialization removes
cross-node overlap and adds one sync round-trip per node — measured in-process as
~8% (unserialized wall 67.7 ms vs serialized 73.3 ms in the same run). The
profile run landed in this machine's slow cluster (67.7 ms vs the recorded
55.9 ms median); the per-node **shares** are the transferable result.

### Per-node breakdown — case (a) exposure-dirty `full_global`, medians of 20

| node | CPU encode (ms) | GPU (ms) | total (ms) | share |
|---|---|---|---|---|
| light-engine | 0.60 | 5.86 | 6.46 | 8.8% |
| dehaze-transmission | 1.79 | 15.92 | 17.70 | 24.0% |
| dehaze-recovery | 0.41 | 5.64 | 6.05 | 8.2% |
| color-engine | 0.61 | 12.87 | 13.48 | 18.3% |
| sharpen | 0.43 | 24.47 | 24.90 | 33.8% |
| geometry | 0.47 | 4.61 | 5.08 | 6.9% |
| **sum** | **4.3 (6%)** | **69.4 (94%)** | **73.7** | |

(source/color-matrix/vignette: clean, never re-evaluated on an exposure drag.
Case (c) is identical except color-engine grows to 3 dispatches: 36.5 ms.)

Key facts the shares establish:

- **A full-res pass has a hard bandwidth floor.** 24 MP × rgba16float read+write
  ≈ 384 MB/pass; light/recovery/geometry at ~5-6 ms ≈ 65-70 GB/s — Iris Xe's
  practical shared-DDR limit. The two fused engine dispatches are already AT
  this floor; no further point-op win exists without removing passes.
- **sharpen (radius 2) is the single largest item (34%)**: 25 `textureLoad`s
  per pixel, sampler-bound, ~4× a plain pass.
- **dehaze-transmission (24%)** legitimately re-runs on every upstream-dirty
  evaluate (its input image changed — caching it would alter output). 29
  dispatches at the capped 1500×1000 working res, dominated by 12 guided-filter
  box passes (21 taps each at working guided radius 10).
- **color-engine ≈ 2.2× light-engine** at identical traffic: the color segment's
  extra ALU (curve LUT + HSL round-trips + grade) makes it partially ALU-bound.
- **CPU encode is ~6% of the evaluate** — no CPU-side optimization (persistent
  buffers, bind caching, submit batching) can move the headline number by more
  than a few percent.

### What was tried

1. **Shared-memory tiled sharpen — REJECTED (3× regression).** A bit-identical
   workgroup-tile version of `sharpen.wgsl` (same texels, same summation order;
   legacy path for radius > 12) measured **73.0 ms vs 24.5 ms** for the naive
   loop on Iris Xe — the naive loop's overlapping taps were already served by
   the texture cache, and the dynamically-sized shared tile destroyed that.
   Reverted; the shader is byte-identical to before.
2. **Pooled persistent uniform/LUT buffers in `LocalAdjustmentsNode::apply` —
   KEPT.** Replaces two `create_buffer_init` allocations (uniform + 3 KiB LUT)
   per engine dispatch per evaluate with pooled buffers written via
   `queue.write_buffer` (slot-per-dispatch, cursor reset each evaluate, so a
   later layer's write can never clobber an earlier one). Output-identical.
3. **Cached views + bind groups in `DehazeTransmissionNode` — KEPT.** The node
   rebuilt ~16 views + ~17 bind groups per evaluate — the largest single
   CPU-encode item (1.8-2.3 ms). All referenced resources are persistent, so the
   full bind set is now built once and reused until the source/out texture
   identity or working dims change (uniform CONTENTS still written every
   evaluate). Measured directly: transmission encode 1.79 → 1.53 ms (a) /
   2.34 → 1.88 ms (c). Output-identical; also what the CLAUDE.md GPU rule asks.

Both kept changes are CPU-encode-only (~0.5-1 ms combined). An interleaved
same-day A/B (old vs new binaries alternated, 3 rounds) shows old ≈ new within
this laptop's noise — the wall-median effect is real but below the noise floor,
exactly as the 6% CPU share predicts. They are kept as measured churn removal
(per-node encode timings above), not as headline-number wins. Parity suite
10/10 and the full `ferrolite-pipeline` suite stayed green after each.

### Re-run: 3-case bench after Task 5b (5 process runs, release, 2026-07-29)

| Run | (a) exposure-dirty | (b) grade-dirty | (c) exposure-dirty + two_masks |
|---|---|---|---|
| 1 | 61.099 | 56.000 | 103.674 |
| 2 | 61.088 | 34.561 | 107.516 |
| 3 | 62.257 | 38.873 | 105.783 |
| 4 | 70.325 | 63.652 | 137.212 |
| 5 | 62.122 | 41.110 | 110.593 |

| Case | Median (ms) | Range (ms) |
|---|---|---|
| (a) exposure-dirty | 62.122 | 61.088 – 70.325 |
| (b) grade-dirty | 41.110 | 34.561 – 63.652 |
| (c) exposure-dirty + two_masks | 107.516 | 103.674 – 137.212 |

**Machine-state caveat (measured, not assumed):** the laptop degraded steadily
through this session — the SAME unmodified Task-5 binary, re-run interleaved
with the new one on the same day, measured (a) at 91.8-95.9 ms vs its recorded
55.9 ms median. Today's absolute numbers are therefore 10-60% inflated across
the board for both old and new code, and the Task-5 verdict table above remains
the canonical post-fusion record; Task 5b changes wall time by less than the
day-to-day noise on this machine.

### Verdict — is ≥2× achievable on this hardware?

**No — not without semantic changes.** The arithmetic from the profile is
decisive: a 2× speedup over the 73.9 ms pre-fusion baseline requires ≤37 ms,
but the non-point-op cost alone (sharpen 24.9 + transmission 17.7 + recovery
6.1 + geometry 5.1 = 53.8 ms serialized in the profile's machine state, ~41 ms
scaled to the fast-cluster state) already exceeds that budget. Even if the two
fused engine dispatches were FREE, case (a) could not reach 37 ms. The spec's
≥2× prediction implicitly assumed the six point-op passes dominated the
evaluate; the profile shows they are ~27% of it — the fusion earned roughly
what those passes were worth (~1.3×), and the rest of the time is (i) the
memory-bandwidth floor of the remaining full-res passes and (ii) the
neighborhood ops that semantically must re-run on an upstream edit.

The realistic ceiling for exposure-drag `full_global` at 6000×4000 on Intel
Iris Xe with the current pass structure is **~50-55 ms** (fast state) — where
it already is. Paths below that exist but all change rendering output and need
author sign-off as accepted deltas (like the fusion's own f16 note):

- **Separable two-pass sharpen** (H then V box blur): ~25 taps → 2×5 taps; est.
  ~-12 ms on (a)/(b)/(c). Introduces one f16 intermediate quantization step
  (sub-1e-3 deltas expected, but not bit-identical).
- **Fuse dehaze recovery into the color-engine dispatch**: removes one full-res
  read+write round trip (~-5 ms); removes one f16 quantization (a precision
  GAIN, but shifts pixels like the fusion did).
- **Lower the transmission working-res cap** (1536 → 1024): ~-7 ms of the
  transmission's 17.7 ms; visibly coarser transmission on large radii.

All three together would put ~35 ms in reach — i.e. the 2× bar is reachable
only as a Phase-4-style follow-up with accepted-delta review, not as a safe
optimization of the current semantics.
