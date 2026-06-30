mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_pipeline::{
    blit_to_rgba8, upload_source, Contrast, EditPipeline, Exposure, Op, OpStack, ToneCurve,
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
