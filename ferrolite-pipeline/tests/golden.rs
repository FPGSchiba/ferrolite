mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_pipeline::{blit_to_rgba8, upload_source, EditPipeline, Exposure, Op, OpStack, WhiteBalance};
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
    let stack = OpStack::default().set_op(Op::WhiteBalance(WhiteBalance { temp: 0.5, tint: -0.2 }));
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
    assert!(diff <= 4, "identity stack diverged from source (diff {diff})");
}
