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

fn solid_rgb16(w: u32, h: u32, rgb: [u16; 3]) -> ferrolite_export::RenderedImage {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..(w * h) {
        v.extend_from_slice(&rgb);
    }
    ferrolite_export::RenderedImage {
        width: w,
        height: h,
        data: ferrolite_export::PixelData::Sixteen(v),
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ferrolite-export-test-{name}"))
}

#[test]
fn avif_writes_a_wellformed_container() {
    // Decoding AVIF would pull dav1d (a C toolchain); assert container structure
    // instead: a non-empty ISOBMFF file whose ftyp box carries the `avif` brand.
    let img = solid_rgb8(32, 24, [200, 100, 40]);
    let dest = tmp("rt.avif");
    let opts = ExportOptions {
        format: ExportFormat::Avif,
        embed_icc: false,
        ..Default::default()
    };
    ferrolite_export::encode_for_test(&img, &opts, &dest).expect("encode avif");
    let bytes = std::fs::read(&dest).expect("read avif");
    assert!(bytes.len() > 32, "avif file should be non-trivial");
    // Bytes 4..8 = box type "ftyp"; the major brand or a compatible brand is "avif".
    assert_eq!(&bytes[4..8], b"ftyp", "first box should be ftyp");
    assert!(
        bytes.windows(4).take(64).any(|w| w == b"avif"),
        "ftyp should advertise the avif brand"
    );
    let _ = std::fs::remove_file(&dest);
}

#[test]
fn jpegxl_lossless_roundtrips_8_and_16_bit() {
    for (name, img, expect8) in [
        ("jxl8", solid_rgb8(32, 24, [200, 100, 40]), true),
        ("jxl16", solid_rgb16(32, 24, [50000, 25000, 1000]), false),
    ] {
        let depth = if expect8 {
            ferrolite_export::BitDepth::Eight
        } else {
            ferrolite_export::BitDepth::Sixteen
        };
        let dest = tmp(&format!("{name}.jxl"));
        let opts = ExportOptions {
            format: ExportFormat::JpegXl,
            bit_depth: depth,
            embed_icc: false,
            ..Default::default()
        };
        ferrolite_export::encode_for_test(&img, &opts, &dest).expect("encode jxl");

        // Decode with jxl-oxide (pure Rust, already in-tree). render_frame(0)
        // yields interleaved f32 samples in [0,1]; scale to the source depth and
        // compare a center pixel exactly (lossless).
        let bytes = std::fs::read(&dest).unwrap();
        let jxl = jxl_oxide::JxlImage::builder()
            .read(std::io::Cursor::new(bytes))
            .expect("parse jxl");
        let render = jxl.render_frame(0).expect("render jxl");
        let fb = render.image_all_channels();
        let ch = fb.channels();
        assert!(ch >= 3, "expected >=3 channels, got {ch}");
        let buf = fb.buf(); // &[f32], interleaved, len = w*h*ch
        let (w, _h) = (32usize, 24usize);
        let idx = ((4 * w) + 4) * ch; // center-ish pixel (4,4)
        if expect8 {
            let px = [
                (buf[idx] * 255.0).round() as u16,
                (buf[idx + 1] * 255.0).round() as u16,
                (buf[idx + 2] * 255.0).round() as u16,
            ];
            assert_eq!(px, [200, 100, 40], "{name} 8-bit lossless");
        } else {
            let px = [
                (buf[idx] * 65535.0).round() as i32,
                (buf[idx + 1] * 65535.0).round() as i32,
                (buf[idx + 2] * 65535.0).round() as i32,
            ];
            for (c, exp) in [50000, 25000, 1000].iter().enumerate() {
                assert!(
                    (px[c] - *exp).abs() <= 1,
                    "{name} 16-bit ch {c}: {} vs {}",
                    px[c],
                    exp
                );
            }
        }
        let _ = std::fs::remove_file(&dest);
    }
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
