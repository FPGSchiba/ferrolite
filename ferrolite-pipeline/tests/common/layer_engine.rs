//! Shared parity fixtures + 16-bit-PNG golden helpers for the fused-layer-engine
//! phase (design doc `2026-07-28-unified-engine-phase3-fused-layers`).
//!
//! **History:** Task 1 originally rendered `fixture_docs()` through the
//! pre-fusion `EditPipeline` chain and committed the results as goldens under
//! `tests/golden/layer_engine/`, so Tasks 2-3 could prove the new two-segment
//! engine reproduced them within `PARITY_TOL`. That old-vs-new parity job
//! completed 2026-07-29 — it caught a real bug (the shared `adjust()` shader's
//! floor clamp wrongly hitting the new global pseudo-layer dispatches) and
//! forced the post-global-color-segment mask-compositing fix. The residual
//! deltas it then measured (up to 0.6 on `two_masks`) were root-caused to
//! inherent floating-point/hue-domain sensitivity from removing intermediate
//! `rgba16float` round-trips and adjudicated as an accepted precision
//! improvement, not a defect (author-approved 2026-07-29; full evidence in
//! `docs/benchmarks/2026-07-28-phase3-fused-engine.md`'s "Accepted rendering
//! deltas vs the pre-fusion chain" section). The goldens here were then
//! regenerated FROM the fused engine.
//!
//! **Going forward**, `fixture_docs()`/these goldens pin the FUSED engine
//! against future drift, not fusion-vs-pre-fusion parity — a regression here
//! means the fused engine's own output changed. Keep this module's path
//! (`ferrolite-pipeline/tests/common/layer_engine.rs`, reached via
//! `mod common; common::layer_engine::...`) stable so future tasks can import
//! it unchanged.
//!
//! **2026-07-29 addendum (Phase 4, `2026-07-29-unified-engine-phase4-mask-neighborhood`):**
//! `mask_dehaze` and `mask_sharpen` were added to cover the per-mask
//! neighborhood ops Tasks 2-4 of that phase landed (dehaze recovery fused
//! into the Color engine + per-mask dehaze amount; separable sharpen +
//! per-mask sharpen at a distinct radius). Rendered directly through the
//! already-fused engine (no pre-fusion comparison — that job is done), so
//! their goldens were written fresh, not regenerated from an older render.
#![allow(dead_code)]

use ferrolite_image::LinearRgbaF32;
use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition, Rgb, Vec2};
use ferrolite_pipeline::{
    AdjustmentSet, ColorGrade, ColorSwatch, Contrast, Dehaze, Exposure, GradeWheel, Hsl,
    LocalAdjustments, MaskLayer, NoiseReduction, Op, OpStack, Sharpen, ToneCurve, WhiteBalance,
};

/// Tolerance for comparing rendered fixture output (scene-linear f32, RGB
/// channels) against its committed golden. Shared verbatim by every task in
/// this phase so "reproduce the golden" means the same thing everywhere.
pub const PARITY_TOL: f32 = 2e-3;

// ---------------------------------------------------------------------------
// Synthetic source
// ---------------------------------------------------------------------------

/// Standard HSV -> linear RGB (h in degrees [0, 360), s/v in [0, 1]).
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let c = v * s;
    let hp = h / 60.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (r1 + m, g1 + m, b1 + m)
}

/// Deterministic 512x512 HSV sweep, generated in-code (no committed source
/// asset). Hue sweeps across x (0..360) for every row. The top half sweeps
/// value down y at full saturation (so it covers near-black through
/// near-full-bright); the bottom half sweeps saturation down y at full value
/// (so it covers vivid through near-grey/near-white). Together this exercises
/// the full hue/sat/value cube well enough to catch a fused-engine regression
/// that only shows up in some corner of it (e.g. a HSL band boundary, a
/// near-black dehaze/curve edge case, or a near-white saturation clamp).
pub fn hsv_sweep_source() -> LinearRgbaF32 {
    let (w, h) = (512u32, 512u32);
    let half = h / 2;
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let hue = 360.0 * x as f32 / w as f32;
            let (sat, val) = if y < half {
                (1.0, y as f32 / half as f32)
            } else {
                (1.0 - (y - half) as f32 / half as f32, 1.0)
            };
            let (r, g, b) = hsv_to_rgb(hue, sat, val);
            px.extend_from_slice(&[r, g, b, 1.0]);
        }
    }
    LinearRgbaF32::new(w, h, px).expect("hsv sweep length")
}

// ---------------------------------------------------------------------------
// Fixture docs
// ---------------------------------------------------------------------------

/// `light_trio`'s three global ops: exposure +0.8 EV, contrast +0.35, temp
/// +0.4/tint -0.2. Also the base every masked fixture layers global edits on
/// top of, per the brief.
fn light_trio_stack() -> OpStack {
    OpStack::default()
        .set_op(Op::Exposure(Exposure { ev: 0.8 }))
        .set_op(Op::Contrast(Contrast { amount: 0.35 }))
        .set_op(Op::WhiteBalance(WhiteBalance {
            temp: 0.4,
            tint: -0.2,
        }))
}

/// Layers `curve_hsl_grade`'s tone-curve/HSL/grade ops on top of `base`: tone
/// curve [(0,0.1),(0.5,0.55),(1,1)], HSL band 0 sat +0.4 / band 3 hue -0.3,
/// grade shadows {hue:210, sat:0.5} + blending 0.7.
fn with_curve_hsl_grade(base: OpStack) -> OpStack {
    let mut hsl = Hsl::default();
    hsl.bands[0].sat = 0.4;
    hsl.bands[3].hue = -0.3;
    base.set_op(Op::ToneCurve(ToneCurve {
        points: vec![(0.0, 0.1), (0.5, 0.55), (1.0, 1.0)],
        ..Default::default()
    }))
    .set_op(Op::Hsl(hsl))
    .set_op(Op::ColorGrade(ColorGrade {
        shadows: GradeWheel {
            hue: 210.0,
            sat: 0.5,
            lum: 0.0,
        },
        blending: 0.7,
        ..Default::default()
    }))
}

/// The masked local layer shared by `one_mask`, `two_masks`, and `mask_only`:
/// full-coverage mask (`MaskDefinition::default()`, no components -> all-ones,
/// same convention `local_node.rs`'s tests use), exposure -1.0, contrast +0.3,
/// temp -0.3, a tone-curve lift, an HSL band saturation bump, and a grade
/// shadows tint.
fn masked_layer_light_trio_variant() -> MaskLayer {
    let mut hsl = Hsl::default();
    hsl.bands[0].sat = 0.35;
    MaskLayer {
        name: "masked-light-trio".into(),
        visible: true,
        mask: MaskDefinition::default(),
        adjustments: AdjustmentSet {
            exposure: -1.0,
            contrast: 0.3,
            temp: -0.3,
            tone_curve: ToneCurve {
                points: vec![(0.0, 0.15), (1.0, 1.0)],
                ..Default::default()
            },
            hsl,
            color_grade: ColorGrade {
                shadows: GradeWheel {
                    hue: 200.0,
                    sat: 0.4,
                    lum: 0.0,
                },
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

/// The masked layer for `luma_range_mask`: a real LUMINANCE RANGE mask
/// component (mid-tones [0.3, 0.7], softness 0.1) driving a strong, clearly
/// visible exposure -1.5.
///
/// Why this exists: a later fused-layer-engine task moves the stack's global
/// color ops (tone curve, HSL, color grade, ...) INSIDE the per-mask node.
/// `LumaRange` (and `ColorRange`) composite against the node's *input* image
/// (`luma_range.wgsl`/`color_range.wgsl` both `textureLoad(src, ...)` the
/// node's input texture directly) — so what the input contains at the point
/// the mask samples it is behavior-defining. Today, with global color ops
/// applied to the whole stack before `LocalAdjustments` runs, a range mask
/// keys off *post-color-grade* content. If the rewiring moves color grading
/// inside the mask node without preserving that ordering, the same mask
/// definition would key off different (pre-grade) content and silently
/// select different pixels. This fixture — global grade/curve on top of a
/// range-masked layer, rendered through the CURRENT chain and pinned as a
/// golden — exists so that regression is caught by parity comparison rather
/// than discovered visually later.
fn masked_layer_luma_range() -> MaskLayer {
    MaskLayer {
        name: "luma-range".into(),
        visible: true,
        mask: MaskDefinition {
            components: vec![(
                MaskComponent::LumaRange {
                    lo: 0.3,
                    hi: 0.7,
                    softness: 0.1,
                },
                CompositeMode::Add,
            )],
            invert: false,
        },
        adjustments: AdjustmentSet {
            exposure: -1.5,
            ..Default::default()
        },
    }
}

/// The masked layer for `color_range_mask`: same pin as
/// `masked_layer_luma_range` (see its doc comment for the gist), but using a
/// COLOR RANGE component instead of luminance — selection around pure red
/// (tolerance 0.3, softness 0.1), again driving a strong exposure -1.5.
/// `color_range.wgsl` reads the node's input the same way `luma_range.wgsl`
/// does, so it's equally cheap to pin and equally exposed to the same
/// input-content regression.
fn masked_layer_color_range() -> MaskLayer {
    MaskLayer {
        name: "color-range".into(),
        visible: true,
        mask: MaskDefinition {
            components: vec![(
                MaskComponent::ColorRange {
                    samples: vec![Rgb::new(1.0, 0.0, 0.0)],
                    tolerance: 0.3,
                    softness: 0.1,
                },
                CompositeMode::Add,
            )],
            invert: false,
        },
        adjustments: AdjustmentSet {
            exposure: -1.5,
            ..Default::default()
        },
    }
}

/// The second masked layer added by `two_masks`: full-coverage mask,
/// saturation +0.5, hue +0.2, color-swatch blend (amount 0.4, pure red).
fn masked_layer_saturation_swatch() -> MaskLayer {
    MaskLayer {
        name: "masked-sat-swatch".into(),
        visible: true,
        mask: MaskDefinition::default(),
        adjustments: AdjustmentSet {
            saturation: 0.5,
            hue: 0.2,
            color: ColorSwatch {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                amount: 0.4,
            },
            ..Default::default()
        },
    }
}

/// The masked layer for `mask_dehaze`: a LEFT/RIGHT split `LinearGradient`
/// mask (half the frame at 0.0, half at 1.0, matching the pattern
/// `local_node.rs`'s own per-mask-dehaze tests use to isolate masked from
/// unmasked pixels) driving ONLY `adjustments.dehaze.amount = 0.5` — no other
/// adjustment on this layer, so a parity regression here can be attributed
/// to the per-mask dehaze wiring (Task 3) and not entangled with any other
/// per-layer step.
fn masked_layer_dehaze_only() -> MaskLayer {
    MaskLayer {
        name: "mask-dehaze".into(),
        visible: true,
        mask: MaskDefinition {
            components: vec![(
                MaskComponent::LinearGradient {
                    start: Vec2::new(0.0, 0.5),
                    end: Vec2::new(1.0, 0.5),
                },
                CompositeMode::Add,
            )],
            invert: false,
        },
        adjustments: AdjustmentSet {
            dehaze: Dehaze {
                amount: 0.5,
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

/// The masked layer for `mask_sharpen`: full-coverage mask (`MaskDefinition::default()`
/// — the sharpen node's own masked-apply path already gets partial-coverage
/// coverage from `sharpen_node.rs`'s unit tests, so this fixture's job is
/// exercising the DISTINCT-radius two-blur path at the whole-pipeline level,
/// not re-proving partial masking) driving ONLY `adjustments.sharpen`, amount
/// 1.0 at radius 4 — deliberately different from the global sharpen's radius
/// 2 (see `mask_sharpen` in `fixture_docs()`) so the fixture forces
/// `SharpenNode` to compute two distinct separable blurs in one evaluate.
fn masked_layer_sharpen_distinct_radius() -> MaskLayer {
    MaskLayer {
        name: "mask-sharpen".into(),
        visible: true,
        mask: MaskDefinition::default(),
        adjustments: AdjustmentSet {
            sharpen: Sharpen {
                amount: 1.0,
                radius: 4,
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

/// The shared fixture set: name -> doc. Consumed by the parity test (this
/// task) and by `engine_bench.rs`'s "case (c)" (which pulls `two_masks`'
/// `LocalAdjustments` back out via `OpStack::local_adjustments()` and layers it
/// onto `full_global`). `identity` MUST stay first — the parity test diffs
/// every other fixture against it as a no-op sanity check.
pub fn fixture_docs() -> Vec<(&'static str, OpStack)> {
    vec![
        ("identity", OpStack::default()),
        ("light_trio", light_trio_stack()),
        ("curve_hsl_grade", with_curve_hsl_grade(OpStack::default())),
        (
            "full_global",
            with_curve_hsl_grade(light_trio_stack())
                .set_op(Op::Sharpen(Sharpen {
                    amount: 0.8,
                    radius: 2,
                    ..Default::default()
                }))
                .set_op(Op::Dehaze(Dehaze {
                    amount: 0.3,
                    radius: 8,
                })),
        ),
        (
            "one_mask",
            light_trio_stack().set_op(Op::LocalAdjustments(LocalAdjustments {
                layers: vec![masked_layer_light_trio_variant()],
            })),
        ),
        (
            "two_masks",
            light_trio_stack().set_op(Op::LocalAdjustments(LocalAdjustments {
                layers: vec![
                    masked_layer_light_trio_variant(),
                    masked_layer_saturation_swatch(),
                ],
            })),
        ),
        (
            "mask_only",
            OpStack::default().set_op(Op::LocalAdjustments(LocalAdjustments {
                layers: vec![masked_layer_light_trio_variant()],
            })),
        ),
        (
            "wb_contrast_both",
            OpStack::default()
                .set_op(Op::WhiteBalance(WhiteBalance {
                    temp: 0.5,
                    tint: 0.0,
                }))
                .set_op(Op::Contrast(Contrast { amount: 0.5 })),
        ),
        // `luma_range_mask` / `color_range_mask`: `curve_hsl_grade`'s global
        // grade + tone curve on top of a range-masked local layer. See
        // `masked_layer_luma_range`'s doc comment for why these pin
        // range-mask-vs-input-content semantics ahead of the fused-layer
        // rewiring.
        (
            "luma_range_mask",
            with_curve_hsl_grade(OpStack::default()).set_op(Op::LocalAdjustments(
                LocalAdjustments {
                    layers: vec![masked_layer_luma_range()],
                },
            )),
        ),
        (
            "color_range_mask",
            with_curve_hsl_grade(OpStack::default()).set_op(Op::LocalAdjustments(
                LocalAdjustments {
                    layers: vec![masked_layer_color_range()],
                },
            )),
        ),
        // `vibrance_global`: a global vibrance adjustment (0.5), regression-
        // pinning fix(es) to the global-order vibrance branch. Vibrance has no
        // dedicated `Op` variant — it's a plain `AdjustmentSet` field written
        // via `with_global` (the same scoped-edit write path the app uses for
        // `EditScope::Global`).
        //
        // Originally added (comment preserved for history) because a *pure*
        // vibrance-only doc was indistinguishable from identity against
        // `hsv_sweep_source` under the THEN-current HSL-round-trip formula:
        // every non-grey pixel in that source has either HSV S=1 or V=1, and
        // `rgb_to_hsl`'s `s = d / (1 - |2l-1|)` reduced to exactly 1 whenever
        // one channel sat at the 0/1 rail, pinning the old fade weight
        // `w = clamp(s, 0, 1)` to 1 (no-op) for virtually the whole image. A
        // small global exposure lift (+1.5 EV) broke that by pushing the
        // source's already-at-1.0 channel past 1.0 into scene-linear
        // over-range — the same over-range regime that, at `l == 1.0`
        // exactly, hits `rgb_to_hsl`'s denominator singularity and produced
        // the NaN/black-pixel bug this file's current fix addresses (see the
        // CPU unit test `vibrance_is_finite_on_l_equals_one_and_l_greater_than_one_pixels`).
        // The CURRENT vibrance implementation no longer round-trips through
        // HSL at all (see `hsv_sat_measure` in `uniforms.rs`), so this exact
        // "indistinguishable without the exposure kick" argument no longer
        // strictly applies — but the +1.5 EV exposure is kept so this fixture
        // still exercises vibrance on legitimate scene-linear over-range
        // pixels (its regression value is unchanged). Exposure lives in
        // `light_segment` (applied at the Light-stage node) and vibrance in
        // `color_segment` (applied at the Color-stage node), so both fields on
        // one `global` AdjustmentSet compose exactly as the app would:
        // exposure first, vibrance second, on the same over-bright pixels.
        (
            "vibrance_global",
            OpStack::default().with_global(AdjustmentSet {
                exposure: 1.5,
                vibrance: 0.5,
                ..Default::default()
            }),
        ),
        // `mask_dehaze`: Phase 4 (per-mask neighborhood) coverage — a global
        // dehaze op (0.3/r8, same params `full_global` already pins) alongside
        // a half-coverage mask layer's OWN dehaze amount (0.5), driven off the
        // SAME shared transmission map. Exercises the global recovery path
        // (Task 2) and the per-mask recovery path (Task 3) together in one
        // render. Base is `OpStack::default()` (unlike `one_mask`/`two_masks`,
        // which layer on `light_trio_stack()`) — compounding light_trio's
        // exposure/contrast/WB with a DOUBLE dehaze-recovery application
        // (global then per-mask, on the same masked half) pushed this HSV
        // sweep's near-black corner's recovered value past this file's
        // `GOLDEN_MAX` encoding headroom (36.7 vs the [-1, 8] range); dropping
        // light_trio keeps the fixture's job (double-recovery composition)
        // isolated without widening the shared golden encoding range for
        // every other fixture.
        (
            "mask_dehaze",
            OpStack::default()
                .set_op(Op::Dehaze(Dehaze {
                    amount: 0.3,
                    radius: 8,
                }))
                .set_op(Op::LocalAdjustments(LocalAdjustments {
                    layers: vec![masked_layer_dehaze_only()],
                })),
        ),
        // `mask_sharpen`: Phase 4 (per-mask neighborhood) coverage — a global
        // sharpen (0.8/r2, same params `full_global` already pins) plus one
        // full-coverage mask layer's OWN sharpen at a DISTINCT radius (1.0/r4),
        // forcing `SharpenNode` to compute two separate separable blurs (one
        // per distinct radius) in a single evaluate — the two-blur path Task 4
        // added.
        (
            "mask_sharpen",
            light_trio_stack()
                .set_op(Op::Sharpen(Sharpen {
                    amount: 0.8,
                    radius: 2,
                    ..Default::default()
                }))
                .set_op(Op::LocalAdjustments(LocalAdjustments {
                    layers: vec![masked_layer_sharpen_distinct_radius()],
                })),
        ),
        // `nr_luma`: P4 (noise-reduction phase) coverage — luma-only à trous
        // NR (strength 0.8, detail 0.2) COMPOSED WITH `light_trio`'s exposure/
        // contrast/WB, so the fixture exercises NR alongside the pre-existing
        // Light-stage engine rather than in isolation (NR sits upstream of the
        // light engine, so "NR then light adjustments" is a genuinely distinct
        // scenario from "NR alone" — the one this fixture is meant to pin).
        //
        // A first attempt wrote this via `light_trio_stack().with_global(AdjustmentSet
        // { noise_reduction: ..., ..Default::default() })` — WRONG, caught in review:
        // `EditDoc::with_global` does `d.global = set.normalized()`, replacing `global`
        // WHOLESALE, so that call silently zeroed the exposure/contrast/temp/tint
        // `light_trio_stack()` had just set, leaving the doc's `global.exposure == 0.0`
        // etc. (confirmed empirically) — the fixture rendered NR in isolation on the bare
        // sweep, contradicting its own doc comment. Fixed below by mutating
        // `global.noise_reduction` directly on the ALREADY-BUILT `light_trio_stack()`
        // doc (the same direct-field pattern `nr_node.rs`'s and `engine_bench.rs`'s own
        // NR tests use), so light_trio's fields survive. `vibrance_global`
        // (a few fixtures above) remains the correct precedent for a genuinely
        // GLOBAL-ONLY `AdjustmentSet` field with no dedicated `Op` variant written via
        // a SINGLE `with_global` call that sets every field it needs at once — the bug
        // here was calling `with_global` a SECOND time on a doc whose `global` was
        // already populated by a different path (`set_op`), not `with_global` itself.
        ("nr_luma", {
            let mut d = light_trio_stack();
            d.global.noise_reduction = NoiseReduction {
                luminance: 0.8,
                detail: 0.2,
                ..Default::default()
            };
            d
        }),
        // `nr_chroma`: chroma-only NR (strength 0.8, chroma detail 0.2) composed with
        // the same `light_trio` base (see `nr_luma`'s comment for the composition-bug
        // history and why the direct-field-mutation form below is correct). Guards the
        // specific failure mode design §9 calls out: chroma shrinkage desaturating hard
        // color edges — this fixture pins the fused engine's chroma NR output against
        // exactly that risk on the HSV-sweep source's saturated bands.
        ("nr_chroma", {
            let mut d = light_trio_stack();
            d.global.noise_reduction = NoiseReduction {
                color: 0.8,
                color_detail: 0.2,
                ..Default::default()
            };
            d
        }),
        // `sharpen_detail_masking`: P4 sharpen upgrade coverage — non-zero
        // `detail` (narrows the high-pass toward `r/3` to suppress halos) and
        // `masking` (suppresses sharpening in flat areas) TOGETHER, at a
        // higher amount/radius than any existing sharpen fixture. Layered on
        // `curve_hsl_grade`'s tone-curve/HSL/grade (the same base
        // `full_global` uses) rather than the bare HSV sweep: a first attempt
        // at this fixture used `OpStack::default()` (sweep source only) and
        // rendered BIT-IDENTICAL to `identity` (max diff exactly 0) — caught
        // by this suite's own no-op sanity assert. Root cause: the raw HSV
        // sweep's local luma gradient is low almost everywhere (hue rotation
        // at fixed value barely moves luma), so `masking`'s
        // `smoothstep(t0, t1, |∇luma|)` edge term was ~0 across the whole
        // frame — a real, honest property of that source, not a masking bug,
        // but it made the fixture worthless for pinning the sharpen+masking
        // math. The tone-curve/HSL/grade base gives the source genuine local
        // contrast for masking's edge detector to key off, so this fixture
        // now actually exercises the combined detail+masking formula.
        (
            "sharpen_detail_masking",
            with_curve_hsl_grade(OpStack::default()).set_op(Op::Sharpen(Sharpen {
                amount: 0.9,
                radius: 4,
                detail: 0.6,
                masking: 0.5,
            })),
        ),
    ]
}

// ---------------------------------------------------------------------------
// 16-bit-PNG goldens
// ---------------------------------------------------------------------------

/// Clamp range for the linear-light -> u16 golden encoding. Generous headroom
/// above 1.0 for exposure/sharpen-overshoot/dehaze-recovery pixels that
/// legitimately exceed the [0,1] display range in scene-linear space, AND
/// below 0.0: contrast/white-balance/HSL/grade can legitimately push a
/// scene-linear channel negative (e.g. `light_trio`, `curve_hsl_grade`, and
/// `wb_contrast_both` all render pixels down to ~-0.09) before any downstream
/// tone-curve floor or display clamp — an EARLIER version of this file clamped
/// the low end at 0.0, which silently discarded those negative values on
/// write and then made every later compare run "fail" by exactly that much,
/// even though the underlying render was fully deterministic. Don't repeat
/// that: keep `GOLDEN_MIN` negative enough that a legitimate excursion never
/// clips (checked by `compare_or_write_golden16`'s sanity assert below).
const GOLDEN_MIN: f32 = -1.0;
const GOLDEN_MAX: f32 = 8.0;
const GOLDEN_SPAN: f32 = GOLDEN_MAX - GOLDEN_MIN;

fn encode16(v: f32) -> u16 {
    (((v.clamp(GOLDEN_MIN, GOLDEN_MAX) - GOLDEN_MIN) / GOLDEN_SPAN) * 65535.0).round() as u16
}

fn decode16(v: u16) -> f32 {
    GOLDEN_MIN + (v as f32 / 65535.0) * GOLDEN_SPAN
}

fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/layer_engine")
        .join(name)
}

/// Compare `pixels` (scene-linear f32 RGBA, `w*h*4` long — e.g. from
/// `common::read_image_linear`) against the committed 16-bit-PNG golden
/// `<name>`. Writes (creating parent dirs as needed) instead of comparing when
/// `UPDATE_GOLDENS=1` is set or the golden file doesn't exist yet, returning
/// `0.0` in that case. Otherwise returns the max per-channel abs diff over R/G/B
/// (alpha is always 1 in these fixtures, so it's not compared) for the caller to
/// check against `PARITY_TOL` and report by name on failure.
pub fn compare_or_write_golden16(pixels: &[f32], w: u32, h: u32, name: &str) -> f32 {
    let path = golden_path(name);

    let (min_val, max_val) = pixels
        .iter()
        .copied()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v), hi.max(v))
        });
    let margin = GOLDEN_SPAN * 0.01;
    assert!(
        min_val > GOLDEN_MIN + margin && max_val < GOLDEN_MAX - margin,
        "{name}: pixel range [{min_val}, {max_val}] is within 1% of the golden encoding range \
         [{GOLDEN_MIN}, {GOLDEN_MAX}] — widen it before it silently clips and masks a regression"
    );

    if std::env::var("UPDATE_GOLDENS").is_ok() || !path.exists() {
        std::fs::create_dir_all(path.parent().expect("golden path has a parent"))
            .expect("create tests/golden/layer_engine");
        let mut buf: Vec<u16> = Vec::with_capacity(pixels.len());
        for chunk in pixels.chunks_exact(4) {
            buf.push(encode16(chunk[0]));
            buf.push(encode16(chunk[1]));
            buf.push(encode16(chunk[2]));
            buf.push(encode16(chunk[3]));
        }
        let img: image::ImageBuffer<image::Rgba<u16>, Vec<u16>> =
            image::ImageBuffer::from_raw(w, h, buf).expect("golden16 buffer size matches w*h*4");
        img.save(&path)
            .unwrap_or_else(|e| panic!("failed to write golden {}: {e}", path.display()));
        eprintln!("wrote golden {}", path.display());
        return 0.0;
    }

    let golden = image::open(&path)
        .unwrap_or_else(|e| panic!("failed to read golden {}: {e}", path.display()))
        .to_rgba16();
    assert_eq!(golden.dimensions(), (w, h), "golden dims mismatch: {name}");

    let mut max_diff = 0.0f32;
    for (got, want) in pixels.chunks_exact(4).zip(golden.pixels()) {
        for c in 0..3 {
            let d = (got[c] - decode16(want[c])).abs();
            if d > max_diff {
                max_diff = d;
            }
        }
    }
    max_diff
}

/// Max per-channel (RGB only) absolute difference between two scene-linear f32
/// RGBA buffers of the same length.
pub fn max_abs_diff_f32(a: &[f32], b: &[f32]) -> f32 {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .flat_map(|(pa, pb)| (0..3).map(move |c| (pa[c] - pb[c]).abs()))
        .fold(0.0f32, f32::max)
}
