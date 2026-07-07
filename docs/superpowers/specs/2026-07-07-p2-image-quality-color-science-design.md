# ferrolite — P2: Image-quality & color-science foundation (design)

> **Status:** Design — pending user review (2026-07-07); then a writing-plans cycle **per plan**.
> **Date:** 2026-07-07
> **Phase:** **P2** of the v2 architecture map (`2026-07-05-ferrolite-v2-architecture-map.md`,
> §4 P2). The concrete delivery of **D1** — "image quality is now a PRIMARY goal" (map §2.2).
> **Parent lineage:** v2 architecture map → **this spec**. Builds on Spec 3
> (`2026-07-01-spec3-color-and-export-design.md` — `ferrolite-color`, `ColorMatrixNode`, the
> swappable display tail, the `ColorProfile` decode product) and Spec 2
> (`2026-06-30-spec2-editing-design.md` — the edit DAG, OpStack sidecar, **VT halo + GPU tile
> producer**, two-tier preview/full-res recompute). Spec 4.3 (monitor tail) is the display end.
> **Crates:** `ferrolite-decode`, `ferrolite-color`, `ferrolite-pipeline` — all **photo tier**.
> **Branching:** **one branch per implementation plan** off `main` (§7), merged in dependency
> order — not one branch for the whole spec — to honor the per-phase/per-plan handoff discipline.

---

## 1. Goal & validation

Deliver the D1 quality promotion **at the pipeline head**, without regressing the G1/G2 browse/load
speed goals (map §2.4). Three deliverables:

> open a RAW → its camera colors are transformed camera→working with a **dual-illuminant matrix
> that follows the white-balance temperature** (Lightroom's model) → at full-res / 1:1 / export the
> image is demosaiced with a **competitive RCD** pass (a WGSL compute shader; QuadBin still serves
> preview/interactive) → highlights and wide/out-of-gamut colors are **carried unclamped** through
> the linear working pipeline and clipped **only at the display + output tail** → the result is
> visibly better color and detail with no new user controls and no speed regression on browse/open.

**Quality is now primary (D1).** This overturns Spec 3's "image quality remains secondary" stance
for the color/demosaic path specifically. Speed stays non-negotiable: the heavy paths (RCD full-res,
CFA upload) run as `Job`s at the appropriate tier and never block the UI thread (CLAUDE.md §1);
pipelines are still built once and reused (CLAUDE.md §2).

---

## 2. Scope

**In:**
- `ferrolite-decode` — surface the camera's **dual-illuminant** calibration (both DNG-style
  matrices + their reference white points) as an **additive** decode product (contract §3); a new
  **RCD** demosaic (CPU reference impl behind the existing `DemosaicToRgb16f` trait); **stop
  clamping** demosaic output to [0,1].
- `ferrolite-color` — **dual-illuminant interpolation** (`camera_to_working_interpolated`,
  inverse-CCT / mired weighting) + **CCT↔xy** helpers. Single-calibration input reduces to today's
  `camera_to_working`.
- `ferrolite-pipeline` — the `ColorMatrixNode` uniform **recomputes from the WhiteBalance
  temperature** (live re-interpolation); a **RCD WGSL compute pass** as a photo-tier node with the
  raw **CFA uploaded as a single-channel GPU source**, halo-consuming for tiled full-res/1:1/export;
  **automatic two-tier** engagement (QuadBin preview / RCD full-res). Audit downstream ops for
  hidden [0,1] assumptions so unclamped values survive to the tail.

**Not in P2 (explicitly deferred, do not design for):**
- **Gamut mapping / compression, out-of-gamut warnings, soft-proofing → P8.** P2 only *preserves*
  gamut (removes premature clamping); it performs **no** gamut correction. (This is the
  "preserve values, clip at tail" decision — the "+gamut compression" alternative was declined.)
- **X-Trans / non-RGGB demosaic (Markesteijn).** RCD is Bayer-only; non-RGGB sensors fall back to
  the existing path. A later phase if needed.
- **A user-facing quality/speed toggle or per-image demosaic selector.** Tiering is automatic; no
  new persisted state (contract §2 stays trivially satisfied).
- **Default working-space change.** Rec.2020 linear stays the default (Spec 3); revisited only if
  P8 gamut work demands it.
- **AI denoise / joint demosaic-denoise → A1.** RCD is the classical baseline A1 later augments.

---

## 3. Settled decisions (this brainstorm, 2026-07-07)

| # | Question | Decision | Rationale |
|---|---|---|---|
| **S1** | Spec/branch shape | **One spec, 5 implementation plans, one branch per plan** off `main`, merged in dependency order. | Honors the per-phase/per-plan handoff discipline; each branch gets its own green gate + author visual test before the next dependent plan starts. |
| **S2** | Demosaic algorithm + compute target | **RCD** — **WGSL compute pass** (GPU); **CPU RCD** as headless/no-GPU fallback + golden reference; **QuadBin retained** as fast/preview path; **X-Trans/non-RGGB out** (fallback). | RCD is darktable's modern default: strong high-frequency detail, faster than AMaZE, and "plays better with capture sharpening" (→ P4). GPU is proven (darktable OpenCL RCD ~20 MP/0.46 s) and matches the WGSL-learning charter. RCD code (RawTherapee/darktable, GPL-3) ports cleanly into photo-tier `ferrolite-decode`/`-pipeline` (GPL-3 binary). |
| **S3** | Dual-illuminant driver | **Live — follows the WB edit.** Re-interpolate camera→working from the current WB temperature. Single-matrix cameras fall back to single-illuminant. | Most correct (Lightroom's model): changing WB temp re-derives the correct camera matrix. Accepted cost: `ColorMatrixNode` becomes dependent on the `WhiteBalance` op. |
| **S4** | Gamut ambition | **Preserve unclamped values, clip only at the tail.** No gamut mapping in P2. Rec.2020 default. | A competitive demosaic + dual-illuminant color are wasted if the next step clamps to [0,1]. The unclamp is a *prerequisite plumbing* change, not a correction feature. Correction/warnings = P8. |
| **S5** | Quality/speed control | **Automatic two-tier** (QuadBin preview / RCD full-res + export). No toggle, no persisted state. | Mirrors the existing preview-vs-full-res recompute; RCD is simply "the full-res quality." Least UI, YAGNI. |

---

## 4. Architecture of the slice

```
ferrolite-app  (no new controls in P2; existing WB temp/tint now drive matrix interpolation)
   │
   ├── ferrolite-decode
   │      ColorProfile → DUAL-illuminant carrier: Vec<(white_xy, xyz_to_cam)> + is_fallback
   │          (both DNG matrices + white points; + ForwardMatrix if rawler exposes it)  ── contract §3 additive
   │      Demosaic: QuadBin (preview, unchanged) + RCD (CPU reference, new) behind DemosaicToRgb16f
   │          NO clamp to [0,1] — carry highlights >1 and wide/negative channels
   │
   ├── ferrolite-color  (pure, CPU, no GPU/UI)
   │      camera_to_working_interpolated(calibrations, target_cct, working)
   │          = inverse-CCT (mired) weighted blend of the two matrices → Bradford → xyz_to_working
   │      CCT↔xy helpers (Robertson/McCamy);  single-calibration → today's camera_to_working
   │
   ├── ferrolite-pipeline
   │      ColorMatrixNode uniform RECOMPUTED on WhiteBalance change (mark_dirty + uniform push;
   │          pipeline still built once — CLAUDE.md §2).  WB temp = scene-CCT estimate.
   │      RCD WGSL compute pass (photo-tier node) — raw CFA uploaded as a single-channel GPU
   │          source; halo-consuming neighbourhood op → rides the VT halo (contract §5) for
   │          tiled full-res / 1:1 / export.  Generic executor untouched (contract §4).
   │      Two-tier: QuadBin below full-res, RCD at 1:1 + export (automatic — Spec 2 recompute).
   │      Downstream ops (contrast pivot, tone curve, histogram) audited for [0,1] assumptions.
   │
   └── ferrolite-vt / display tail (Spec 3/4.3) ── UNCHANGED; the sole clip/convert point:
          working→display 3×3 + sRGB OETF (display), working→output 3×3 + OETF (encode).
```

**Licensing tiers preserved (map §3).** All P2 work is **photo-tier** (`ferrolite-decode`,
`ferrolite-color`, `ferrolite-pipeline`). No engine-tier crate changes; the CFA-as-GPU-source and
RCD compute node are photo ops supplied to the untouched generic executor (contract §4), and the
VT streams the CFA as a generic large source (contract §5) with no photo concepts leaking down.

---

## 5. The three deliverables in detail

### 5.1 Dual-illuminant camera color (live, WB-driven — S3)

**Decode (`ferrolite-decode/src/color.rs`).** `ColorProfile` today picks D65-or-first from
rawler's `HashMap<Illuminant, FlatColorMatrix>` and stores one `xyz_to_cam` + `white_xy`. It becomes
a dual-illuminant carrier surfacing **both** calibration points (typically Standard-A ≈ 2856 K and
D65) with their white points, plus `is_fallback`. Purely additive (contract §3): existing consumers
that want a single matrix take the nearest/first; the fallback (`srgb_fallback`) and single-matrix
cameras collapse to one entry. If rawler exposes a DNG **ForwardMatrix**, surface it (better than
inverting `ColorMatrix`) — resolved at plan-write time (§8).

**Color math (`ferrolite-color`).** New `camera_to_working_interpolated(calibrations, target_cct,
working)`: weight the two matrices by **inverse CCT (mired)** — DNG's convention — interpolate,
then Bradford-adapt to the working white and compose `xyz_to_working`. New **CCT↔xy** helpers map a
white-balance temperature to the interpolation weight (and back). One calibration → delegates to the
existing `camera_to_working` unchanged; zero calibrations → `srgb_fallback` path unchanged.

**Pipeline (`ferrolite-pipeline`).** The `ColorMatrixNode` uniform is **recomputed from the current
WhiteBalance temperature**: on WB change, re-interpolate → push the 3×3 uniform → `mark_dirty(
ColorMatrixNode)` so the chain re-runs. The pipeline is **still built once** (CLAUDE.md §2) — only
the uniform changes. This is the S3 coupling: the head matrix is no longer a static decode product;
the WB temp is now the scene-illuminant estimate that drives it (Lightroom's model), reconciled with
the existing `WhiteBalance` op (§8).

### 5.2 Competitive demosaic — RCD (S2)

- **Preview/interactive tier:** QuadBin (unchanged; CPU; half-res; zero demosaic artifacts).
- **Full-res quality tier:** **RCD WGSL compute pass** in `ferrolite-pipeline`. Raw CFA is uploaded
  as a **single-channel GPU source** (format + pattern-offset uniform — §8); RCD is a
  **halo-consuming neighbourhood op** and rides the source-agnostic **VT halo** (contract §5) so it
  tiles correctly for full-res, 1:1, and export (reusing Spec 2's tile producer + halo proof).
- **CPU fallback + reference:** CPU RCD (rayon; `wide` SIMD optional/deferrable) behind the existing
  `DemosaicToRgb16f` trait — used when `GpuContext::headless()` (CI) or no GPU, and serving as the
  **golden reference** the WGSL pass is validated against. Keeps `cargo test --workspace` green
  headless.
- **Non-RGGB/X-Trans:** fall back to the existing path (out of scope — §2).
- **Tiering is automatic (S5):** QuadBin below full-res, RCD at 1:1 + export, via Spec 2's
  preview-vs-full-res recompute. No new control, no persisted state.

### 5.3 Gamut-preserving working path (S4 — plumbing, NOT correction)

- **Remove the premature clamp.** QuadBin currently does `.clamp(0.0, 1.0)` per channel
  (`ferrolite-decode/src/demosaic.rs`); remove it, and never clamp in RCD. The RGBA16F working
  buffer already holds values >1 and negatives.
- **Carry unclamped through the working pipeline.** Audit downstream ops (contrast pivot / mid-grey,
  tone curve, HSL, histogram binning) for hidden [0,1] assumptions and fix any that would crush
  highlights or wide-gamut channels.
- **Clip/convert only at the tail.** The Spec 3 `working→display` (+sRGB OETF, on the GPU) and
  `working→output` (+OETF, at encode) transforms are the **sole** places values are gamut-clipped.
  Unchanged by P2 — P2 just stops destroying values before they reach it.
- **No gamut correction here.** Mapping/compression, OOG warnings, soft-proof are **P8**.

---

## 6. Error handling

- **No/short/singular camera matrix** → `srgb_fallback` (single-illuminant), logged; pipeline always
  has a defined transform. Never panics (existing behavior preserved).
- **One calibration only** → single-illuminant path (no interpolation); never panics.
- **RCD GPU pass failure / device loss** → existing wgpu error-scope recovery recreates
  `GpuContext`/pipelines (incl. the RCD pass); on unrecoverable GPU absence, fall back to CPU RCD or
  QuadBin, logged — never a crash, never a blank image.
- **CFA upload OOM** → the tiled producer bounds VRAM (Spec 2); on pressure, shrink the working set /
  fall back to QuadBin for that view, logged.
- **Non-RGGB/X-Trans sensor** → existing path, logged; no panic.
- **Unclamped values** must not produce NaN/Inf downstream — clamp only NaN/Inf to a defined value at
  the tail; finite out-of-range values pass through.

---

## 7. Decomposition into implementation plans

Dependency order; **each plan is its own branch off `main`** (S1), its own writing-plans → TDD
cycle, and its own green gate + author visual test before the next dependent plan begins.

1. **Dual-illuminant decode + color math** *(pure CPU; no UI/GPU)*
   `ColorProfile` dual-illuminant carrier in `ferrolite-decode`; `camera_to_working_interpolated`
   + CCT↔xy helpers in `ferrolite-color`; single-calibration reduces to the old path. Full CPU
   tests. **Depends on:** nothing new. **Visual test:** none (engine-internal, not yet wired — the
   real color test lands with plan 2).
2. **Live WB-driven matrix wiring** *(GPU)*
   `ColorMatrixNode` recompute-on-WB-change; reconcile WB temp as scene-CCT; interpolated-matrix GPU
   golden. **Depends on:** plan 1. **Visual test:** drag WB temp on a dual-illuminant RAW → color
   tracks correctly.
3. **Gamut-preserving unclamp** *(small, cross-cutting)*
   Remove demosaic clamp; audit downstream [0,1] assumptions; confirm the tail is the sole clip
   point. **Depends on:** plan 1/2 (so color is correct when highlights return). **Visual test:**
   recover a blown highlight → detail returns, no hue shift before the tail.
4. **RCD demosaic — CPU (reference)**
   CPU RCD behind `DemosaicToRgb16f`; non-RGGB fallback; correctness + no-clamp tests. This is the
   golden reference for plan 5. **Depends on:** plan 3 (unclamped). **Visual test:** force CPU RCD →
   full-res detail vs QuadBin.
5. **RCD demosaic — WGSL GPU + two-tier wiring**
   CFA-as-GPU-source; RCD compute pass; halo/tiling via VT halo; automatic two-tier engagement; GPU
   golden vs the plan-4 CPU reference. **Depends on:** plan 4. **Visual test:** zoom to 1:1 →
   RCD detail; export → RCD applied; no freeze on open/zoom.

---

## 8. Open implementation questions (resolve at plan-write time, not now)

- Does rawler expose a DNG **ForwardMatrix**? (Use it vs inverting `ColorMatrix` for cam→XYZ.)
- Exact **CCT↔xy** method (Robertson table vs McCamy approximation) and the precise interpolation
  domain (matrix elements vs their inverses), matching DNG behavior closely enough.
- **WB-op reconciliation:** today's `WhiteBalance` is a working-space multiplier tweak; it needs an
  **as-shot baseline CCT** so the temp/tint sliders drive matrix interpolation while still producing
  a neutral white balance. How much of the WB op changes vs stays.
- **RCD halo/support radius** → the VT halo size it requests; the CFA GPU texture **format** (R16 vs
  R32) and the **pattern-offset uniform** for arbitrary RGGB phase.
- Whether CPU-RCD **SIMD** (`wide`) lands in plan 4 or a perf follow-up.

---

## 9. Cross-cutting interface contracts (map §5) — honored

1. **Job submission** — CFA upload + full-res RCD tiles + demosaic run as `Job`s at the right tier;
   never block the UI thread.
2. **Catalog is a cache** — **no new persisted state** (S5); trivially satisfied.
3. **Decode yields separable products** — dual-illuminant `ColorProfile` is **additive**; existing
   consumers keep working.
4. **GPU executor is photo-agnostic** — RCD WGSL + `ColorMatrixNode` interpolation are
   `ferrolite-pipeline` **nodes**; the generic `Graph<PipelineImage>` executor is **not modified**.
5. **VT is source-agnostic** — the raw CFA streams as a **generic large source**; RCD's halo uses
   the existing VT halo. No photo concepts in the engine tier.
6. **AI inference seam** — N/A for P2 (RCD is the classical baseline A1 later augments).

---

## 10. Testing (TDD; CLAUDE.md gate, then hold for the author's visual test)

**Pure CPU logic (every OS in CI — the 80%+ target):**
- Dual-illuminant interpolation: weight at A → matrix A, at D65 → matrix D65, sane midpoint;
  CCT↔xy round-trips; single-calibration == old `camera_to_working`; no-matrix → `srgb_fallback`.
- `ColorProfile`: surfaces both calibrations; single/none fallback selection.
- CPU RCD: correctness on synthetic CFA (known edges/gradient) within tolerance; **preserves values
  >1** (highlight fixture); non-RGGB fallback taken; serial-vs-parallel bit-identity (QuadBin
  precedent).
- Unclamp: demosaic output retains channels >1 and out-of-gamut values.

**Golden-image GPU diffs (auto-skip when `GpuContext::headless()` is `None`, per Spec 1):**
- Interpolated `ColorMatrixNode` vs a reference at a fixed CCT.
- RCD WGSL vs the CPU-RCD reference within tolerance (tile-seam/halo correctness, reusing Spec 2's
  halo proof).
- **Regression golden:** sRGB working + single-illuminant + QuadBin ≡ today's output.
- Goldens authored/verified locally on the dev GPU (RTX 3060/3070 class).

**Visual (author's hands-on test — real surface this phase):** per-plan visual tests listed in §7 —
1:1 RCD detail, live WB-temp color tracking, highlight recovery, export applies RCD, no freeze on
open/zoom/navigation.

**Gate (per branch):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings`
+ `cargo test --workspace` green → **then STOP and hold for the author's (Jann's) visual test** of
the running app before finishing that branch (CLAUDE.md "Finishing a branch" rule).
