//! GPU goldens for the tiled export render. Auto-skip headless. Proves the
//! tile-by-tile render + convert matches a whole-image reference (tile-seam
//! correctness reusing the Spec 2 halo), and that cancellation stops the render.

use std::sync::Arc;

use ferrolite_color::WorkingSpace;
use ferrolite_export::{render_tiled, BitDepth, PixelData};
use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_jobs::CancelToken;
use ferrolite_lens::{load_bundled, LensDb};
use ferrolite_pipeline::{
    estimate_atmospheric_light, Correction, Dehaze, EditPipeline, GpuPyramidSource, LensCorrection,
    Op, OpStack,
};

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
        None,
        BitDepth::Eight,
        ferrolite_pipeline::DEHAZE_ATMOS_NEUTRAL,
        None,
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
        None,
        BitDepth::Eight,
        ferrolite_pipeline::DEHAZE_ATMOS_NEUTRAL,
        None,
        &cancel,
        &mut |_, _| {},
    );
    assert!(matches!(r, Err(ferrolite_export::ExportError::Cancelled)));
}

// ---------------------------------------------------------------------------
// C1 golden: the export path actually RENDERS lens corrections. Mirrors the
// pipeline `lens_golden.rs` corrected render — export a synthetic image with a
// real bundled lens's distortion+TCA+vignetting enabled and assert the output
// differs from the uncorrected export (and is non-trivially so). Before the C1
// fix, `render_tiled` passed `None, None` and this diff would be exactly zero.
// ---------------------------------------------------------------------------

/// The bundled distorting lens used by the pipeline lens goldens (real Lensfun
/// distortion + TCA + vignetting calibration; wide end distorts most).
fn corrected_lens_op() -> Op {
    Op::LensCorrection(LensCorrection {
        lens_id: Some("Canon EF 24-70mm f/2.8L II USM".into()),
        focal_len: 24.0,
        aperture: 2.8, // wide open vignettes most
        crop_factor: 1.0,
        distortion: Correction {
            enabled: true,
            amount: 1.0,
        },
        tca: Correction {
            enabled: true,
            amount: 1.0,
        },
        vignetting: Correction {
            enabled: true,
            amount: 1.0,
        },
    })
}

#[test]
fn export_renders_lens_corrections() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let Ok(db) = load_bundled() else {
        eprintln!("bundled lens db unavailable; skipping");
        return;
    };
    // Confirm the bundled db actually resolves + bakes for this lens; otherwise
    // the corrected export would be identity and the assertion below vacuous.
    let db = Arc::new(db);
    if db.match_by_id("Canon EF 24-70mm f/2.8L II USM").is_none() {
        eprintln!("bundled db has no calibration for the fixture lens; skipping");
        return;
    }

    let (w, h) = (600u32, 500u32);
    let img = probe(w, h);
    let ctx = Arc::new(ctx);
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &img));
    let cancel = CancelToken::new();

    let stack = OpStack::default().set_op(corrected_lens_op());

    // Corrected export (db present → bakes + renders the warp/vignette).
    let corrected = render_tiled(
        &ctx,
        &pyramid,
        &stack,
        IDENTITY,
        WorkingSpace::Srgb,
        WorkingSpace::Srgb,
        Some(&db),
        BitDepth::Eight,
        ferrolite_pipeline::DEHAZE_ATMOS_NEUTRAL,
        None,
        &cancel,
        &mut |_, _| {},
    )
    .expect("corrected render");

    // Uncorrected export of the SAME stack but with no db → identity (the
    // pre-C1 behavior). Same dimensions (a lens correction doesn't change the
    // output size), so a pixel-wise diff is well defined.
    let uncorrected = render_tiled(
        &ctx,
        &pyramid,
        &stack,
        IDENTITY,
        WorkingSpace::Srgb,
        WorkingSpace::Srgb,
        None,
        BitDepth::Eight,
        ferrolite_pipeline::DEHAZE_ATMOS_NEUTRAL,
        None,
        &cancel,
        &mut |_, _| {},
    )
    .expect("uncorrected render");

    assert_eq!(
        (corrected.width, corrected.height),
        (uncorrected.width, uncorrected.height),
        "a lens correction must not change output dimensions"
    );
    let PixelData::Eight(a) = corrected.data else {
        panic!("expected 8-bit corrected")
    };
    let PixelData::Eight(b) = uncorrected.data else {
        panic!("expected 8-bit uncorrected")
    };
    assert_eq!(a.len(), b.len());

    let mut max_diff = 0i32;
    let mut changed = 0usize;
    for (x, y) in a.iter().zip(b.iter()) {
        let d = (*x as i32 - *y as i32).abs();
        max_diff = max_diff.max(d);
        if d > 0 {
            changed += 1;
        }
    }
    eprintln!("export corrected-vs-uncorrected max diff = {max_diff}, changed bytes = {changed}");
    assert!(
        max_diff > 2,
        "the export must visibly apply the lens correction (max diff {max_diff}) — \
         render_tiled dropped the bake?"
    );
    // A distortion+TCA+vignetting correction touches a large fraction of pixels,
    // not just a handful of edge samples.
    assert!(
        changed > a.len() / 10,
        "the correction should affect a substantial region ({changed}/{} bytes)",
        a.len()
    );
}

// ---------------------------------------------------------------------------
// ST-Task 5 golden: the export path actually RENDERS dehaze. Before this fix,
// `render_tiled` never called `TileEditPipeline::set_shared_transmission`, so
// an exported dehaze stack rendered as an identity passthrough — the amount
// slider would have zero effect on the exported file even though it visibly
// changed the on-screen preview. Proves `transmission_source: Some(&img)`
// makes a real difference vs. `None` (the passthrough) for the SAME active
// dehaze stack + atmospheric light.
// ---------------------------------------------------------------------------

#[test]
fn export_renders_dehaze() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let (w, h) = (600u32, 500u32);
    let img = probe(w, h);
    let ctx = Arc::new(ctx);
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &img));
    let cancel = CancelToken::new();

    // `amount: 2.0` keeps the recovery's dependence on the transmission `t`
    // strong (mirrors the pipeline tile-seam golden), so a dropped/passthrough
    // transmission is not masked by a small amount.
    let stack = OpStack::default().set_op(Op::Dehaze(Dehaze {
        amount: 2.0,
        radius: 12,
    }));
    // Estimated once from the same CPU source `render_tiled` builds its bounded
    // transmission from, exactly as `App::confirm_export` does via `ViewerState::
    // dehaze_atmos`.
    let atmos = estimate_atmospheric_light(&img);

    let dehazed = render_tiled(
        &ctx,
        &pyramid,
        &stack,
        IDENTITY,
        WorkingSpace::Srgb,
        WorkingSpace::Srgb,
        None,
        BitDepth::Eight,
        atmos,
        Some(&img),
        &cancel,
        &mut |_, _| {},
    )
    .expect("dehazed render");

    // Same stack/atmos but `transmission_source: None` — the pre-fix behavior:
    // no shared transmission is ever bound, so the tiled recovery passes `I`
    // through unchanged regardless of `amount`/`radius`.
    let passthrough = render_tiled(
        &ctx,
        &pyramid,
        &stack,
        IDENTITY,
        WorkingSpace::Srgb,
        WorkingSpace::Srgb,
        None,
        BitDepth::Eight,
        atmos,
        None,
        &cancel,
        &mut |_, _| {},
    )
    .expect("passthrough render");

    assert_eq!(
        (dehazed.width, dehazed.height),
        (passthrough.width, passthrough.height)
    );
    let PixelData::Eight(a) = dehazed.data else {
        panic!("expected 8-bit dehazed")
    };
    let PixelData::Eight(b) = passthrough.data else {
        panic!("expected 8-bit passthrough")
    };
    assert_eq!(a.len(), b.len());

    let mut max_diff = 0i32;
    let mut changed = 0usize;
    for (x, y) in a.iter().zip(b.iter()) {
        let d = (*x as i32 - *y as i32).abs();
        max_diff = max_diff.max(d);
        if d > 0 {
            changed += 1;
        }
    }
    eprintln!("export dehazed-vs-passthrough max diff = {max_diff}, changed bytes = {changed}");
    assert!(
        max_diff > 2,
        "an active dehaze stack must visibly change the exported render (max diff \
         {max_diff}) — did render_tiled drop the shared transmission wiring?"
    );
    assert!(
        changed > a.len() / 10,
        "dehaze should affect a substantial region ({changed}/{} bytes)",
        a.len()
    );
}
