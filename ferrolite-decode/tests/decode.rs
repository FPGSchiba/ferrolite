use ferrolite_image::FileKind;
use std::path::{Path, PathBuf};

/// First file in the shared fixture directory (extension-agnostic).
fn fixture() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/raw");
    std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.is_file())
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
