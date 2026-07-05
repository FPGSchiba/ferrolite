mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{BrushRasterizer, Dab, Vec2};
use std::sync::Arc;

#[test]
fn single_dab_paints_center_and_leaves_corner_empty() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());
    let dab = Dab {
        pos: Vec2::new(0.5, 0.5),
        radius: 0.25,
        hardness: 0.5,
        flow: 1.0,
    };
    let mask = r.rasterize_full(&[dab], false, 64, 64);
    let values = common::read_r32f(&ctx, &mask);
    let center = values[((64 / 2) * 64 + 64 / 2) as usize];
    assert!(center > 0.99, "center painted, got {center}");
    assert!(values[0] < 0.01, "corner empty, got {}", values[0]);
}

#[test]
fn empty_dab_batch_is_identity() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());
    // Zero dabs must not panic (>=1 storage record uploaded internally) and must
    // leave the zeroed base untouched.
    let mask = r.rasterize_full(&[], false, 32, 32);
    let values = common::read_r32f(&ctx, &mask);
    assert!(values.iter().all(|&v| v == 0.0), "empty batch is identity");
}
