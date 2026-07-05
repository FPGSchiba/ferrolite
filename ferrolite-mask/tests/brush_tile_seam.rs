mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_image::{TileCoord, TILE_SIZE};
use ferrolite_mask::{
    halo_px, max_dab_radius, stroke_dabs, BrushNode, BrushRasterizer, Stroke, Vec2, SPACING_FRAC,
};
use std::sync::Arc;

fn node(x: f32, y: f32, r: f32) -> BrushNode {
    BrushNode {
        pos: Vec2::new(x, y),
        radius: r,
        hardness: 0.4,
        flow: 1.0,
    }
}

// Quantize an [0,1] value the way the golden helper does, for u8 diffing.
fn q(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[test]
fn haloed_tiles_match_whole_image_at_seams() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());

    // 512x512 = exactly 2x2 tiles at lod 0 (no partial tiles).
    let dim = TILE_SIZE * 2;
    // A stroke that crosses the central seam (x=0.5) so a dab straddles the
    // tile border and MUST rasterize completely on each side.
    let stroke = Stroke {
        nodes: vec![node(0.35, 0.5, 0.06), node(0.65, 0.5, 0.06)],
        erase: false,
    };
    let dabs = stroke_dabs(&stroke, SPACING_FRAC);
    assert!(!dabs.is_empty());

    // Whole-image reference.
    let whole = r.rasterize_full(&dabs, false, dim, dim);
    let whole_vals = common::read_r32f(&ctx, &whole);

    // halo = max dab radius, in pixels at this level.
    let halo = halo_px(max_dab_radius(std::slice::from_ref(&stroke)), dim, dim);
    assert!(halo > 0, "stroke has a positive radius -> positive halo");

    // For each of the 4 tiles, rasterize the interior with halo and compare it
    // to the corresponding TILE_SIZE region of the whole image.
    for ty in 0..2u32 {
        for tx in 0..2u32 {
            let coord = TileCoord {
                lod: 0,
                x: tx,
                y: ty,
            };
            let tile = r.rasterize_tile(&dabs, false, coord, halo, (dim, dim));
            let tile_vals = common::read_r32f(&ctx, &tile);
            let (ox, oy) = (tx * TILE_SIZE, ty * TILE_SIZE);
            let mut max_diff = 0u8;
            for iy in 0..TILE_SIZE {
                for ix in 0..TILE_SIZE {
                    let t = q(tile_vals[(iy * TILE_SIZE + ix) as usize]);
                    let w = q(whole_vals[((oy + iy) * dim + (ox + ix)) as usize]);
                    max_diff = max_diff.max(t.abs_diff(w));
                }
            }
            assert!(
                max_diff <= 1,
                "tile ({tx},{ty}) drifted from whole image by {max_diff} (seam/halo bug)"
            );
        }
    }
}

#[test]
fn incremental_stamping_equals_single_shot() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());
    let dim = 128;
    let stroke = Stroke {
        nodes: vec![
            node(0.2, 0.5, 0.08),
            node(0.5, 0.5, 0.08),
            node(0.8, 0.5, 0.08),
        ],
        erase: false,
    };
    let dabs = stroke_dabs(&stroke, SPACING_FRAC);
    assert!(dabs.len() >= 4, "need several dabs to split");

    // Single shot.
    let whole = r.rasterize_full(&dabs, false, dim, dim);
    let whole_vals = common::read_r32f(&ctx, &whole);

    // Incremental: split the dab list and stamp in two passes (ping-pong).
    let split = dabs.len() / 2;
    let base = ferrolite_mask::MaskBuffer::alloc_zeroed(&ctx, dim, dim);
    let step1 = r.stamp_onto(&base, &dabs[..split], false, (0, 0), (dim, dim));
    let step2 = r.stamp_onto(&step1, &dabs[split..], false, (0, 0), (dim, dim));
    let inc_vals = common::read_r32f(&ctx, &step2);

    let a: Vec<u8> = whole_vals.iter().map(|&v| q(v)).collect();
    let b: Vec<u8> = inc_vals.iter().map(|&v| q(v)).collect();
    assert!(
        common::mask_max_abs_diff(&a, &b) <= 1,
        "incremental stamping diverged from single-shot"
    );
}
