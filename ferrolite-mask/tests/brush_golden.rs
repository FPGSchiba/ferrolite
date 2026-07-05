mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{stroke_dabs, BrushNode, BrushRasterizer, Stroke, Vec2, SPACING_FRAC};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 64;

fn node(x: f32, y: f32, r: f32, hardness: f32) -> BrushNode {
    BrushNode {
        pos: Vec2::new(x, y),
        radius: r,
        hardness,
        flow: 1.0,
    }
}

#[test]
fn brush_stroke_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden (expected in headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());
    // A soft diagonal stroke across the frame.
    let stroke = Stroke {
        nodes: vec![
            node(0.2, 0.25, 0.12, 0.4),
            node(0.5, 0.5, 0.15, 0.4),
            node(0.8, 0.75, 0.12, 0.4),
        ],
        erase: false,
    };
    let dabs = stroke_dabs(&stroke, SPACING_FRAC);
    assert!(!dabs.is_empty());
    let mask = r.rasterize_full(&dabs, false, W, H);
    let values = common::read_r32f(&ctx, &mask);
    // Sanity: mid of the stroke is painted, a far corner is not.
    let mid = values[((H / 2) * W + W / 2) as usize];
    assert!(mid > 0.9, "stroke midpoint painted, got {mid}");
    assert!(values[0] < 0.01, "top-left corner untouched");
    common::assert_mask_golden(&values, W, H, "brush_stroke.png");
}

#[test]
fn brush_erase_carves_out_of_full_mask() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());
    // Start from a fully-painted mask (one big central dab), then erase a dot.
    let paint = stroke_dabs(
        &Stroke {
            nodes: vec![node(0.5, 0.5, 1.2, 1.0)],
            erase: false,
        },
        SPACING_FRAC,
    );
    let full = r.rasterize_full(&paint, false, W, H);
    let erase = stroke_dabs(
        &Stroke {
            nodes: vec![node(0.5, 0.5, 0.25, 0.8)],
            erase: true,
        },
        SPACING_FRAC,
    );
    let carved = r.stamp_onto(&full, &erase, true, (0, 0), (W, H));
    let values = common::read_r32f(&ctx, &carved);
    let center = values[((H / 2) * W + W / 2) as usize];
    assert!(center < 0.2, "center erased, got {center}");
    common::assert_mask_golden(&values, W, H, "brush_erase.png");
}
