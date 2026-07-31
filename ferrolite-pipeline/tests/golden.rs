mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_pipeline::{
    blit_to_rgba8, clamp_uv_to_crop_bounds, dehaze_recover, estimate_atmospheric_light,
    geometry_src_px, geometry_uniform, transmission_mip_level_count, transmission_working_dims,
    upload_source, AdjustmentSet, Aspect, ColorGrade, Contrast, CropRect, CurveMode, Dehaze,
    EditPipeline, Exposure, Geometry, GpuPyramidSource, GradeWheel, Hsl, HslBand, LocalAdjustments,
    MaskLayer, Op, OpStack, ParametricCurve, PointCurve, Sharpen, TileEditPipeline, ToneCurve,
    WhiteBalance, DEHAZE_DEFAULT_RADIUS,
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
        ..Default::default()
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    // out dims = round(0.8 * 64) x round(0.8 * 48) = 51 x 38.
    common::assert_golden(&pixels, 51, 38, "geometry_crop_rotate.png");
}

/// The keystone parity/golden fixture (plan `crop-overhaul` C4 Task 5):
/// kv = 0.5, kh = -0.3 on top of a real crop + rotation.
fn keystone_fixture_stack() -> OpStack {
    OpStack::default().set_op(Op::Geometry(Geometry {
        crop: CropRect {
            x: 0.1,
            y: 0.1,
            w: 0.8,
            h: 0.8,
        },
        angle_deg: 10.0,
        aspect: Aspect::Free,
        keystone_v: 0.5,
        keystone_h: -0.3,
    }))
}

#[test]
fn geometry_keystone_matches_golden() {
    // Plan `crop-overhaul` C4 Task 5: the committed keystone golden
    // (`geometry_keystone.png`, authored with UPDATE_GOLDEN=1 on the dev GPU)
    // pins the projective geometry pass — kv/kh + crop + rotation.
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let mut pipe = EditPipeline::new(
        Arc::new(ctx),
        &common::gradient(W, H),
        keystone_fixture_stack(),
        IDENTITY,
    );
    let pixels = pipe.render_to_image();
    // Keystone does not change the output extent: out dims stay the crop's,
    // round(0.8 * 64) x round(0.8 * 48) = 51 x 38.
    common::assert_golden(&pixels, 51, 38, "geometry_keystone.png");
}

#[test]
fn geometry_keystone_gpu_matches_cpu_homography_reference() {
    // Brief test (c): CPU/GPU parity on kv=0.5, kh=-0.3 + crop + rotation.
    // The CPU side predicts every output pixel analytically through the SAME
    // uniform the GPU consumes: `geometry_src_px` (the homography mirror) →
    // normalize → `clamp_uv_to_crop_bounds` → evaluate `common::gradient`'s
    // exact linear formula at the sampled coordinate (bilinear sampling of a
    // linear ramp is the ramp itself, so texel value at uv is `uv − half
    // texel`) → the blit's sRGB OETF.
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = keystone_fixture_stack();
    let mut pipe = EditPipeline::new(
        Arc::new(ctx),
        &common::gradient(W, H),
        stack.clone(),
        IDENTITY,
    );
    let pixels = pipe.render_to_image();

    let (u, out_w, out_h) = geometry_uniform(stack.geometry(), W, H);
    assert_eq!((out_w, out_h), (51, 38));
    let mut expected = Vec::with_capacity((out_w * out_h * 4) as usize);
    for y in 0..out_h {
        for x in 0..out_w {
            let po = [x as f32 + 0.5, y as f32 + 0.5];
            let s = geometry_src_px(&u, po);
            let uv = clamp_uv_to_crop_bounds([s[0] / W as f32, s[1] / H as f32], u.crop_bounds);
            let r = uv[0] - 0.5 / W as f32;
            let g = uv[1] - 0.5 / H as f32;
            expected.extend_from_slice(&[srgb_u8(r), srgb_u8(g), srgb_u8(0.25), 255]);
        }
    }
    let diff = common::max_abs_diff(&pixels, &expected);
    assert!(
        diff <= 4,
        "keystone GPU render diverged from the CPU homography reference (diff {diff})"
    );
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
            ..Default::default()
        }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    // out dims = round(0.9*64) x round(0.9*48) = 58 x 43.
    common::assert_golden(&pixels, 58, 43, "full_seven_op_stack.png");
}

/// The `sRGB` OETF `blit_to_rgba8` applies to the display-linear pipeline
/// output (mirrors `blit.wgsl`'s `linear_to_srgb` exactly), so a corner's
/// expected `Rgba8` value can be predicted directly from its (clamped)
/// source-normalized coordinate against `common::gradient`'s exact
/// `[x/w, y/h, 0.25, 1.0]` formula.
fn srgb_u8(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).round() as u8
}

#[test]
fn rotated_crop_edge_is_not_smeared() {
    // Spec C2 part 2: a rotated crop's out-of-bounds corner used to clamp
    // against the WHOLE source texture, so it read (and duplicated) the
    // FRAME's edge texel -- e.g. a crop with real margin on every side would
    // still show the image's absolute corner color, unrelated to the crop's
    // own local content. This asserts the fixed geometry pass instead clamps
    // to the CROP's own edge.
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // A gradient fixture, cropped to a 0.4x0.4 region with a real 5% margin
    // to the frame on every side, rotated 45 degrees. The output's bottom-left
    // corner (0, out_h-1) maps to a raw source coordinate whose `u` (x/w) is
    // NEGATIVE -- i.e. past the whole FRAME's left edge, not merely past the
    // crop's -- so this exercises exactly the old clamp-to-frame bug.
    let src = common::gradient(200, 200);
    let stack = OpStack::default().set_op(Op::Geometry(Geometry {
        crop: CropRect {
            x: 0.05,
            y: 0.05,
            w: 0.4,
            h: 0.4,
        },
        angle_deg: 45.0,
        aspect: Aspect::Free,
        ..Default::default()
    }));
    let mut pipe = EditPipeline::new(ctx, &src, stack, IDENTITY);
    let pixels = pipe.render_to_image();
    let (out_w, out_h) = (80u32, 80u32);
    let px = |x: u32, y: u32| -> &[u8] {
        let i = ((y * out_w + x) * 4) as usize;
        &pixels[i..i + 4]
    };
    let corner = px(0, out_h - 1);

    // The bug's signature: clamped to the FRAME's absolute edge (u == 0.0,
    // the image's true left column) rather than the crop's own edge.
    let frame_edge_r = srgb_u8(0.0);
    // The fix: clamped to the crop's own left edge (u == crop.x, inset half a
    // source texel), which for this fixture (u == x/w) is a clearly
    // different, non-zero color -- NOT a duplicate of the frame's edge.
    let crop_edge_r = srgb_u8(0.05 + 0.5 / 200.0);

    assert!(
        corner[1].abs_diff(srgb_u8(0.25)) <= 4,
        "corner G {} unexpected (v should be un-clamped, ~{})",
        corner[1],
        srgb_u8(0.25)
    );
    assert!(
        corner[0].abs_diff(crop_edge_r) <= 4,
        "corner R {} did not land on the crop's own edge (expected ~{crop_edge_r}) \
         -- clamp_uv_to_crop_bounds should pin `u` to `crop_bounds[0]`",
        corner[0]
    );
    assert!(
        corner[0].abs_diff(frame_edge_r) > 8,
        "corner R {} matches the FRAME's edge color (~{frame_edge_r}) -- this is \
         the smear artifact this task fixes: the sample clamped to the whole \
         source texture instead of the crop's own sub-rect",
        corner[0]
    );

    // Full-row/column sanity: the crop's own top row (unaffected by the
    // corner clamp) must still vary smoothly -- i.e. this fix does not
    // introduce a NEW flat/duplicate band elsewhere in the output.
    let top_row = |x: u32| px(x, 0);
    assert_ne!(
        top_row(out_w / 2),
        top_row(out_w / 2 - 1),
        "interior top-row neighbors are byte-identical -- unexpected duplicate"
    );
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

// ST-Task 3 (shared-transmission migration): `TileEditPipeline` no longer
// computes its own per-tile guided-filter transmission (the old QS-Task 5
// per-tile `DehazeTransmissionNode`, halo `7r`, is GONE — see `dehaze_halo`,
// now always 0). Instead its `DehazeRecoveryNode` SAMPLES the shared
// whole-image transmission that `EditPipeline` computes once (source space),
// handed over via `TileEditPipeline::set_shared_transmission`. Under identity
// geometry the recovery's source-UV mapping reduces to `uv = pixel / dims`,
// exactly the whole-image tier's own UV into the SAME shared texture, so this
// is a genuine per-pixel parity check: no halo (dehaze contributes none) and
// no true-edge margin (the old margin existed because the tiled tier used to
// compute its OWN transmission per tile — a SEPARATE computation from the
// whole-image one, which disagreed near true canvas edges due to a filter
// boundary-convention difference, see the removed MARGIN NOTE in prior
// history. With only ONE transmission computation now, shared by both tiers,
// that discrepancy cannot occur — this test checks the FULL image, not just
// an internal seam band).
//
// MANDATORY sensitivity check (recorded in the ST-Task 3 report): temporarily
// calling `tep.set_shared_transmission(None)` (transmission-missing) — or
// zeroing the recovery's bound geometry — must make this test's assertion
// FAIL; otherwise the parity check below would be vacuous.
#[test]
fn dehaze_tiled_matches_whole_image() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // A multi-tile image: 480x256 -> 2x1 tiles at LOD 0 (seam at x = 256).
    //
    // NOTE: this is a LOCAL fixture, not `common::gradient` — the shared gradient
    // (R = x/w, G = y/h, B = 0.25) is insensitive to this test's purpose. The
    // dehaze dark channel is `min(R,G,B)`; with the shared gradient, B (a flat
    // 0.25) or G (which varies only with y) dominates that min near the x=256
    // seam, so the dark channel never varies across x there — a change to the
    // shared-transmission wiring could go undetected. This fixture instead puts
    // a high-frequency sawtooth ripple in R across x (period 16px) and pins G/B
    // to a high constant so R stays the min everywhere — the dark channel (and
    // therefore the transmission) then varies sharply right at the seam, so a
    // wrong source-UV mapping (misaligned by even a fraction of a tile) shows
    // up clearly.
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
    // radius 12 (arbitrary now — it no longer sizes a halo, see `dehaze_halo`).
    // `amount: 2.0` (an aggressive push, past the [-1,1] the UI slider exposes
    // today but not rejected by the op itself) keeps the recovery's dependence
    // on `t` strong, so a wrong/missing transmission sample is NOT masked by a
    // small `amount` — load-bearing for the MANDATORY sensitivity check below.
    let radius = 12u32;
    let amount = 2.0f32;
    let stack = OpStack::default().set_op(Op::Dehaze(Dehaze { amount, radius }));
    // Estimated ONCE from the CPU source and handed to both tiers: `EditPipeline`
    // estimates `A` internally from the same `src`, so the two agree exactly.
    let atmos = estimate_atmospheric_light(&src);

    // Whole-image reference: evaluating computes (and caches) the shared
    // transmission; render to display-linear f32 and read the transmission
    // texture back AFTER evaluate so it reflects what produced `whole_lin`.
    let mut whole = EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    let whole_lin = common::read_image_linear(&ctx, &whole.evaluate());
    let shared_transmission = whole.transmission_texture();
    assert!(
        shared_transmission.is_some(),
        "dehaze is active (amount != 0) -> a shared transmission texture must exist"
    );

    // Per-tile producer over the GPU-resident source pyramid. `TileEditPipeline`
    // has no CPU source to estimate `A` from — it starts NEUTRAL and the caller
    // hands it the same estimate via `set_dehaze_atmos`. It ALSO has no
    // transmission of its own anymore (ST-Task 3) — the whole-image tier's
    // texture computed above is handed over via `set_shared_transmission`.
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    let mut tep = TileEditPipeline::new(ctx.clone(), pyramid, stack, IDENTITY, None, None);
    tep.set_dehaze_atmos(atmos);
    tep.set_shared_transmission(shared_transmission);

    // Produce both tiles, read interiors, and compare against the whole-image
    // reference over the FULL image (no true-edge margin needed — see the
    // module doc above this test: both tiers now sample the SAME transmission
    // texture, so there is no separate per-tile computation to disagree near a
    // true edge).
    use ferrolite_image::{TileCoord, TILE_SIZE};
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
        "per-tile dehaze diverged from whole-image (diff {max_diff}) at {max_loc:?} — shared transmission wiring broken?"
    );
}

// Phase 4 Task 3: the SAME tile-vs-whole-image parity as
// `dehaze_tiled_matches_whole_image` above, but for a MASK-LAYER dehaze
// amount with NO global `Dehaze` op (so `stack.dehaze()` is `None` — only
// `EditDoc::dehaze_active_anywhere()` is true, via the layer). Proves the
// full chain end-to-end at the tiled tier: the transmission is computed
// (Task 3's pipeline.rs wiring fix), handed to the tile producer via the
// SAME `set_shared_transmission` call the global case uses, and the
// per-mask-layer dispatch (Task 3's `local_node.rs`/`local_adjust.wgsl`
// change) actually consumes it identically to the whole-image tier. Uses a
// FULL mask (`MaskDefinition::default()`, coverage 1.0 everywhere) so the
// comparison isolates the shared-transmission wiring, not partial-mask
// blending (covered separately by `local_node.rs`'s
// `mask_layer_dehaze_amount_changes_only_masked_pixels`).
#[test]
fn mask_only_dehaze_tiled_matches_whole_image() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // Same seam-sensitive sawtooth-ripple fixture as `dehaze_tiled_matches_whole_image`
    // (see that test's doc for why: a flat/simple gradient's dark channel is
    // insensitive to a broken shared-transmission wiring).
    let (iw, ih) = (480u32, 256u32);
    let src = {
        let mut px = Vec::with_capacity((iw * ih * 4) as usize);
        for _y in 0..ih {
            for x in 0..iw {
                let r = (x % 16) as f32 / 16.0 * 0.8;
                px.extend_from_slice(&[r, 0.9, 0.9, 1.0]);
            }
        }
        LinearRgbaF32::new(iw, ih, px).expect("dehaze seam fixture length")
    };
    let radius = 12u32;
    let amount = 2.0f32; // aggressive, mirrors the global test's sensitivity rationale
    let la = LocalAdjustments {
        layers: vec![MaskLayer {
            name: "dehaze-mask".into(),
            visible: true,
            mask: ferrolite_mask::MaskDefinition::default(), // full coverage
            adjustments: AdjustmentSet {
                dehaze: Dehaze { amount, radius },
                ..Default::default()
            },
        }],
    };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));
    assert!(stack.dehaze().is_none(), "sanity: no global Dehaze op");
    assert!(
        stack.dehaze_active_anywhere(),
        "sanity: the mask layer's amount activates the doc-wide gate"
    );
    let atmos = estimate_atmospheric_light(&src);

    let mut whole = EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    let whole_lin = common::read_image_linear(&ctx, &whole.evaluate());
    let shared_transmission = whole.transmission_texture();
    assert!(
        shared_transmission.is_some(),
        "a mask-only dehaze layer must still yield a shared transmission texture"
    );

    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    let mut tep = TileEditPipeline::new(ctx.clone(), pyramid, stack, IDENTITY, None, None);
    tep.set_dehaze_atmos(atmos);
    tep.set_shared_transmission(shared_transmission);

    use ferrolite_image::{TileCoord, TILE_SIZE};
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
                    continue;
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
    eprintln!("mask-only dehaze tile-seam max linear diff = {max_diff} at {max_loc:?}");
    assert!(
        max_diff <= SEAM_TOL,
        "per-tile mask-only dehaze diverged from whole-image (diff {max_diff}) at {max_loc:?}"
    );
}

// Regression golden for the "dehaze goes near-black when zoomed OUT beyond
// fit" bug: `dehaze_recovery.wgsl` mapped the source-UV as
// `uv = (frame_origin + gid) / src_dims`, where `src_dims` is the LEVEL-0
// source dims but `frame_origin + gid` are in the CURRENT LOD's (downscaled)
// output-pixel space. At LOD 0 that's correct (`frame_origin + gid` IS a
// level-0 pixel), but at a coarser LOD (e.g. LOD 1, half-res) it silently
// samples only a QUARTER of the transmission map's UV space (everything
// collapses toward the top-left corner) instead of the full [0,1] range —
// `dehaze_tiled_matches_whole_image` above only ever produces LOD-0 tiles, so
// it never caught this. The fix normalizes by the per-LOD `TileFrame::
// full_dims` first (LOD-independent), then re-expands by the level-0
// `GeometryUniform::out_dims` before applying the geometry mapping.
//
// This fixture is a diagonal grey ramp (not the sawtooth used above): value
// increases monotonically with `x + y`, so a wrong (quadrant-collapsed) UV
// samples a systematically different — and narrower — slice of the
// transmission map than the correct mapping, which pulls the MEAN recovered
// luminance of the coarse-LOD tile far away from the whole-image mean (the
// reported near-black symptom). A periodic/high-frequency fixture (like the
// seam test's sawtooth) would not reliably expose this, since a repeating
// pattern looks statistically similar whichever sub-range of it you sample —
// the monotonic ramp cannot hide that.
//
// MANDATORY sensitivity check (recorded in the report): reverting the
// `dehaze_recovery.wgsl`/`RecoveryParams`/`set_geometry` fix (`git stash` back
// to the pre-fix source-UV mapping) must make this test's assertion FAIL —
// confirmed: pre-fix mean diverges far outside `MEAN_LUMA_TOL`; post-fix it is
// well within it. See the report for the exact recorded numbers.
#[test]
fn dehaze_coarse_lod_matches_whole_image_mean_luminance() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);

    // 512x512 -> pyramid has exactly 2 levels: LOD 0 (512x512) and LOD 1
    // (256x256 == TILE_SIZE, a single whole-image tile, no halo needed).
    let (iw, ih) = (512u32, 512u32);
    let src = {
        let mut px = Vec::with_capacity((iw * ih * 4) as usize);
        let denom = (iw + ih - 2) as f32;
        for y in 0..ih {
            for x in 0..iw {
                // Monotonic grey ramp in [0.05, 0.90] driven by x+y.
                let v = 0.05 + ((x + y) as f32 / denom) * 0.85;
                px.extend_from_slice(&[v, v, v, 1.0]);
            }
        }
        LinearRgbaF32::new(iw, ih, px).expect("diagonal ramp fixture length")
    };

    let radius = 12u32;
    let amount = 2.0f32; // aggressive push (see the seam test's rationale)
    let stack = OpStack::default().set_op(Op::Dehaze(Dehaze { amount, radius }));
    let atmos = estimate_atmospheric_light(&src);

    // Whole-image reference (LOD 0).
    let mut whole = EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    let whole_lin = common::read_image_linear(&ctx, &whole.evaluate());
    let shared_transmission = whole.transmission_texture();
    assert!(
        shared_transmission.is_some(),
        "dehaze is active (amount != 0) -> a shared transmission texture must exist"
    );
    // The transmission is mip-mapped (LOD fix): the coarse-LOD recovery below
    // (LOD 1: full_dims 256 < the 512px map -> sample LOD 1) relies on those
    // levels existing to fetch a band-limited transmission
    // (`transmission_sample_lod`) instead of aliasing the base map into
    // ringing. Guard that the chain is actually allocated and matches the pure
    // `transmission_mip_level_count` the node builds from.
    {
        let (tw, th, _) = transmission_working_dims(iw, ih);
        let tex = shared_transmission.as_ref().unwrap();
        assert_eq!(
            tex.mip_level_count(),
            transmission_mip_level_count(tw, th),
            "transmission mip chain length must match transmission_mip_level_count"
        );
        assert!(
            tex.mip_level_count() > 1,
            "a large transmission ({tw}x{th}) must have >1 mip level so a zoomed-out \
             tile can sample a band-limited level (LOD fix)"
        );
    }
    let whole_mean: f32 = whole_lin
        .chunks_exact(4)
        .flat_map(|px| px[..3].iter().copied())
        .sum::<f32>()
        / (iw * ih * 3) as f32;

    // Coarse-LOD (zoomed-out-past-fit) tiled render, LOD 1.
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    assert_eq!(
        pyramid.level_count(),
        2,
        "fixture must have exactly a LOD-0/LOD-1 pyramid for this test's assumptions to hold"
    );
    let mut tep = TileEditPipeline::new(ctx.clone(), pyramid, stack, IDENTITY, None, None);
    tep.set_dehaze_atmos(atmos);
    tep.set_shared_transmission(shared_transmission);

    use ferrolite_image::{TileCoord, TILE_SIZE};
    let tile = tep.produce_tile(TileCoord { lod: 1, x: 0, y: 0 });
    let tile_lin = common::read_tile_linear(&ctx, &tile);
    let lod1_mean: f32 = tile_lin
        .chunks_exact(4)
        .flat_map(|px| px[..3].iter().copied())
        .sum::<f32>()
        / (TILE_SIZE * TILE_SIZE * 3) as f32;

    let rel_diff = (lod1_mean - whole_mean).abs() / whole_mean.abs().max(1e-6);
    eprintln!(
        "dehaze_coarse_lod_matches_whole_image_mean_luminance: whole_mean={whole_mean:.6} lod1_mean={lod1_mean:.6} rel_diff={rel_diff:.4}"
    );
    const MEAN_LUMA_TOL: f32 = 0.15; // 15% relative — generous but catches a quadrant-collapsed UV
    assert!(
        rel_diff <= MEAN_LUMA_TOL,
        "coarse-LOD dehaze mean luminance ({lod1_mean:.6}) diverged from whole-image mean \
         ({whole_mean:.6}, rel diff {rel_diff:.4}) — LOD-independent source-UV mapping broken?"
    );
}
