//! Shared golden-test helpers (mirrors ferrolite-vt/tests/common). Golden PNGs
//! are authored on the dev GPU (set UPDATE_GOLDEN=1 or delete the fixture) and
//! committed; in headless CI the GPU tests skip before reaching these.

use ferrolite_image::LinearRgbaF32;

/// A deterministic RGB gradient used as the edit source.
pub fn gradient(w: u32, h: u32) -> LinearRgbaF32 {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[x as f32 / w as f32, y as f32 / h as f32, 0.25, 1.0]);
        }
    }
    LinearRgbaF32::new(w, h, px).expect("gradient length")
}

pub fn max_abs_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

const TOL: u8 = 4; // absorbs driver float differences

/// Compare `pixels` against `tests/fixtures/<name>`. Authors the golden if the
/// file is absent or UPDATE_GOLDEN is set (then returns, passing).
pub fn assert_golden(pixels: &[u8], w: u32, h: u32, name: &str) {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    if std::env::var("UPDATE_GOLDEN").is_ok() || !std::path::Path::new(&path).exists() {
        std::fs::create_dir_all(format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))).unwrap();
        image::save_buffer(&path, pixels, w, h, image::ColorType::Rgba8).unwrap();
        eprintln!("wrote golden {path}");
        return;
    }
    let golden = image::open(&path).unwrap().to_rgba8();
    assert_eq!(golden.dimensions(), (w, h), "golden dims mismatch: {name}");
    assert!(
        max_abs_diff(pixels, golden.as_raw()) <= TOL,
        "{name}: rendered output drifted from golden beyond tolerance"
    );
}
