# P4 — Noise Reduction & Sharpening (Classical) — Design

> **Status:** Design — approved in brainstorming (2026-07-31); pending the author's final review of
> this spec, then writing-plans.
> **Date:** 2026-07-31
> **Branch:** `feat/p4-noise-reduction-and-sharpening` (off `main`) — one branch per v2 phase.
> **Parent:** v2 architecture map (`2026-07-05-ferrolite-v2-architecture-map.md`) §4 **P4**.
> **Builds on:** Spec 2's `Sharpen` unsharp-mask op + VT halo plumbing; P3's dehaze
> (`2026-07-08-p3-tone-and-color-grading-design.md` §5) for the neighbourhood-op precedent; the
> unified maskable adjustments design (`2026-07-28`) for the `AdjustmentSet` / registry model and
> its fused layer engine.
> **Proves:** The Develop panel's four greyed NOISE REDUCTION sliders become a real, competitive
> classical denoiser; capture sharpening stops fighting it (Detail + Masking); and export gains
> medium-aware output sharpening — all with zero new dependencies, zero engine-tier changes, and
> every existing parity golden staying green **unregenerated**.

---

## 1. Goal & requirements

1. **Wire the four greyed NR sliders with a competitive classical denoiser.** `NoiseReduction
   { luminance, detail, color, color_detail }` already exists on `AdjustmentSet` and ships four
   sliders greyed with *"Noise reduction is not wired yet — coming with its GPU pass"*. This phase
   makes them real, at the quality level of RawTherapee/darktable's wavelet denoise: genuinely
   usable at ISO 3200–6400, no halos, no blotching. **Not** chasing DeepPRIME — A1 (AI denoise)
   owns that tier; the v2 map frames classical NR as the always-available baseline A1 augments.
2. **Capture sharpening gains Detail + Masking.** Today's op is `amount` + `radius` only. The V2
   design README specifies a **Detail** slider that was never built, and nothing protects flat
   areas — so sharpening re-amplifies exactly the noise NR just removed. Both are fixed here.
3. **Export gains output sharpening.** Medium-aware (Screen / Matte / Glossy) × amount tier
   (Low / Standard / High), applied after resize, before encode.
4. **Byte-identical defaults.** Every new parameter is zero-identity and every new pass is skipped
   at identity, so existing renders, existing exports, and **every existing parity golden** are
   unchanged without regeneration. This is the phase's primary regression gate (§7.2).
5. **No new dependencies, no engine-tier changes.** The wavelet is separable box-class arithmetic.
   All work is photo-tier (`ferrolite-pipeline`, `ferrolite-export`, `ferrolite-app`).

### Non-goals

* **No AI denoise.** A1 owns learned denoise (raw-domain UtNet2 + RGB NAFNet) behind the
  `ferrolite-ai` seam. This phase's NR is the classical baseline it augments.
* **No per-mask NR.** Forced by the chain position (§3.1, §3.5) — NR runs upstream of where masks
  are composited. The four NR sliders stay greyed in Mask scope with an honest reason (§6.2).
  A future per-mask NR node downstream is not foreclosed (§3.5).
* **No clarity / texture.** The reserved `AdjustmentSet::texture` / `::clarity` fields keep no
  shader and no UI. They are local-contrast tone tools, closer to P3's mandate than P4's, and were
  explicitly declared out of scope by the Phase-4 unified-adjustments plan. They stay reserved.
* **No profiled (per-camera) noise models.** Considered and rejected as partly duplicating A1.
* **No move of the sharpen node.** Sharpen stays at its current chain position — see §4.1.
* **No print/soft-proof sharpening semantics.** P8 owns print; the Matte/Glossy media here are
  output-sharpening radii only, not a proofing story.

---

## 2. Settled decisions (from the 2026-07-31 brainstorm — do not re-litigate)

| # | Question | Decision | Rationale |
|---|---|---|---|
| **P4-D1** | Phase scope | **The full v2-map scope**: classical luma + chroma NR, a capture-sharpening upgrade, and output sharpening at export. | NR and sharpening are the same tradeoff seen from two sides. Shipping NR alone leaves sharpening amplifying the noise it just removed; the user ends up fighting the two controls. |
| **P4-D2** | NR quality bar | **Competitive baseline** — RawTherapee/darktable wavelet class. | The four shipped slider labels already promise this much; a weak denoiser under those labels reads as broken. A1 owns the heavier learned tier. |
| **P4-D3** | NR algorithm | **À trous (undecimated) wavelet shrinkage** in a luma/chroma space, `L = 5` levels, soft thresholding with per-level falloff. | Maps 1:1 onto the four existing sliders; multi-scale catches fine grain AND coarse chroma blotching with one algorithm; separable, dependency-free, GPU-friendly. See §3.2 for the two rejected alternatives. |
| **P4-D4** | NR chain position | **Early and global-only** — between `color_matrix` and `vignette`. | Denoising before shadow-lifting and tone curves means a given slider value behaves the same regardless of the user's other edits. That predictability is the decisive property for a threshold-based denoiser. Cost: per-mask NR is impossible there (§3.5). |
| **P4-D5** | NR at coarse LOD | **Honest per-LOD rendering** (radius in level pixels, thresholds unscaled) **plus a "judge at 1:1" hint** in the UI. | At coarse LOD the noise really is averaged away, so the denoiser naturally no-ops — which matches what the user sees. Lightroom's model; it never lies about the pixels on screen. Scaling the radius by LOD would remove real detail in place of noise that is already gone. |
| **P4-D6** | Sharpen control set | **Amount / Radius / Detail / Masking** (Lightroom's four). | Masking (an edge mask) is the control that lets NR and sharpening coexist. Costs one slider beyond the V2 design README, which is a living doc that already absorbs accepted changes. |
| **P4-D7** | Export sharpening UI | **Two preset combos** — `Sharpen for: None/Screen/Matte/Glossy` × `Amount: Low/Standard/High` — mapping internally to `(radius, amount)`. | Output sharpening is a "what medium is this going to" decision, not a numeric one; it is judged on a screen or print the user is not currently looking at, so numeric sliders give rope without feedback. Two enums are cheap to persist and need no per-control reset. |

---

## 3. Noise reduction

### 3.1 Chain position

A new `NoiseReductionNode` is inserted between `color_matrix` and `vignette`, in **both**
`EditPipeline` (whole-image) and `TileEditPipeline` (per-tile):

```
source → color_matrix → NR → vignette → light_engine → dehaze_transmission → color_engine → sharpen → geometry
```

Two reasons for exactly this slot:

* **After `color_matrix`** so the luma/chroma decomposition happens in a well-defined working
  space rather than camera-native RGB.
* **Before `vignette`** because vignette correction multiplies the corners up. Downstream of it,
  NR would face spatially-varying noise variance and a single global threshold would either
  under-correct the centre or over-smooth the corners.

Everything downstream (`light_engine`, dehaze, `color_engine`, sharpen, geometry) is unchanged and
unmoved.

### 3.2 Algorithm — à trous wavelet shrinkage

Decompose into `L` detail scales with a separable 5-tap B3-spline kernel `[1, 4, 6, 4, 1]/16` at
hole spacing `2^l`, soft-threshold each level's coefficients, and reconstruct as
`residual + Σ shrunk_details`.

**Rejected alternatives, recorded so they are not re-proposed:**

* **Single-scale guided filter** (reusing dehaze's machinery near-verbatim, `lerp(src, smoothed,
  strength)`): least new code and a tiny halo, but it is a smoother rather than a denoiser — one
  setting cannot catch both fine grain and coarse chroma blotching, and *Detail* / *Color Detail*
  degrade to weak knobs. Below P4-D2's bar.
* **Hybrid — wavelet luma + large-radius guided chroma**: initially attractive on perceptual
  grounds, but our passes are `vec4`-wide over `rgba16float`, so running the wavelet on all three
  channels costs the **same GPU passes** as luma-only. The hybrid buys nothing and costs a second
  algorithm, a second CPU reference, and a second golden set.

### 3.3 Streaming form (memory-bounded — load-bearing)

The naive reading of the repo's heavy-map/cheap-apply pattern would cache all `L` detail levels:
approximately **7 full-res textures**, and this repo already carries live-instance/live-byte
diagnostics precisely because of that class of problem (`dehaze_node` caps its intermediates at a
working resolution — its comment calls that "the QS-Task fix for the full-res preview-tier OOM").

À trous does not need it: reconstruction is a **sum**, so shrinkage fuses into the decomposition
loop and no level is ever retained.

```
approx = to_ycbcr(src)
acc    = 0
for l in 0..L:
    next   = b3_spline_2d(approx, spacing = 1 << l)   // ONE fused 2D pass
    acc   += shrink(approx - next, threshold(l))      // fused into the same pass
    approx = next
out = to_working(acc + approx)
```

**Four live textures regardless of `L`** — `approx_a`, `approx_b`, `acc_a`, `acc_b`. Both pairs
ping-pong because each is read-modify-write across levels and a read==write binding would alias.

**The convolution is a fused 2D pass, not separable H-then-V.** For a 5-tap B3-spline the separable
form is 10 taps vs 25 fused — but it also costs a fifth full-res texture and an extra full-res
round-trip, and these passes are bandwidth-bound rather than ALU-bound. The fused 2D form is
therefore expected to be both **smaller and faster** here. (This is the opposite of the separable-
sharpen conclusion, and for a concrete reason: sharpen's box radius reaches 256, where separable's
`O(r)` vs `O(r²)` is decisive. At a fixed 5 taps that asymmetry vanishes.) `nr.rs` keeps **both**
CPU forms and `separable_b3spline_equals_direct` proves they agree, so the shipped 2D pass has a
verified oracle.

**Honest memory accounting.** These are full-res `rgba16float` textures: **192 MB each at 24 MP**,
and **0** at identity (nothing is allocated until after the passthrough early-return, and a node
that goes back to identity releases them). On the tile path — haloed tiles of ~380² — the whole set
totals ~3.6 MB, i.e. free.

**Corrected 2026-07-31 with the measured figure.** An earlier draft of this paragraph said
"~768 MB", counting only the four ping-pong intermediates. That undercounted by one texture: the
node also holds its **output**, so the real cost is `5 × w × h × 8` bytes. Measured on the largest
RAW fixture in the repo (6048×4024 = 24.3 MP) through the whole-image `EditPipeline`:
**973,486,080 bytes = 0.907 GiB** active NR, plus 0.242 GiB for the resident source pyramid, for a
**1.148 GiB total peak** — comfortably inside a 6–8 GB budget. Identity NR measured **exactly 0**.
Per the gate below, the tile-path-only fallback was therefore **not** invoked: NR ships on both the
tile and whole-image paths.

**Which paths pay it.** The develop canvas, 1:1 inspection, and export all go through
`TileEditPipeline`, so the full-res cost applies *only* to the whole-image `EditPipeline`
(reveal / before-view / thumbnail-regen).

**Pre-agreed fallback (settled 2026-07-31, so no mid-implementation decision is needed):** the
implementation measures peak GPU bytes on the largest available RAW using the existing
live-GPU-byte gauges. If that is at or near an OOM on the target 6–8 GB budget, NR becomes
**tile-path-only** — the whole-image reveal/before-view/thumbnail path renders NR-free, accepting a
transient un-denoised reveal and NR-free regenerated thumbnails, since every path the user actually
judges NR on is the tile path.

**Why no heavy/cheap node split.** The split exists to keep slider drags responsive, but a drag at
fit-view re-renders **visible VT tiles**, not the whole image, so cost is bounded by tile size and
not by sensor size. The whole-image `EditPipeline` runs on reveal / before-view / thumbnail-regen,
not per drag frame. The fused form is therefore both simpler and sufficient.

**Escape hatch, documented up front:** `engine_bench` gains an NR-dirty case (§7.4). If it misses
budget, cache only the two coarsest levels (the cheapest levels to store, the most expensive to
recompute) and re-measure. Do not reach for the escape hatch without the measurement.

### 3.4 Parameters, thresholds, and halo

`L = 5`, giving a coarsest feature scale of 32 px — enough to catch high-ISO chroma blotching —
and a halo of `2·(2^L − 1) = **62 px**`, comfortably inside what the tile machinery already
tolerates (`MAX_SHARPEN_RADIUS` is 256).

Shrinkage is **soft**, not hard: `shrink(d, t) = sign(d)·max(|d| − t, 0)`. Hard thresholding is
what produces the "plastic"/blotchy look at high strength.

**The threshold curve, stated explicitly so there is only one reading:**

```
t_l = strength · s_l · f(detail, l)
      where s_l = the standard à trous B3-spline noise-propagation constants
                  ≈ [0.890, 0.201, 0.086, 0.041, 0.020]   for l = 0..4
            f(detail, l) = 1 − detail · max(0, 1 − l/2)
```

`s_l` is the factor by which unit-variance white noise's standard deviation survives into level `l`
of a B3-spline à trous decomposition — so a *single* strength slider produces a physically
consistent threshold at every scale. `f` is the detail control: at `detail = 0` it is `1` at every
level (pure strength-scaled thresholding); at `detail = 1` it zeroes the finest level's threshold
and halves the second's, leaving coarse levels untouched — i.e. fine detail is preserved while
coarse blotching is still removed. Levels `l ≥ 2` are never attenuated by `detail`.

Slider mapping onto the four fields that already exist on `NoiseReduction`:

| Slider | Field | Feeds |
|---|---|---|
| Luminance | `luminance` | `strength` for the luma channel |
| Detail | `detail` | `detail` for the luma channel |
| Color | `color` | `strength` for the two chroma channels |
| Color Detail | `color_detail` | `detail` for the two chroma channels |

All four are `vec4`-wide in the same passes, so chroma NR is free relative to luma-only. The `s_l`
constants are the standard B3-spline noise-propagation table, not fitted to this codebase; the
§7.1 synthetic white-noise test checks only an aggregate variance reduction, which would pass for
almost any roughly-decaying constants — it does not (and is not meant to) verify these specific
per-level values.

**Post-implementation correction (final-review FIX 4):** `strength` as fed by the Luminance/Color
sliders is a raw `0..1` value in scene-linear working-space units, while `s_l` above is calibrated
for UNIT-VARIANCE noise — real RAW noise sits at σ ≈ 0.005–0.02 linear, so only the bottom few
percent of slider travel was ever useful. `nr.rs` applies a single named `NR_STRENGTH_SCALE`
constant (starting value `0.05`, author-tunable) inside `threshold_at` so the slider's full range
maps onto the useful threshold band; the formula above should be read as
`t_l = NR_STRENGTH_SCALE · strength · s_l · f(detail, l)`.

`nr_halo(&NoiseReduction) -> u32` returns `2·(2^L − 1)` when NR is active anywhere and **0 at
identity**, exactly like `sharpen_halo`. It joins the existing halo max in `tile_edit.rs`:
`sharpen_halo_doc(..).max(lens_halo_px(..)).max(nr_halo(..))`, and joins `needs_full_rebuild`'s
halo comparison in `ferrolite-app/src/develop/ops_edit.rs`.

**Identity passthrough:** when all four fields are zero the node returns `src.clone()` (a cheap
`Arc` bump) without dispatching anything — the `DehazeTransmissionNode` early-return pattern.

### 3.5 Global-only, and why (P4-D4 consequence)

Masks are composited inside the **Color-stage** engine (`local_node.rs`, `EngineStage::Color`) and
shared downstream to the sharpen node via the `SharedMasks` handle. NR sits far **upstream** of
that, so no composited mask exists at its position. Per-mask NR at this position would require
compositing masks before the Light engine — a substantially larger architectural change than this
phase's mandate.

Consequence: the four NR sliders stay `mask_ready: false` with a new honest reason (§6.2). This
does **not** foreclose per-mask NR later: a second NR node placed between `color_engine` and
`sharpen`, consuming the same `SharedMasks` handle sharpen already uses, would add it without
disturbing the global path. That is deliberately left to a follow-up.

### 3.6 Behaviour at coarse LOD (P4-D5)

NR runs on whatever LOD the requested tile is, with the radius in **level** pixels and thresholds
unscaled. At a coarse LOD the tile pixels are already downscaled, so noise has been averaged away
and a threshold-based denoiser naturally finds little to remove — which matches what the user
actually sees on screen. The fit-view preview therefore cannot and does not match the exported
full-res result; §6.2's hint line says so in the UI. Sharpen has had this same property silently
since Spec 2; the hint covers both.

---

## 4. Capture sharpening

### 4.1 Position is forced (no change)

Sharpen stays exactly where it is, between `color_engine` and `geometry`. Moving it earlier (the
classical "capture sharpening straight after demosaic" position) would place it upstream of where
masks are composited and thus **break per-mask sharpen**, which shipped in the unified-adjustments
Phase 4. The position is a constraint, not an open question.

### 4.2 Op model

```rust
pub struct Sharpen {
    pub amount: f32,
    pub radius: u32,
    #[serde(default)] pub detail: f32,   // new, 0..1, zero-identity
    #[serde(default)] pub masking: f32,  // new, 0..1, zero-identity
}
```

Both new fields must satisfy `op.rs`'s stated invariant — an identity-valued `set_op` is byte-equal
to a reset across `is_identity()`, `PartialEq` against `Default`, and the serde hash
(`hash_serde`). Defaulting to `0.0` satisfies all three.

**Corrected 2026-07-31 (author decision).** An earlier draft of this section claimed `detail` and
`masking` would be "per-mask automatically" because `Sharpen` lives inside `AdjustmentSet`. **That was
wrong.** The per-layer fields do exist and persist, but the per-layer apply pass
(`sharpen_apply_masked.wgsl`) is a *separate shader* from the global one and does not read them — so a
per-mask Detail/Masking slider would persist to the sidecar and change nothing on screen.

Therefore **Detail and Masking are GLOBAL-ONLY in this phase**, and the two sliders ship greyed in
Mask scope with an honest reason (§6.1) — the same precedent as `dehaze_radius` and as this phase's own
NR sliders. Wiring `sharpen_apply_masked.wgsl` (a per-layer fine-blur radius per distinct layer radius,
plus new per-mask parity fixtures) is deliberately deferred rather than added to an already five-task
phase. `amount` and `radius` remain per-mask exactly as before.

### 4.3 Math

```
delta_r     = src − blur_r                       // today's high-pass
delta_fine  = src − blur_{max(1, r/3)}           // narrower high-pass
edge        = smoothstep(t0(masking), t1(masking), |∇luma|)
out         = src + amount · edge · mix(delta_r, delta_fine, detail)
```

* **Detail** suppresses halos by weighting toward the narrower high-pass. `r/3` is simply another
  **distinct radius**, which the sharpen node's existing "one separable box blur per distinct
  radius across the whole evaluate" machinery already handles at no structural cost.
* **Masking** is a central-difference luma gradient (radius 1) shaped by `smoothstep`, so flat
  areas keep the noise NR removed. Stated explicitly so there is only one reading:

  ```
  t0 = masking · G          t1 = t0 + 0.25·G          G = the gradient normalization constant
  edge = masking > 0 ? smoothstep(t0, t1, |∇luma|) : 1.0
  ```

  The `masking > 0` branch is the same condition that skips the gradient pass entirely (below), so
  the degenerate `smoothstep(0, 0, x)` case is never evaluated. `G` is a single named constant
  (the tuning knob, in the spirit of `KEYSTONE_STRENGTH`) fixed during implementation against the
  §7.3 fixture.

**Identity collapse (the regression gate).** At `detail = 0, masking = 0`:
`mix(delta_r, delta_fine, 0) = delta_r` and `edge = 1`, so the formula is **byte-identical** to
today's shader. The node additionally **skips computing `blur_fine` and the gradient entirely**
when those params are zero, so existing edits gain no cost and every existing parity golden stays
green without regeneration.

### 4.4 Halo

`r` dominates `ceil(r/3)`, so the only growth is the 1 px gradient, and only when masking is
active: `sharpen_halo_doc` becomes `max_radius + (any active masking ? 1 : 0)`, keeping its
existing max-over-{global ∪ visible layers} shape and its `MAX_SHARPEN_RADIUS` clamp per
contributor.

---

## 5. Output sharpening at export

### 5.1 Options model

```rust
pub enum OutputMedium { None, Screen, Matte, Glossy }        // default None
pub enum OutputSharpenAmount { Low, Standard, High }         // default Standard
```

Added to `ExportOptions`. Defaults `None` / `Standard` make existing exports **byte-identical**.

`Medium → radius`: Screen crispest (smallest radius), Matte widest (to fight paper dot gain),
Glossy between. `Amount` tier scales the strength. **Starting table** — an explicit point of
departure so the implementer tunes rather than invents, revised against the §7.3 fixtures and the
final values recorded in the plan:

| Medium | radius (px) | `Low` | `Standard` | `High` |
|---|---|---|---|---|
| None | — | 0.0 | 0.0 | 0.0 |
| Screen | 0.7 | 0.30 | 0.50 | 0.75 |
| Glossy | 1.0 | 0.35 | 0.60 | 0.90 |
| Matte | 1.3 | 0.45 | 0.75 | 1.10 |

Radius is `f32` here (a CPU pass on the resized buffer), unlike the develop op's `u32` pixel radius
— sub-pixel radii are the point at output scale.

### 5.2 Placement in the export path

Today: `render (tiled, GPU) → quantize → resize (CPU) → encode`. Output sharpening becomes a new
pure module `ferrolite-export/src/output_sharpen.rs` (rayon-parallel separable unsharp), invoked
from `job.rs` **after resize, before encode**, handling both 8-bit and 16-bit buffers via the same
depth branch `resize.rs` already uses.

It applies whether or not a resize is active — a full-size export still benefits.

Two explicit choices, named because both could reasonably go the other way:

* **It runs in the output-encoded (gamma) domain**, not linear. Standard practice, and it avoids a
  linear round-trip purely for sharpening.
* **It computes in f32 internally and rounds once at the end**, so an 8-bit export does not
  compound quantization error through the unsharp pass.

Contract 1 is honored: this runs inside the existing export `Job`, no new job type.

---

## 6. UI

### 6.1 Effects tab — SHARPENING

Four sliders: **Amount, Radius, Detail, Masking**. `detail` and `masking` are new registry entries
in `ferrolite-app/src/develop/adjustments.rs` with `global_ready: true` **and** `mask_ready: true`
(per-mask comes free, §4.2). Each carries the shared per-control reset affordance
(`widgets::draw_reset_arrow` + the `EguiSlider` reset column) — a new editable control is not
complete without it (CLAUDE.md).

### 6.2 Effects tab — NOISE REDUCTION

The four existing sliders flip to `global_ready: true` with `global_reason` cleared. They keep
`mask_ready: false`, with the placeholder reason replaced by an accurate one:

> "Noise reduction runs before the tone and color stages so its strength stays independent of your
> other edits — global only."

Same greyed-with-reason precedent `dehaze_radius` already sets. A one-line subheader under the
section label carries the LOD hint, matching the REGION TONES subheader convention:

> "Judge noise reduction and sharpening at 1:1."

The existing "AI" chip on the section stays — A1 fills it.

### 6.3 Export settings panel

Two combos, `Sharpen for` and `Amount`, in the panel's established control-left / label-right row
style alongside Format / Color space / Quality / Resize.

### 6.4 Conventions that need no work (verified, stated so nobody re-checks)

* **No new keybinds or gestures** ⇒ no Settings-keyboard `GROUPS` entry and no Help-panel shortcut
  row. `every_action_is_in_a_settings_group` is unaffected.
* **No new `*_open` disclosure flags** — `noise_reduction_open` and `mask_noise_reduction_open`
  both already exist in `settings/dto.rs`, so `app.rs`'s count-asserting `disclosure_snapshot`
  test is unaffected.
* **All icons** stay sourced from `icons.rs`; nothing new is drawn.

### 6.5 Design-doc delta

`docs/design/V2/README.md` is updated in the same branch: the Masking slider in the Effects tab's
SHARPENING list, the 1:1 hint line under NOISE REDUCTION, and the two export combos in the Export
right-panel description.

---

## 7. Testing

### 7.1 Pure-math references and unit tests

* `nr::atrous_shrink_reference` — a pure CPU implementation of the whole §3.3 loop, goldened
  against the GPU node within f16 tolerance. This is the `dehaze::transmission_map` pattern:
  expose the spatial math as a pure fn, golden the GPU against it.
* `separable_b3spline_equals_direct` — CPU-vs-CPU proof that the separable H-then-V B3-spline
  convolution equals the direct 2D form within 1e-6, mirroring the existing
  `separable_box_equals_2d_box` test that preceded the separable sharpen work.
* Threshold-mapping unit tests: each of the four sliders moves the intended threshold curve in the
  intended direction, and all-zero yields all-zero thresholds.

### 7.2 Identity gates (the primary regression barrier)

Three separate gates, each guarding a class of existing artifacts:

1. **NR identity** — all four fields zero ⇒ `src.clone()` early return, no dispatch, byte-identical.
2. **Sharpen identity** — `detail = 0, masking = 0` ⇒ byte-identical to the pre-change shader, and
   **every existing parity golden stays green without regeneration**.
3. **Export identity** — `None` / `Standard` ⇒ byte-identical to current exports, guarding every
   existing export test.

If gate 2 or 3 fails, the cause is a bug in the identity path, **not** a golden to adjudicate.
This differs deliberately from the unified-adjustments phases, where fusing f16 round-trips made
bounded drift legitimate; here nothing is re-fused, so there is no drift mechanism to accept.

**Two repo asserts that this phase legitimately touches** — named so they are updated
deliberately rather than discovered as failures:

* **The hand-maintained `node_count` field** in `ferrolite-pipeline/src/pipeline.rs` (currently
  `node_count: 8`, with a comment enumerating "source, color-matrix, vignette, light-engine,
  dehaze-transmission, color-engine (recovery fused in), sharpen, geometry"). Adding the NR node
  makes it 9 — update the count **and** the enumerating comment together. It is exposed via
  `node_count()` and consumed by two assertions in `ferrolite-pipeline/tests/golden.rs`:
  `assert_eq!(pipe.eval_count(), pipe.node_count())` — which still holds, because an
  identity NR node is still *evaluated* (it early-returns `src.clone()`, which counts as an eval) —
  and `prev + (pipe.node_count() - 3)`, whose magic `- 3` offset **must be re-derived by reading
  what those three skipped nodes are**, not blindly kept or bumped to 4.
* **`PIPELINE_SCHEMA_VERSION`** in `ferrolite-previews` must **NOT** be bumped. It keys the
  identity-render preview cache, and preview write-back is gated on `OpStack::default()` — where NR
  is identity and the render is unchanged. Bumping it would needlessly invalidate every cached
  preview. (The separate `hash_serde` shift from `Sharpen`'s new fields is unrelated and expected;
  see §8, contract 2.)

### 7.3 New parity fixtures

* `nr_luma` — luma NR only, on a high-ISO fixture.
* `nr_chroma` — chroma NR only. Guards the specific failure mode that chroma shrinkage
  desaturates hard color edges.
* `sharpen_detail_masking` — non-zero `detail` and `masking` together.
* **`nr_tile_seam` — tiled-vs-whole parity, with a deliberately high-frequency fixture at the tile
  seam.** This test MUST fail if the 62 px halo fold-in is removed. A smooth-gradient fixture
  passes even when seam handling is broken, which would make it a fake test — the same trap
  recorded from the dehaze work.
* Export fixtures at 8-bit and 16-bit for each medium.

### 7.4 Halo and perf

* `nr_halo` returns `2·(2^L − 1)` when active and 0 at identity; the `tile_edit.rs` halo max
  includes it; an NR-only change forces a full rebuild via `needs_full_rebuild`.
* `sharpen_halo_doc` accounts for masking's `+1` and remains a max over global ∪ visible layers.
* New `engine_bench` cases: NR-dirty evaluate, and NR + sharpen combined. Recorded in the
  benchmark doc. Gate: **no regression on existing cases**; NR's own cost is recorded as a new
  baseline, and it is what §3.3's escape hatch is judged against.
* **Peak-GPU-memory measurement (gates §3.3's fallback).** Using the existing
  `live_gpu_pyramid_bytes`-style gauges, record peak GPU bytes with NR active on the largest
  available RAW through the whole-image `EditPipeline`, and confirm identity NR allocates **zero**
  NR textures. If the active figure is at or near an OOM on a 6-8 GB budget, take §3.3's
  pre-agreed tile-path-only fallback rather than opening a new decision.

### 7.5 UI tests

NR sliders enabled in Adjust scope and disabled-with-the-new-reason in Mask scope; sharpen Detail
and Masking enabled in **both** scopes; per-control reset present on all four sharpening sliders.

---

## 8. Contracts, tiers, and CLAUDE.md rules honored

* **Contract 1 (jobs are universal):** export output sharpening runs inside the existing export
  `Job`; no new slow work reaches the UI thread.
* **Contract 2 (catalog is a cache):** the two new `Sharpen` fields persist in the OpStack sidecar,
  not the catalog. `#[serde(default)]` means old sidecars load as `detail = 0, masking = 0` —
  today's exact behavior. `hash_serde` changes, so warm/preview cache keys shift and the first open
  after upgrade is a one-time cache miss. Harmless and expected.
* **Contract 3 (decode products are additive):** untouched — nothing about decode changes.
* **Contract 4 (the GPU executor is photo-agnostic):** NR arrives as a `Node<PipelineImage>`
  supplied by `ferrolite-pipeline`; `ferrolite-gpu`'s generic retained-DAG executor is not
  modified and nothing reaches into its internals.
* **Contract 5 (the VT is source-agnostic):** NR is a halo consumer on the existing halo plumbing;
  `ferrolite-vt` gains no photo concepts.
* **Licensing tiers:** all work is photo-tier (`ferrolite-pipeline`, `ferrolite-export`,
  `ferrolite-app`). **No new dependencies at all** — the wavelet is separable box-class arithmetic.
  The engine-transferable tier is untouched, so it stays copyleft-free and weight-free.
* **Build-once GPU pipelines:** every new shader's pipeline is built once in the node's `new` and
  added to the startup prewarm list; nothing is rebuilt per image, per open, or per interaction.
* **Never block the UI thread:** no new synchronous full-res work on the update thread; tile
  rendering stays the drag-time path.
* **Per-control reset:** on all four sharpening sliders and all four NR sliders.

---

## 9. Risks

| Risk | Mitigation |
|---|---|
| Wavelet shrinkage goes **plastic / blotchy** at high strength. | Soft thresholding with per-level falloff (§3.4). This defect is invisible to `cargo test` — the author's hands-on visual test is the real gate, exactly as it was for dehaze's halos. |
| Chroma shrinkage **desaturates hard color edges**. | The `nr_chroma` fixture (§7.3) exists specifically for this. |
| The 62 px halo makes a haloed 256 px tile **~2.2× the area**. | Paid only while NR is active; `nr_halo` is 0 at identity, so no existing edit gets slower. |
| NR's 4 full-res textures (~768 MB at 24 MP) **OOM the whole-image path** on a 6-8 GB budget. | Measured in implementation via the existing live-GPU-byte gauges on the largest available RAW; the pre-agreed fallback (§3.3) makes NR tile-path-only, which costs ~2.9 MB. Zero bytes at identity. |
| NR **slider drag lag** at 1:1. | `engine_bench` NR-dirty case gates it; §3.3's two-coarsest-level cache is the documented, measurement-gated escape hatch. |
| The fit-view preview **does not match the export** (inherent, §3.6). | The 1:1 hint line (§6.2). Accepted per P4-D5 — the alternative lies about the pixels on screen. |
| Output sharpening on **quantized 8-bit** data could posterize. | f32 internally, one rounding at the end (§5.2). |

---

## 10. Plan decomposition

Expected to become a single `writing-plans` cycle on one branch, with tasks in this order (the
plan settles the final split):

1. **NR engine** — pure CPU reference + `separable_b3spline_equals_direct`, then the WGSL passes
   and `NoiseReductionNode` in both pipelines, wired at §3.1's position; identity gate.
2. **NR halo + tiling** — `nr_halo`, the `tile_edit.rs` halo max, `needs_full_rebuild`, and the
   `nr_tile_seam` golden that must fail without the fold-in.
3. **NR UI** — ungrey in global scope, the new mask-scope reason, the 1:1 hint, gating tests.
4. **Sharpen Detail + Masking** — op fields, shader, halo `+1`, UI sliders with per-control reset;
   the byte-identical gate for existing goldens.
5. **Export output sharpening** — `output_sharpen.rs`, `ExportOptions`, `job.rs` wiring, the two
   combos, 8/16-bit fixtures, the byte-identical gate.
6. **Benchmarks + docs** — `engine_bench` cases recorded, `docs/design/V2/README.md` delta.

Per CLAUDE.md's gate tiers: subagents run the **scoped gate** for their crate(s); the coordinator
runs the **repo gate** once at the end on the latest stable, then **holds for the author's visual
test** with a numbered checklist before finishing the branch.

---

## 11. Reference

* **v2 architecture map** — `2026-07-05-ferrolite-v2-architecture-map.md` §4 P4 (this phase's
  parent), §5 (the six contracts), §6 (build order: P4 is soft-ordered, unblocked).
* **Spec 2 (Editing)** — `2026-06-30-spec2-editing-design.md`: the `Sharpen` op, the OpStack
  sidecar, the VT halo + GPU tile producer this phase extends.
* **P3 design** — `2026-07-08-p3-tone-and-color-grading-design.md` §5: dehaze, the in-repo
  precedent for a guided/neighbourhood op incl. its tiling treatment of a global statistic.
* **Unified maskable adjustments** — `2026-07-28-unified-maskable-adjustments-design.md`: the
  `AdjustmentSet` model, the adjustment registry with `global_ready`/`mask_ready` + hover reasons,
  and the fused layer engine + `SharedMasks` handle that fixes sharpen's (and NR's) position.
* **Phase-4 unified plan** — `docs/superpowers/plans/2026-07-29-unified-engine-phase4-mask-neighborhood.md`:
  separable sharpen, per-mask sharpen, and the explicit deferral of NR/clarity/texture that this
  phase picks up.
* **Benchmark doc** — `docs/benchmarks/2026-07-28-phase3-fused-engine.md`: the `engine_bench`
  harness, method, and current baselines.
* **Design system** — `docs/design/V2/README.md`: the Effects tab and Export panel this phase
  edits.

---

## 12. Known issues & follow-ups (recorded at merge, 2026-07-31)

Carried out of the implementation's review record so they survive the deletion of the
git-ignored SDD scratch. None blocked the merge; the whole-branch review triaged each.

**Known issue — tiled-vs-whole divergence at the true canvas edge.** With NR active, the
whole-image render and the settled tiled render disagree by up to ~0.06 (display-linear) within
~90 px of the true image border. Root cause is a geometry-resample discrepancy at the canvas
edge, independent of tile-seam halo handling. It is **excluded**, not fixed: the tile-seam
golden's `NR_EDGE_MARGIN` (`ferrolite-pipeline/tests/golden.rs`) skips that band. Note this is
the opposite of the precedent in the same file — the dehaze tiled tests once needed such a margin
and it was **removed** once the cause was fixed. Possible user-visible effect: the frame edge may
pop when tiles replace the initial reveal. Worth root-causing.

**Follow-up — delete `nr_clear.wgsl` entirely.** `nr_atrous.wgsl` already branches on
`p.level == 0`. Having level 0 write `shrunk` directly instead of `acc_in + shrunk` removes the
clear shader, its pipeline/BGL/bind group, one dispatch per evaluate, a `prewarm_shaders` entry,
**and** the whole "the accumulator must be zeroed every evaluate" correctness class that consumed
most of this phase's risk budget. Deferred only to avoid churning verified-correct code at branch
end.

**Follow-up — pre-warm the NR and detail-sharpen passes.** `prewarm_pipelines` evaluates
`OpStack::default()`, where NR early-returns and sharpen is inactive, so `nr-atrous` / `nr-clear` /
`nr-combine` / `sharpen-apply-detail` are never *dispatched* at startup and the driver compiles
them on first use, on the render thread. Consistent with dehaze's existing behaviour, but
CLAUDE.md names pre-warm load-bearing; a second dummy evaluate with NR + detail/masking active
would close it.

**Follow-up — `SharpenNode::blur_slots` grows monotonically.** Touching Detail once with a global
sharpen active adds a blur pair (~384 MB at 24 MP on the whole-image pipeline), and returning
Detail to 0 does not change the halo, so no rebuild occurs and the slot stays parked for the
pipeline's lifetime. Same class as the "strength-to-0 must not leave ~1 GiB parked" issue that
`NoiseReductionNode` deliberately fixes by releasing in its inactive branch.

**Tuning knob.** `NR_STRENGTH_SCALE` (`ferrolite-pipeline/src/nr.rs`, currently `0.05`) is the
single constant governing NR strength calibration. It was set from first principles
(`3σ·s_l` for σ ≈ 0.005–0.02 linear) and is the intended adjustment point if real-world use shows
the slider range is off.

**Cosmetic.** The retired reference shader `sharpen.wgsl` still names the last two uniform fields
`pad0`/`pad1` (dead code — not in `prewarm_shaders`, bound to nothing). `uniforms.rs`'s
`nr_uniform` sets `active` from `!is_identity()`, the last activity-flavoured use of that
predicate — provably unreachable for detail-only NR, but `is_active()` would be more robust.
`docs/design/V2/README.md` and §6.3 call the second export combo "Amount" where the code labels it
"Sharpen amount".
