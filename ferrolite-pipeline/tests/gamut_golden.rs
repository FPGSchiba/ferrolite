//! GPU goldens for the P2 gamut-preserving unclamp (spec §5.3): an identity edit
//! chain must carry out-of-range values (highlights >1 and negative wide-gamut
//! channels) through the tone-curve and HSL nodes to the tail, where they clip.
//! We read the working buffer through `blit_to_rgba8_with_matrix` with a probing
//! matrix (scale-down / channel-mix) so a still-out-of-range value maps to a
//! distinct in-[0,1] readback — distinguishing "preserved" from "crushed" without
//! a float readback. Auto-skip when no GPU adapter is present.

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_pipeline::{blit_to_rgba8_with_matrix, EditPipeline, OpStack};

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
const TOL: i32 = 4;

fn srgb_oetf(l: f32) -> f32 {
    if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

fn u8_of(lin: f32) -> i32 {
    (srgb_oetf(lin.clamp(0.0, 1.0)) * 255.0).round() as i32
}

#[test]
fn identity_chain_preserves_highlight_above_one() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // One texel, all channels 1.5 (a blown highlight).
    let src = LinearRgbaF32::new(1, 1, vec![1.5, 1.5, 1.5, 1.0]).unwrap();
    let mut ep = EditPipeline::new(std::sync::Arc::new(ctx), &src, OpStack::default(), IDENTITY);
    let img = ep.evaluate();
    let gpu = ep.gpu_context();
    // Read back through a 0.5x display matrix: preserved 1.5 -> 0.75; a value
    // crushed to 1.0 would read back 0.5. sRGB(0.75) vs sRGB(0.5) differ by ~37 codes.
    let half = [[0.5, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.5]];
    let out = blit_to_rgba8_with_matrix(&gpu, &img, half);
    let want = u8_of(0.75);
    for (c, &got) in out.iter().take(3).enumerate() {
        assert!(
            (got as i32 - want).abs() <= TOL,
            "channel {c}: highlight crushed — want {want} (0.75 lin) got {got} ; a crush would read {}",
            u8_of(0.5)
        );
    }
}

#[test]
fn identity_chain_preserves_negative_channel() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // R negative (wide-gamut), G=1, B=0.5.
    let src = LinearRgbaF32::new(1, 1, vec![-0.2, 1.0, 0.5, 1.0]).unwrap();
    let mut ep = EditPipeline::new(std::sync::Arc::new(ctx), &src, OpStack::default(), IDENTITY);
    let img = ep.evaluate();
    let gpu = ep.gpu_context();
    // Display matrix mixes G into R (row 0 = [1,1,0]): preserved R=-0.2 -> -0.2+1.0=0.8;
    // an R crushed to 0 would read back 0+1.0=1.0. sRGB(0.8) vs sRGB(1.0) differ clearly.
    let mix = [[1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let out = blit_to_rgba8_with_matrix(&gpu, &img, mix);
    let want = u8_of(0.8);
    assert!(
        (out[0] as i32 - want).abs() <= TOL,
        "R channel: negative crushed — want {want} (0.8 lin) got {} ; a crush would read {}",
        out[0],
        u8_of(1.0)
    );
}
