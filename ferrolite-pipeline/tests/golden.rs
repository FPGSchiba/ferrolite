mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_pipeline::{
    blit_to_rgba8, dehaze_recover, estimate_atmospheric_light, upload_source, Aspect, ColorGrade,
    Contrast, CropRect, CurveMode, Dehaze, EditPipeline, Exposure, Geometry, GpuPyramidSource,
    GradeWheel, Hsl, HslBand, Op, OpStack, ParametricCurve, PointCurve, Sharpen, TileEditPipeline,
    ToneCurve, WhiteBalance, DEHAZE_DEFAULT_RADIUS,
};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 48;

/// Identity camera->working matrix — these goldens predate Spec 3's color
/// pipeline and assert on the existing (pre-color-matrix) op chain, so the
/// color matrix must be a no-op here.
const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

#[test]
fn source_upload_blit_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden (expected in headless CI)");
        return;
    };
    let src = common::gradient(W, H);
    let img = upload_source(&ctx, &src);
    let pixels = blit_to_rgba8(&ctx, &img);
    common::assert_golden(&pixels, W, H, "source.png");
}

#[test]
fn exposure_plus_one_ev_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::Exposure(Exposure { ev: 1.0 }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "exposure_plus1.png");
}

#[test]
fn white_balance_warm_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::WhiteBalance(WhiteBalance {
        temp: 0.5,
        tint: -0.2,
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "wb_warm.png");
}

#[test]
fn identity_stack_matches_source_render() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let src = common::gradient(W, H);
    // Source rendered directly through the blit.
    let source_render = blit_to_rgba8(&ctx, &upload_source(&ctx, &src));
    // Empty stack through the full pipeline must match within tolerance.
    let mut pipe = EditPipeline::new(ctx.clone(), &src, OpStack::default(), IDENTITY);
    let edited = pipe.render_to_image();
    let diff = common::max_abs_diff(&source_render, &edited);
    assert!(
        diff <= 4,
        "identity stack diverged from source (diff {diff})"
    );
}

#[test]
fn contrast_boost_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::Contrast(Contrast { amount: 0.5 }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "contrast_boost.png");
}

#[test]
fn dehaze_positive_increases_contrast_on_hazy_image() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // A low-contrast "hazy" gradient: values compressed toward a bright floor.
    let (w, h) = (64u32, 64u32);
    let mut px = Vec::with_capacity((w * h) as usize * 4);
    for y in 0..h {
        for _x in 0..w {
            let v = 0.6 + 0.25 * (y as f32 / h as f32); // 0.60..0.85, low spread
            px.extend_from_slice(&[v, v, v, 1.0]);
        }
    }
    let src = LinearRgbaF32::new(w, h, px).unwrap();

    let base = OpStack::default();
    let dehazed = base.set_op(Op::Dehaze(Dehaze {
        amount: 1.0,
        radius: DEHAZE_DEFAULT_RADIUS,
    }));

    let mut p0 = EditPipeline::new(ctx.clone(), &src, base, IDENTITY);
    let mut p1 = EditPipeline::new(ctx.clone(), &src, dehazed, IDENTITY);
    let a = p0.render_to_image();
    let b = p1.render_to_image();

    // Range (max - min) over the red channel: dehaze must widen it (more contrast).
    let range = |buf: &[u8]| {
        let (mut lo, mut hi) = (255u8, 0u8);
        for px in buf.chunks_exact(4) {
            lo = lo.min(px[0]);
            hi = hi.max(px[0]);
        }
        hi as i32 - lo as i32
    };
    assert!(
        range(&b) > range(&a),
        "positive dehaze widens tonal range: before={} after={}",
        range(&a),
        range(&b)
    );
}

/// QS-Task 4: the two-node (transmission + recovery, guided-filter-refined)
/// dehaze must show substantially LESS of the classic Dark-Channel-Prior
/// "bright halo ring" around a dark object in a hazy field than the OLD
/// single-pass (block-min-only, no guided filter) implementation would.
///
/// Fixture: a thin, pure-white "sky" band seeds the atmospheric-light estimate
/// `A`, well above the main (haze-washed) bright field, so the field is
/// genuinely haze-affected; a single vertical dark/bright edge sits well clear
/// of both image borders (mirrors the isolated-edge geometry of the CPU-level
/// `guided_refinement_removes_most_of_the_block_min_halo` test — a dark BAR
/// narrower than `2 * guided_radius(radius)` would let one edge's
/// guided-filter window see the OTHER edge too, contaminating the very
/// correlation the filter uses to track the edge; a single isolated edge with
/// wide margins avoids that confound).
///
/// The block-min dark-channel prior dilates the dark side's near-zero
/// reflectance ratio `radius` px into the bright field, driving the UNREFINED
/// transmission estimate there toward "no haze" (t≈1) — which, recovered,
/// leaves those pixels much brighter than the correctly dehazed far field:
/// the halo. NOTE on the threshold: Task 1's CPU-level unit test established
/// "guided filter removes >=60% of the halo" in TRANSMISSION space; that bound
/// does not transfer directly to recovered-BRIGHTNESS space asserted here,
/// because recovery divides by transmission (`(I-A)/t + A`) — a reciprocal
/// that amplifies whatever residual transmission error remains, more so the
/// closer the true far-field transmission is to the floor `t0`. Verified
/// empirically (via `transmission_map` on this exact fixture) across several
/// atmospheric-light/field combinations, the recovered-brightness halo
/// removal at `edge+radius` consistently exceeds 40%; the assertion below
/// uses a materially looser (30%) bound for margin against GPU f16 rounding.
#[test]
fn dehaze_no_halo_on_dark_edge() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);

    let (w, h) = (200usize, 32usize);
    let sky_rows = 4usize; // y in [0, sky_rows): seeds A well above the main field.
    let edge = 100usize; // x < edge: dark; x >= edge: bright (haze-washed) field.
    let (sky, field, dark) = (1.0f32, 0.4f32, 0.05f32);

    let mut planar = vec![[0.0f32; 3]; w * h];
    let mut interleaved = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let v = if y < sky_rows {
                sky
            } else if x < edge {
                dark
            } else {
                field
            };
            planar[y * w + x] = [v, v, v];
            interleaved.extend_from_slice(&[v, v, v, 1.0]);
        }
    }
    let src = LinearRgbaF32::new(w as u32, h as u32, interleaved).expect("halo fixture length");

    let radius = 8u32;
    let stack = OpStack::default().set_op(Op::Dehaze(Dehaze {
        amount: 1.0,
        radius,
    }));
    let mut pipe = EditPipeline::new(ctx.clone(), &src, stack, IDENTITY);
    let out = pipe.evaluate();
    let rendered = common::read_image_linear(&ctx, &out);
    let px_at = |x: usize, y: usize| -> f32 { rendered[(y * w + x) * 4] }; // R channel

    let row = h / 2; // deep inside the main-field rows, far from the sky band.
                     // Deep in the bright field, far from the edge (and far beyond
                     // `2 * guided_radius(radius)` = 48px, so the guided filter's box windows
                     // there carry no trace of the dark side).
    let far_field = px_at(w - 10, row);
    // Per-spec band: bright pixels within [radius/2 .. radius] px of the dark
    // edge, back into the bright field — sampled at the band's near end
    // (`edge + radius/2`), which is guaranteed to fall inside the block-min
    // window's dilation zone (a window of radius `radius` centered there
    // reaches `radius/2` px past the edge into the dark side).
    let near_edge = px_at(edge + (radius / 2) as usize, row);

    // Sanity baseline: the KNOWN-BAD unrefined (single-pass, no guided filter)
    // recovery at any point within `radius` px of the edge is driven by the
    // dark bar's OWN reflectance ratio — a block-min window overlapping ANY
    // dark pixel returns exactly that ratio (the min over a uniform dark
    // region). This is the position-independent worst-case unrefined halo
    // (public math only — the same per-pixel ratio and `dehaze_recover`
    // transform the GPU path shares); confirm it shows a REAL halo relative
    // to the far field, or this fixture tests nothing.
    let a = estimate_atmospheric_light(&src);
    let naive_dark = (dark / a[0]).min(dark / a[1]).min(dark / a[2]);
    let naive_halo = dehaze_recover([field, field, field], naive_dark, a, 1.0)[0];
    let naive_overshoot = (naive_halo - far_field).abs();
    assert!(
        naive_overshoot > 0.1,
        "fixture must have a real unrefined halo to remove \
         (naive={naive_halo}, far_field={far_field}, overshoot={naive_overshoot})"
    );

    // The refined (guided-filter) GPU output near the edge must stay
    // materially closer to the far field than the unrefined baseline would
    // (see the doc comment above for why the bound is 30%, not Task 1's 60%).
    let refined_overshoot = (near_edge - far_field).abs();
    eprintln!(
        "dehaze_no_halo_on_dark_edge: naive_overshoot={naive_overshoot} \
         refined_overshoot={refined_overshoot} removed_frac={}",
        1.0 - refined_overshoot / naive_overshoot
    );
    assert!(
        refined_overshoot < naive_overshoot * 0.7,
        "guided-filter dehaze must remove a substantial fraction of the halo near \
         the dark edge (near_edge={near_edge}, far_field={far_field}, \
         unrefined/known-bad baseline={naive_halo}, \
         refined_overshoot={refined_overshoot} vs unrefined_overshoot={naive_overshoot})"
    );
}

#[test]
fn full_stack_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default()
        .set_op(Op::Exposure(Exposure { ev: 0.5 }))
        .set_op(Op::WhiteBalance(WhiteBalance {
            temp: 0.3,
            tint: 0.0,
        }))
        .set_op(Op::Contrast(Contrast { amount: 0.4 }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "full_stack.png");
}

#[test]
fn editing_one_op_reevaluates_minimally() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let base = OpStack::default().set_op(Op::Exposure(Exposure { ev: 0.2 }));
    let mut pipe = EditPipeline::new(
        Arc::new(ctx),
        &common::gradient(W, H),
        base.clone(),
        IDENTITY,
    );

    // First evaluate runs every node exactly once (source + one per op).
    let _ = pipe.evaluate();
    assert_eq!(pipe.eval_count(), pipe.node_count());

    // Re-evaluating with no change re-runs nothing (all cached).
    let after_first = pipe.eval_count();
    pipe.set_stack(base.clone());
    let _ = pipe.evaluate();
    assert_eq!(
        after_first,
        pipe.eval_count(),
        "no node re-ran when nothing changed"
    );

    // Dirtying exposure re-runs it + every downstream op; the three nodes ahead
    // of exposure in the chain — source, the camera→working color-matrix, and the
    // scene-linear vignette pass — stay cached -> exactly node_count - 3
    // re-evaluations.
    let prev = pipe.eval_count();
    pipe.set_stack(OpStack::default().set_op(Op::Exposure(Exposure { ev: 1.5 })));
    let _ = pipe.evaluate();
    assert_eq!(
        pipe.eval_count(),
        prev + (pipe.node_count() - 3),
        "exposure + downstream re-evaluated; source, color-matrix, and vignette stay cached"
    );
}

#[test]
fn tone_curve_darken_midtones_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::ToneCurve(ToneCurve {
        points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
        mode: CurveMode::Linear,
        ..Default::default()
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "tone_curve.png");
}

#[test]
fn tone_curve_smooth_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::ToneCurve(ToneCurve {
        points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
        mode: CurveMode::Smooth,
        ..Default::default()
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "tone_curve_smooth.png");
}

#[test]
fn tone_curve_per_channel_and_parametric_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Master smooth + a red channel curve + a parametric shadows lift.
    let stack = OpStack::default().set_op(Op::ToneCurve(ToneCurve {
        points: vec![(0.0, 0.0), (0.5, 0.55), (1.0, 1.0)],
        mode: CurveMode::Smooth,
        red: PointCurve {
            points: vec![(0.0, 0.0), (0.5, 0.35), (1.0, 1.0)],
            mode: CurveMode::Linear,
        },
        green: PointCurve::default(),
        blue: PointCurve {
            points: vec![(0.0, 0.05), (1.0, 0.95)],
            mode: CurveMode::Linear,
        },
        parametric: ParametricCurve {
            shadows: 0.4,
            highlights: -0.2,
            ..Default::default()
        },
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "tone_curve_p3.png");
}

#[test]
fn sharpen_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::Sharpen(Sharpen {
        amount: 0.8,
        radius: 2,
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "sharpen.png");
}

#[test]
fn hsl_shift_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Boost saturation + nudge hue across all bands.
    let stack = OpStack::default().set_op(Op::Hsl(Hsl {
        bands: [HslBand {
            hue: 0.2,
            sat: 0.4,
            lum: 0.0,
        }; 8],
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "hsl.png");
}

#[test]
fn color_grade_three_way_plus_global_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::ColorGrade(ColorGrade {
        shadows: GradeWheel {
            hue: 220.0,
            sat: 0.6,
            lum: -0.1,
        }, // cool shadows
        midtones: GradeWheel {
            hue: 120.0,
            sat: 0.2,
            lum: 0.0,
        }, // slight green mids
        highlights: GradeWheel {
            hue: 40.0,
            sat: 0.5,
            lum: 0.1,
        }, // warm highlights
        global: GradeWheel {
            hue: 300.0,
            sat: 0.15,
            lum: 0.0,
        }, // faint magenta cast
        blending: 0.6,
        balance: -0.1,
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "color_grade.png");
}

#[test]
fn geometry_crop_rotate_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::Geometry(Geometry {
        crop: CropRect {
            x: 0.1,
            y: 0.1,
            w: 0.8,
            h: 0.8,
        },
        angle_deg: 10.0,
        aspect: Aspect::Free,
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    // out dims = round(0.8 * 64) x round(0.8 * 48) = 51 x 38.
    common::assert_golden(&pixels, 51, 38, "geometry_crop_rotate.png");
}

#[test]
fn full_seven_op_stack_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default()
        .set_op(Op::Exposure(Exposure { ev: 0.3 }))
        .set_op(Op::WhiteBalance(WhiteBalance {
            temp: 0.2,
            tint: 0.0,
        }))
        .set_op(Op::Contrast(Contrast { amount: 0.3 }))
        .set_op(Op::ToneCurve(ToneCurve {
            points: vec![(0.0, 0.0), (0.5, 0.4), (1.0, 1.0)],
            mode: CurveMode::Linear,
            ..Default::default()
        }))
        .set_op(Op::Hsl(Hsl {
            bands: [HslBand {
                hue: 0.0,
                sat: 0.2,
                lum: 0.0,
            }; 8],
        }))
        .set_op(Op::Sharpen(Sharpen {
            amount: 0.5,
            radius: 1,
        }))
        .set_op(Op::Geometry(Geometry {
            crop: CropRect {
                x: 0.05,
                y: 0.05,
                w: 0.9,
                h: 0.9,
            },
            angle_deg: 3.0,
            aspect: Aspect::Free,
        }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    // out dims = round(0.9*64) x round(0.9*48) = 58 x 43.
    common::assert_golden(&pixels, 58, 43, "full_seven_op_stack.png");
}

const SEAM_TOL: f32 = 0.02; // display-linear; absorbs f16 + the head resample.

#[test]
fn sharpen_tiles_match_whole_image_at_seam() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // A multi-tile image: 300x200 -> 2x1 tiles at LOD 0 (seam at x = 256).
    let (iw, ih) = (300u32, 200u32);
    let src = common::gradient(iw, ih);
    let stack = OpStack::default().set_op(Op::Sharpen(Sharpen {
        amount: 0.8,
        radius: 3,
    }));

    // Whole-image reference: render the edited image to display-linear f32 by
    // evaluating the EditPipeline and reading its output back.
    let mut whole = EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    let whole_lin = common::read_image_linear(&ctx, &whole.evaluate());

    // Per-tile producer over the GPU-resident source pyramid.
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    let mut tep = TileEditPipeline::new(ctx.clone(), pyramid, stack, IDENTITY, None, None);

    // Produce both tiles, read interiors, and compare the valid region against
    // the whole-image reference — focusing on the seam column.
    use ferrolite_image::{TileCoord, TILE_SIZE};
    let mut max_diff = 0.0f32;
    for tx in 0..2u32 {
        let tile = tep.produce_tile(TileCoord {
            lod: 0,
            x: tx,
            y: 0,
        });
        let tile_lin = common::read_tile_linear(&ctx, &tile);
        for ly in 0..TILE_SIZE {
            for lx in 0..TILE_SIZE {
                let gx = tx * TILE_SIZE + lx;
                let gy = ly;
                if gx >= iw || gy >= ih {
                    continue; // out-of-image tile padding
                }
                let ti = ((ly * TILE_SIZE + lx) * 4) as usize;
                let wi = ((gy * iw + gx) * 4) as usize;
                for c in 0..3 {
                    max_diff = max_diff.max((tile_lin[ti + c] - whole_lin[wi + c]).abs());
                }
            }
        }
    }
    eprintln!("tile-seam max linear diff = {max_diff}");
    assert!(
        max_diff <= SEAM_TOL,
        "per-tile sharpen diverged from whole-image (diff {max_diff}) — halo broken?"
    );
}

// QS-Task 4 gave `EditPipeline` (whole-image) the guided-filter-refined
// transmission+recovery dehaze; QS-Task 5 migrated `TileEditPipeline` (tiled)
// to the SAME two nodes (`DehazeTransmissionNode` + `DehazeRecoveryNode`) and
// enlarged the halo to `dehaze_halo = r + 2*guided_radius(r) = 7r` so the
// guided filter's full neighbourhood (not just the block-min patch) is
// available across the tile seam. Both tiers now run the identical algorithm
// with the identical `A`, so this asserts genuine tiled/whole-image parity
// across an INTERNAL tile seam (previously ignored because the two tiers ran
// different algorithms).
//
// MARGIN NOTE (found while enabling this test): the min/box filter passes
// clamp by INDEXING an already-filtered intermediate array at the buffer's own
// edge (mirrors the pure CPU reference in `dehaze.rs`, `min_filter_separable`/
// `box_blur_separable`'s `idx` clamp) rather than re-deriving a filtered value
// from an edge-extended RAW signal. For a genuinely separable, multi-stage
// filter (block-min -> guided coefficients -> box-filter-again) those two
// boundary conventions are NOT numerically equivalent near a TRUE image edge —
// "clamp the index into an already-filtered array" (what a single, self-
// contained whole-image buffer does) differs from "the tile's real neighbour
// data, extended past a true canvas edge by the geometry head, then re-filtered
// fresh" (what a haloed tile does). This is an inherent property of separable
// edge-clamped filtering, orthogonal to tiling correctness: verified with a
// direct probe (`DehazeTransmissionNode` fed a manually clamp-extended buffer
// vs. an exact-width buffer) that the divergence exists even at `radius=1` and
// is independent of buffer width/alignment — i.e. it is NOT a tile-seam defect
// and not introduced by this task's halo enlargement. An INTERNAL tile seam
// (checked here) never hits it: both neighbours have REAL data, so no clamp of
// any kind is needed. This test therefore checks a band around the x=256 seam
// while staying `>= dehaze_halo` away from the canvas's true left/right/top/
// bottom edges (the filters' dependency never reaches further than that), so
// it asserts exactly what QS-Task 5 owns (seam parity) without being
// confounded by that separate, pre-existing true-edge property (which would
// require changing the filters' boundary convention — out of this task's
// scope — to eliminate).
#[test]
fn dehaze_tiled_matches_whole_image() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // A multi-tile image: 480x256 -> 2x1 tiles at LOD 0 (seam at x = 256).
    // Sized so the checked region (below) has a margin (>= `dehaze_halo`) from
    // every true canvas edge (see the MARGIN NOTE above) while the seam itself
    // sits comfortably in the middle of that checked region.
    //
    // NOTE: this is a LOCAL fixture, not `common::gradient` — the shared gradient
    // (R = x/w, G = y/h, B = 0.25) is insensitive to this test's purpose. The
    // dehaze dark channel is `min(R,G,B)`; with the shared gradient, B (a flat
    // 0.25) or G (which varies only with y) dominates that min near the x=256
    // seam, so the dark channel — and therefore the halo min-filter's cross-tile
    // fetch — never varies across x there. That let this test pass (max diff 0)
    // whether or not the dehaze halo fold-in (`tile_edit.rs`'s
    // `.max(dehaze_halo(...))`) was even present, i.e. it asserted nothing about
    // the feature it's meant to guard. This fixture instead puts a high-frequency
    // sawtooth ripple in R across x (period 16px) and pins G/B to a high constant
    // so R stays the min everywhere — the dark channel then varies sharply right
    // at the seam, so the patch min-filter genuinely needs cross-tile neighbours.
    let (iw, ih) = (480u32, 256u32);
    let src = {
        let mut px = Vec::with_capacity((iw * ih * 4) as usize);
        for _y in 0..ih {
            for x in 0..iw {
                let r = (x % 16) as f32 / 16.0 * 0.8; // sawtooth ripple in [0, 0.8)
                px.extend_from_slice(&[r, 0.9, 0.9, 1.0]);
            }
        }
        LinearRgbaF32::new(iw, ih, px).expect("dehaze seam fixture length")
    };
    // radius 12 (rather than the default 8) so the enlarged `r + 2*guided_radius(r)
    // = 7r = 84`px halo is exercised across the x=256 seam. `amount: 2.0` (an
    // aggressive push, past the [-1,1] the UI slider exposes today but not
    // rejected by the op itself) is chosen for the MANDATORY sensitivity check
    // below: at `amount: 0.8` the seam is close enough even WITHOUT the halo
    // fold-in (this fixture's periodic ripple partially self-heals at the tile's
    // own clamped edge) that removing the fold-in stayed under `SEAM_TOL` — a
    // false-negative sensitivity guard. `amount: 2.0` reliably pushes the
    // without-fold-in seam error over `SEAM_TOL` (see the fold-in sensitivity
    // numbers recorded in the QS-Task 5 report) while the WITH-fold-in case
    // stays exact (max diff 0), so this value is load-bearing for the guard,
    // not arbitrary.
    let radius = 12u32;
    let amount = 2.0f32;
    let stack = OpStack::default().set_op(Op::Dehaze(Dehaze { amount, radius }));
    // Estimated ONCE from the CPU source and handed to both tiers: `EditPipeline`
    // estimates `A` internally from the same `src`, so the two agree exactly.
    let atmos = estimate_atmospheric_light(&src);

    // Whole-image reference: render the edited image to display-linear f32 by
    // evaluating the EditPipeline and reading its output back.
    let mut whole = EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    let whole_lin = common::read_image_linear(&ctx, &whole.evaluate());

    // Per-tile producer over the GPU-resident source pyramid. `TileEditPipeline`
    // has no CPU source to estimate `A` from — it starts NEUTRAL and the caller
    // hands it the same estimate via `set_dehaze_atmos`.
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    let mut tep = TileEditPipeline::new(ctx.clone(), pyramid, stack, IDENTITY, None, None);
    tep.set_dehaze_atmos(atmos);

    // Produce both tiles, read interiors, and compare against the whole-image
    // reference within a margin of the true canvas edges (see MARGIN NOTE). The
    // filters' dependency never reaches beyond `halo` (=84) px from where they're
    // evaluated, so `halo` is already an exact safety bound; add a small buffer
    // for rounding. `ih=256` only leaves room for a modest margin (256/2=128 max),
    // so this is NOT doubled the way `iw`'s margin comfortably could be.
    use ferrolite_image::{TileCoord, TILE_SIZE};
    let margin = ferrolite_pipeline::dehaze_halo(Some(Dehaze { amount, radius })) + 10;
    let mut max_diff = 0.0f32;
    let mut max_loc = (0u32, 0u32, 0usize);
    for tx in 0..2u32 {
        let tile = tep.produce_tile(TileCoord {
            lod: 0,
            x: tx,
            y: 0,
        });
        let tile_lin = common::read_tile_linear(&ctx, &tile);
        for ly in 0..TILE_SIZE {
            for lx in 0..TILE_SIZE {
                let gx = tx * TILE_SIZE + lx;
                let gy = ly;
                if gx >= iw || gy >= ih {
                    continue; // out-of-image tile padding
                }
                if gx < margin || gx >= iw - margin || gy < margin || gy >= ih - margin {
                    continue; // too close to a TRUE canvas edge (see MARGIN NOTE)
                }
                let ti = ((ly * TILE_SIZE + lx) * 4) as usize;
                let wi = ((gy * iw + gx) * 4) as usize;
                for c in 0..3 {
                    let d = (tile_lin[ti + c] - whole_lin[wi + c]).abs();
                    if d > max_diff {
                        max_diff = d;
                        max_loc = (gx, gy, c);
                    }
                }
            }
        }
    }
    eprintln!("dehaze tile-seam max linear diff = {max_diff} at {max_loc:?}");
    assert!(
        max_diff <= SEAM_TOL,
        "per-tile dehaze diverged from whole-image (diff {max_diff}) — halo broken?"
    );
}
