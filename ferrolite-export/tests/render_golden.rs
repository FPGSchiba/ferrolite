//! GPU goldens for the tiled export render. Auto-skip headless. Proves the
//! tile-by-tile render + convert matches a whole-image reference (tile-seam
//! correctness reusing the Spec 2 halo), and that cancellation stops the render.

use std::sync::Arc;

use ferrolite_color::WorkingSpace;
use ferrolite_export::{render_tiled, BitDepth, PixelData};
use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_jobs::CancelToken;
use ferrolite_pipeline::{EditPipeline, GpuPyramidSource, OpStack};

const TOL: i32 = 6; // absorbs f16 + tile-edge resample (Spec 2 SEAM_TOL rationale)

fn probe(w: u32, h: u32) -> LinearRgbaF32 {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[(x as f32 / w as f32), (y as f32 / h as f32), 0.35, 1.0]);
        }
    }
    LinearRgbaF32::new(w, h, px).unwrap()
}

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

#[test]
fn tiled_render_matches_whole_image_reference() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Non-tile-aligned dims so edge tiles are exercised.
    let (w, h) = (600u32, 500u32);
    let img = probe(w, h);
    let ctx = Arc::new(ctx);

    // Whole-image reference: preview EditPipeline (uploads whole image; fine in a
    // small test) with sRGB working/output so the tail == the export convert path.
    let mut ep = EditPipeline::new(ctx.clone(), &img, OpStack::default(), IDENTITY);
    let reference = ep.render_to_image(); // sRGB Rgba8, w×h, row-unpadded

    // Tiled export render, sRGB working -> sRGB output, 8-bit RGB.
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &img));
    let cancel = CancelToken::new();
    let mut seen = (0u32, 0u32);
    let out = render_tiled(
        &ctx,
        &pyramid,
        &OpStack::default(),
        IDENTITY,
        WorkingSpace::Srgb,
        WorkingSpace::Srgb,
        BitDepth::Eight,
        &cancel,
        &mut |d, t| seen = (d, t),
    )
    .expect("render");

    assert_eq!((out.width, out.height), (w, h));
    assert_eq!(seen.0, seen.1, "progress reached 100%");
    let PixelData::Eight(rgb) = out.data else {
        panic!("expected 8-bit")
    };
    // Compare RGB (export) vs RGBA (reference) channel-by-channel within tolerance.
    let mut max_diff = 0i32;
    for i in 0..(w * h) as usize {
        for c in 0..3 {
            let a = rgb[i * 3 + c] as i32;
            let b = reference[i * 4 + c] as i32;
            max_diff = max_diff.max((a - b).abs());
        }
    }
    assert!(
        max_diff <= TOL,
        "tiled vs whole-image max channel diff {max_diff} > {TOL}"
    );
}

#[test]
fn cancellation_stops_render() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let img = probe(600, 500);
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &img));
    let cancel = CancelToken::new();
    cancel.cancel(); // pre-cancelled -> first tile check returns Cancelled
    let r = render_tiled(
        &ctx,
        &pyramid,
        &OpStack::default(),
        IDENTITY,
        WorkingSpace::Srgb,
        WorkingSpace::Srgb,
        BitDepth::Eight,
        &cancel,
        &mut |_, _| {},
    );
    assert!(matches!(r, Err(ferrolite_export::ExportError::Cancelled)));
}
