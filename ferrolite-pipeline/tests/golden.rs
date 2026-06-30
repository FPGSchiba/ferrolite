mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_pipeline::{
    blit_to_rgba8, upload_source, Aspect, Contrast, CropRect, EditPipeline, Exposure, Geometry,
    GpuPyramidSource, Hsl, HslBand, Op, OpStack, Sharpen, TileEditPipeline, ToneCurve,
    WhiteBalance,
};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 48;

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
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
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
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
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
    let mut pipe = EditPipeline::new(ctx.clone(), &src, OpStack::default());
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
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "contrast_boost.png");
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
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
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
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), base.clone());

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

    // Dirtying the root op (exposure) re-runs it + every downstream op; the
    // source node stays cached -> exactly node_count - 1 re-evaluations.
    let prev = pipe.eval_count();
    pipe.set_stack(OpStack::default().set_op(Op::Exposure(Exposure { ev: 1.5 })));
    let _ = pipe.evaluate();
    assert_eq!(
        pipe.eval_count(),
        prev + (pipe.node_count() - 1),
        "exposure + every downstream op re-evaluated (source stays cached)"
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
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "tone_curve.png");
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
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
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
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "hsl.png");
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
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
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
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
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
    let mut whole = EditPipeline::new(ctx.clone(), &src, stack.clone());
    let whole_lin = common::read_image_linear(&ctx, &whole.evaluate());

    // Per-tile producer over the GPU-resident source pyramid.
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    let mut tep = TileEditPipeline::new(ctx.clone(), pyramid, stack);

    // Produce both tiles, read interiors, and compare the valid region against
    // the whole-image reference — focusing on the seam column.
    use ferrolite_image::{TileCoord, TILE_SIZE};
    let mut max_diff = 0.0f32;
    for tx in 0..2u32 {
        let tile = tep.produce_tile(TileCoord { lod: 0, x: tx, y: 0 });
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
