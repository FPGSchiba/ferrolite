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

/// §10 GPU golden: a dual-illuminant matrix INTERPOLATED at a fixed CCT (via
/// `ferrolite_color::camera_to_working_interpolated`) must flow through the
/// `ColorMatrixNode` and match the same matrix applied on the CPU (+ sRGB OETF).
/// Proves Plan 1's interpolation result reaches the GPU head unchanged.
#[test]
fn interpolated_matrix_flows_through_color_matrix_node() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    use ferrolite_color::Xy;
    // Two distinct fake calibrations (Standard-A-ish and D65-ish white points).
    let a_white = Xy {
        x: 0.4476,
        y: 0.4074,
    };
    let d65_white = Xy {
        x: 0.3128,
        y: 0.3290,
    };
    let m_a = [[1.0, 0.1, 0.0], [0.2, 1.0, 0.1], [0.0, 0.2, 1.0]];
    let m_d65 = [[1.2, -0.1, 0.0], [-0.05, 1.1, -0.05], [0.0, -0.1, 1.3]];
    let cals = [(a_white, m_a), (d65_white, m_d65)];
    let m = ferrolite_color::camera_to_working_interpolated(
        &cals,
        4000.0,
        ferrolite_color::WorkingSpace::Rec2020,
    );

    let img = probe_image();
    let mut ep = EditPipeline::new(std::sync::Arc::new(ctx), &img, OpStack::default(), m);
    let out = ep.render_to_image();

    for i in 0..4usize {
        let (r, g, b) = (
            img.pixels[i * 4],
            img.pixels[i * 4 + 1],
            img.pixels[i * 4 + 2],
        );
        // Expected linear = m · [r,g,b] (row-major), matching the shader.
        let lin = [
            m[0][0] * r + m[0][1] * g + m[0][2] * b,
            m[1][0] * r + m[1][1] * g + m[1][2] * b,
            m[2][0] * r + m[2][1] * g + m[2][2] * b,
        ];
        for c in 0..3 {
            let want = (srgb_oetf(lin[c].clamp(0.0, 1.0)) * 255.0).round() as i32;
            let got = out[i * 4 + c] as i32;
            assert!(
                (want - got).abs() <= TOL as i32,
                "texel {i} ch {c}: want {want} got {got}"
            );
        }
    }
}

/// The Plan 2 mechanic: re-pushing a new matrix via `set_color_matrix` (the same
/// call the app makes on a WB temp change) updates the output live — no rebuild.
#[test]
fn set_color_matrix_repush_changes_output_live() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let img = probe_image();
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let mut ep = EditPipeline::new(std::sync::Arc::new(ctx), &img, OpStack::default(), identity);
    let before = ep.render_to_image();

    // Channel-swap matrix: out.r = b, out.g = r, out.b = g — visibly different.
    let swap = [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    ep.set_color_matrix(swap);
    let after = ep.render_to_image();

    assert_ne!(
        before, after,
        "re-pushing the color matrix must change the output"
    );
    // Spot-check: after-swap red channel == before green OETF-wise (r_out = b_in path
    // is hard to compare directly; assert the swapped output matches a CPU apply).
    for i in 0..4usize {
        let (r, g, b) = (
            img.pixels[i * 4],
            img.pixels[i * 4 + 1],
            img.pixels[i * 4 + 2],
        );
        let lin = [b, r, g];
        for c in 0..3 {
            let want = (srgb_oetf(lin[c].clamp(0.0, 1.0)) * 255.0).round() as i32;
            let got = after[i * 4 + c] as i32;
            assert!(
                (want - got).abs() <= TOL as i32,
                "texel {i} ch {c}: want {want} got {got}"
            );
        }
    }
}
