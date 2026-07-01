//! Encode → decode round-trips per format, and an ICC-present check for a format
//! that embeds it (PNG). CPU-only; no GPU.

use ferrolite_export::{ExportFormat, ExportOptions, PixelData};

// Small helper: build a RenderedImage directly (bypass GPU) via a public shim.
// render.rs types are public, so construct one here for encode tests.
fn solid_rgb8(w: u32, h: u32, rgb: [u8; 3]) -> ferrolite_export::RenderedImage {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..(w * h) {
        v.extend_from_slice(&rgb);
    }
    ferrolite_export::RenderedImage {
        width: w,
        height: h,
        data: PixelData::Eight(v),
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ferrolite-export-test-{name}"))
}

#[test]
fn roundtrip_each_format_within_tolerance() {
    let img = solid_rgb8(32, 24, [200, 100, 40]);
    for (fmt, ext) in [
        (ExportFormat::Jpeg, "jpg"),
        (ExportFormat::Png, "png"),
        (ExportFormat::Tiff, "tif"),
        (ExportFormat::WebP, "webp"),
    ] {
        let dest = tmp(&format!("rt.{ext}"));
        let opts = ExportOptions {
            format: fmt,
            embed_icc: false, // isolate pixel round-trip from ICC
            ..Default::default()
        };
        // encode via the crate's public API path: reuse encode by exporting it.
        ferrolite_export::encode_for_test(&img, &opts, &dest).expect("encode");
        let decoded = image::open(&dest).expect("decode").to_rgb8();
        assert_eq!(decoded.dimensions(), (32, 24), "{fmt:?} dims");
        // JPEG is lossy; allow a wide tolerance. Others lossless.
        let tol = if matches!(fmt, ExportFormat::Jpeg) {
            12
        } else {
            0
        };
        let p = decoded.get_pixel(4, 4).0;
        let expected: [i32; 3] = [200, 100, 40];
        for c in 0..3 {
            assert!(
                (p[c] as i32 - expected[c]).abs() <= tol,
                "{fmt:?} ch {c}: {} vs {}",
                p[c],
                expected[c]
            );
        }
        let _ = std::fs::remove_file(&dest);
    }
}

#[test]
fn png_embeds_icc_profile() {
    let img = solid_rgb8(16, 16, [128, 128, 128]);
    let dest = tmp("icc.png");
    let opts = ExportOptions {
        format: ExportFormat::Png,
        output_space: ferrolite_color::WorkingSpace::Srgb,
        embed_icc: true,
        ..Default::default()
    };
    ferrolite_export::encode_for_test(&img, &opts, &dest).expect("encode");

    // Reopen with the PNG decoder and read the ICC chunk back.
    use image::ImageDecoder;
    let file = std::fs::File::open(&dest).unwrap();
    let mut dec = image::codecs::png::PngDecoder::new(std::io::BufReader::new(file)).unwrap();
    let icc = dec.icc_profile().unwrap();
    assert!(
        icc.is_some_and(|p| !p.is_empty()),
        "PNG should carry an ICC profile"
    );
    let _ = std::fs::remove_file(&dest);
}
