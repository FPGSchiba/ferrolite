//! GPU goldens for the Spec 3 color pipeline: the camera→working ColorMatrixNode
//! and the sRGB≡old blit regression. Auto-skip when no GPU adapter is present.

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_pipeline::{EditPipeline, OpStack};

const TOL: u8 = 4;

/// A 2×2 image with distinct linear RGB per texel (values chosen to stay in-gamut
/// after a channel-swap matrix and below the sRGB linear knee for at least one).
fn probe_image() -> LinearRgbaF32 {
    // RGBA f32, row-major, 2×2.
    let px = vec![
        0.20, 0.40, 0.60, 1.0, //
        0.50, 0.10, 0.30, 1.0, //
        0.05, 0.25, 0.45, 1.0, //
        0.60, 0.55, 0.15, 1.0, //
    ];
    LinearRgbaF32::new(2, 2, px).unwrap()
}

fn srgb_oetf(l: f32) -> f32 {
    if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

#[test]
fn color_matrix_node_applies_matrix_before_srgb() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let img = probe_image();
    // A channel-swap + scale matrix (row-major): out.r = 0.5*b, out.g = r, out.b = g.
    let m = [[0.0, 0.0, 0.5], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mut ep = EditPipeline::new(
        std::sync::Arc::new(ctx),
        &img,
        OpStack::default(), // identity ops: isolate the color matrix
        m,
    );
    let out = ep.render_to_image(); // sRGB Rgba8, 2×2, row-unpadded

    for i in 0..4usize {
        let (r, g, b) = (
            img.pixels[i * 4],
            img.pixels[i * 4 + 1],
            img.pixels[i * 4 + 2],
        );
        let lin = [0.5 * b, r, g]; // expected linear after the matrix
        for c in 0..3 {
            let want = (srgb_oetf(lin[c]).clamp(0.0, 1.0) * 255.0).round() as i32;
            let got = out[i * 4 + c] as i32;
            assert!(
                (want - got).abs() <= TOL as i32,
                "texel {i} ch {c}: want {want} got {got}"
            );
        }
    }
}

/// Regression invariant (spec §4.3): the identity-matrix tail == the old
/// hardcoded `linear_to_srgb`. Proven by comparing the identity blit against
/// `ferrolite_color::srgb_oetf` over a known image.
#[test]
fn blit_srgb_identity_equals_old_linear_to_srgb() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let img = probe_image();
    // Upload as a PipelineImage via a no-op identity EditPipeline evaluate.
    let mut ep = EditPipeline::new(
        std::sync::Arc::new(ctx),
        &img,
        OpStack::default(),
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    );
    let out = ep.render_to_image(); // uses blit_to_rgba8 (identity)

    for i in 0..4usize {
        for c in 0..3 {
            let lin = img.pixels[i * 4 + c];
            let want = (ferrolite_color::srgb_oetf(lin).clamp(0.0, 1.0) * 255.0).round() as i32;
            let got = out[i * 4 + c] as i32;
            assert!(
                (want - got).abs() <= TOL as i32,
                "texel {i} ch {c}: identity tail drifted from sRGB OETF (want {want}, got {got})"
            );
        }
    }
}
