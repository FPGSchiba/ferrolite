mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{ColorRangePass, LinearGradientPass, LumaRangePass, RadialGradientPass, Vec2};
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

#[test]
fn radial_gradient_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let pass = RadialGradientPass::new(ctx.clone());
    // Centred ellipse, wider than tall, mild feather.
    let mask = pass.run(
        Vec2::new(0.5, 0.5),
        Vec2::new(0.35, 0.2),
        0.0,
        0.3,
        false,
        W,
        H,
    );
    let values = common::read_r32f(&ctx, &mask);
    let center = values[((H / 2) * W + W / 2) as usize];
    assert!(center > 0.99, "ellipse center should be fully selected");
    assert!(values[0] < 0.01, "top-left corner should be outside");
    common::assert_mask_golden(&values, W, H, "radial_gradient.png");
}

#[test]
fn luma_range_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // Vertical luma ramp: dark at top (y=0) to bright at bottom (y=H-1).
    let input = common::upload_rgba16f(&ctx, W, H, |_x, y| {
        let l = y as f32 / (H - 1) as f32;
        [l, l, l, 1.0]
    });
    let view = input.create_view(&wgpu::TextureViewDescriptor::default());
    let pass = LumaRangePass::new(ctx.clone());
    // Select mid-tones [0.35, 0.65].
    let mask = pass.run(0.35, 0.65, 0.05, &view, W, H);
    let values = common::read_r32f(&ctx, &mask);
    assert!(values[0] < 0.01, "darkest row should be outside the band");
    let mid = values[((H / 2) * W) as usize];
    assert!(mid > 0.99, "mid-tone row should be fully selected");
    common::assert_mask_golden(&values, W, H, "luma_range.png");
}

#[test]
fn color_range_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // Left half red, right half green.
    let input = common::upload_rgba16f(&ctx, W, H, |x, _y| {
        if x < W / 2 {
            [1.0, 0.0, 0.0, 1.0]
        } else {
            [0.0, 1.0, 0.0, 1.0]
        }
    });
    let view = input.create_view(&wgpu::TextureViewDescriptor::default());
    let pass = ColorRangePass::new(ctx.clone());
    // Select near-red only.
    let mask = pass.run(
        &[ferrolite_mask::Rgb::new(1.0, 0.0, 0.0)],
        0.3,
        0.1,
        &view,
        W,
        H,
    );
    let values = common::read_r32f(&ctx, &mask);
    assert!(values[0] > 0.99, "red region selected");
    assert!(values[(W - 1) as usize] < 0.01, "green region rejected");
    common::assert_mask_golden(&values, W, H, "color_range.png");
}
