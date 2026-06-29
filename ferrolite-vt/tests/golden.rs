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
