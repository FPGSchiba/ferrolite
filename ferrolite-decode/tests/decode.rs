use ferrolite_image::FileKind;
use std::path::{Path, PathBuf};

/// First RAW file in the shared fixture directory. Sidecars and exported
/// images (`.xmp`, `.jpg`, `.png`, …) that may land in the dir during manual
/// testing are skipped so a stray file cannot hijack the fixture selection.
fn fixture() -> PathBuf {
    const NON_RAW: &[&str] = &["xmp", "jpg", "jpeg", "png", "tif", "tiff", "webp", "avif"];
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/raw");
    std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.is_file()
                && !p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| NON_RAW.contains(&e.to_ascii_lowercase().as_str()))
                    .unwrap_or(false)
        })
        .expect("a RAW fixture in fixtures/raw")
}

#[test]
fn read_metadata_returns_camera_and_dimensions() {
    let meta = ferrolite_decode::read_metadata(&fixture(), FileKind::Raw).expect("metadata");
    assert!(!meta.make.is_empty(), "make should be populated");
    assert!(!meta.model.is_empty(), "model should be populated");
    assert!(
        meta.width > 0 && meta.height > 0,
        "dimensions should be > 0"
    );
}

#[test]
fn decode_preview_returns_nonempty_rgb8() {
    use ferrolite_image::PixelFormat;
    let buf = ferrolite_decode::decode_preview(&fixture(), FileKind::Raw).expect("preview");
    assert_eq!(buf.format, PixelFormat::Rgb8);
    assert!(buf.width > 0 && buf.height > 0);
    assert_eq!(
        buf.pixels.len(),
        buf.width as usize * buf.height as usize * 3
    );
}

#[test]
fn combined_matches_separate_paths() {
    let (m, p, _info) =
        ferrolite_decode::decode_meta_and_preview(&fixture(), FileKind::Raw, false, 256)
            .expect("combined");
    let m2 = ferrolite_decode::read_metadata(&fixture(), FileKind::Raw).expect("metadata");
    let p2 = ferrolite_decode::decode_preview(&fixture(), FileKind::Raw).expect("preview");
    assert_eq!(
        m, m2,
        "combined metadata should match separate read_metadata"
    );
    assert_eq!((p.width, p.height), (p2.width, p2.height));
    assert_eq!(p.pixels, p2.pixels, "preview pixels should be identical");
}

#[test]
fn preview_info_reports_dims_and_gated_timings() {
    use ferrolite_decode::PreviewSource;
    // measure = true: dims populated, sub-timings present, source is a RAW branch.
    let (_m, p, info) =
        ferrolite_decode::decode_meta_and_preview(&fixture(), FileKind::Raw, true, 256)
            .expect("measured");
    assert!(
        info.src_w > 0 && info.src_h > 0,
        "embedded dims should be > 0"
    );
    assert!(
        (info.src_w == p.width && info.src_h == p.height)
            || (info.src_w == p.height && info.src_h == p.width),
        "embedded dims match the buffer up to a 90-degree orientation swap"
    );
    assert!(
        info.extract.is_some() && info.orient.is_some(),
        "measured => Some timings"
    );
    // No rawler 0.7.2 decoder implements `preview_image`, so a RAW preview must
    // come from the full-resolution embedded image or the embedded thumbnail —
    // never the `preview_image` branch. This is the spec's core mechanism claim.
    assert_ne!(
        info.source,
        PreviewSource::EmbeddedPreview,
        "RAW preview must not use the (unimplemented) preview_image branch"
    );

    // measure = false: no timings recorded.
    let (_m2, _p2, info2) =
        ferrolite_decode::decode_meta_and_preview(&fixture(), FileKind::Raw, false, 256)
            .expect("unmeasured");
    assert!(
        info2.extract.is_none() && info2.orient.is_none(),
        "unmeasured => None timings"
    );
}

#[test]
fn decode_full_matches_metadata_dimensions_and_buffer() {
    let meta = ferrolite_decode::read_metadata(&fixture(), FileKind::Raw).expect("metadata");
    let full = ferrolite_decode::decode_full(&fixture()).expect("full decode");
    assert_eq!(full.width, meta.width);
    assert_eq!(full.height, meta.height);
    assert!(full.cpp >= 1);
    assert_eq!(
        full.pixels.len(),
        full.width as usize * full.height as usize * full.cpp
    );
}
