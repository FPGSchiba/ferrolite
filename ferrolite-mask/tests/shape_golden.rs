mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{LinearGradientPass, Vec2};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 48;

#[test]
fn linear_gradient_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden (expected in headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let pass = LinearGradientPass::new(ctx.clone());
    // Horizontal ramp across the middle third of the image.
    let mask = pass.run(Vec2::new(0.2, 0.5), Vec2::new(0.8, 0.5), W, H);
    let values = common::read_r32f(&ctx, &mask);
    // Sanity: left edge clamps to 0, right edge clamps to 1.
    assert!(values[0] < 0.01, "left edge should clamp to 0");
    assert!(
        values[(W - 1) as usize] > 0.99,
        "right edge should clamp to 1"
    );
    common::assert_mask_golden(&values, W, H, "linear_gradient.png");
}
