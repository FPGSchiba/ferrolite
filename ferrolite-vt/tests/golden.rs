mod common;
use ferrolite_gpu::GpuContext;
use ferrolite_vt::{ViewTransform, VirtualTexture};

const TOL: u8 = 4; // absorbs driver float differences

#[test]
fn rung1_fit_view_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden (expected in headless CI)");
        return;
    };
    let img = common::split_image();
    let (w, h) = (64u32, 64u32);
    let view = ViewTransform::fit((img.width, img.height), (w as f32, h as f32));
    let pixels = VirtualTexture::render_to_image(&ctx, &img, &view, (w as f32, h as f32), w, h);

    let golden_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rung1_fit.png");
    if std::env::var("UPDATE_GOLDEN").is_ok() || !std::path::Path::new(golden_path).exists() {
        image::save_buffer(golden_path, &pixels, w, h, image::ColorType::Rgba8).unwrap();
        eprintln!("wrote golden {golden_path}");
        return;
    }
    let golden = image::open(golden_path).unwrap().to_rgba8();
    assert_eq!(golden.dimensions(), (w, h));
    assert!(
        common::max_abs_diff(&pixels, golden.as_raw()) <= TOL,
        "rendered output drifted from golden beyond tolerance"
    );
}

#[test]
fn rung2_tiled_matches_single_texture() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // A larger gradient so multiple tiles exist.
    let (iw, ih) = (300u32, 200u32);
    let mut px = Vec::new();
    for y in 0..ih {
        for x in 0..iw {
            px.extend_from_slice(&[x as f32 / iw as f32, y as f32 / ih as f32, 0.25, 1.0]);
        }
    }
    let img = ferrolite_image::LinearRgbaF32::new(iw, ih, px).unwrap();
    let (w, h) = (128u32, 128u32);
    let view = ViewTransform::fit((iw, ih), (w as f32, h as f32));

    let single = VirtualTexture::render_to_image(&ctx, &img, &view, (w as f32, h as f32), w, h);
    let src = ferrolite_vt::PyramidTileSource::new(img);
    let tiled =
        VirtualTexture::render_tiled_to_image(&ctx, &src, &view, (w as f32, h as f32), w, h);

    // At fit zoom the tiled path samples a coarse LOD; allow a generous tolerance
    // vs the single-texture reference (different filtering), but they must broadly agree.
    let diff = common::max_abs_diff(&single, &tiled);
    eprintln!("rung2 max_abs_diff = {diff}");
    assert!(diff <= 24, "tiled diverges from single-texture reference");
}
