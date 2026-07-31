//! GPU behaviour of the NR node: the identity gate, real denoising, and the
//! no-allocation-at-identity property the memory gate depends on.
mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_pipeline::{blit_to_rgba8, EditPipeline, NoiseReduction, OpStack};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 64;
const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn luma_variance(px: &[u8]) -> f32 {
    let lum: Vec<f32> = px
        .chunks_exact(4)
        .map(|c| 0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32)
        .collect();
    let m = lum.iter().sum::<f32>() / lum.len() as f32;
    lum.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / lum.len() as f32
}

/// Gate 1 (spec §7.2): identity NR is a byte-exact passthrough.
#[test]
fn nr_identity_is_byte_identical() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let img = common::gradient(W, H);

    let mut base = EditPipeline::new(ctx.clone(), &img, OpStack::default(), IDENTITY);
    let want = blit_to_rgba8(&ctx, &base.evaluate());

    let mut doc = OpStack::default();
    doc.global.noise_reduction = NoiseReduction::default();
    let mut with_nr = EditPipeline::new(ctx.clone(), &img, doc, IDENTITY);
    let got = blit_to_rgba8(&ctx, &with_nr.evaluate());

    assert_eq!(want, got, "identity NR changed the render");
}

/// Identity NR must not dispatch, and must allocate NOTHING — the property the
/// memory gate (Step 7) and the zero-cost claim both rest on.
#[test]
fn nr_identity_dispatches_nothing_and_allocates_nothing() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let mut pipe = EditPipeline::new(
        Arc::new(ctx),
        &common::gradient(W, H),
        OpStack::default(),
        IDENTITY,
    );
    let _ = pipe.evaluate();
    assert_eq!(pipe.nr_eval_count(), 0, "identity NR must dispatch nothing");
    assert_eq!(
        pipe.nr_live_bytes(),
        0,
        "identity NR must allocate no textures"
    );
}

/// Active NR must actually denoise — the GPU counterpart of
/// `nr::tests::white_noise_variance_drops`.
#[test]
fn nr_reduces_variance_on_noise() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let img = common::noisy_flat(W, H);

    let mut base = EditPipeline::new(ctx.clone(), &img, OpStack::default(), IDENTITY);
    let before = luma_variance(&blit_to_rgba8(&ctx, &base.evaluate()));

    let mut doc = OpStack::default();
    doc.global.noise_reduction = NoiseReduction {
        luminance: 1.0,
        ..Default::default()
    };
    let mut denoised = EditPipeline::new(ctx.clone(), &img, doc, IDENTITY);
    let after = luma_variance(&blit_to_rgba8(&ctx, &denoised.evaluate()));

    assert!(after < before * 0.8, "variance {after} not below {before}");
}

/// A flat field has no detail at any scale, so NR cannot change it — the GPU
/// counterpart of `nr::tests::flat_image_is_unchanged_by_any_strength`, and the
/// check that catches a stale (un-zeroed) accumulator.
#[test]
fn nr_leaves_a_flat_field_alone() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let flat = {
        let px = vec![0.4f32; (W * H * 4) as usize];
        ferrolite_image::LinearRgbaF32::new(W, H, px).expect("flat length")
    };
    let mut base = EditPipeline::new(ctx.clone(), &flat, OpStack::default(), IDENTITY);
    let want = blit_to_rgba8(&ctx, &base.evaluate());

    let mut doc = OpStack::default();
    doc.global.noise_reduction = NoiseReduction {
        luminance: 1.0,
        color: 1.0,
        ..Default::default()
    };
    let mut denoised = EditPipeline::new(ctx.clone(), &flat, doc, IDENTITY);
    let got = blit_to_rgba8(&ctx, &denoised.evaluate());

    assert!(
        common::max_abs_diff(&want, &got) <= 1,
        "flat field changed under NR (stale accumulator?)"
    );
}
