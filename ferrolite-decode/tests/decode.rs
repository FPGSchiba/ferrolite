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
    let meta = ferrolite_decode::read_metadata(&fixture()).expect("metadata");
    assert!(!meta.make.is_empty(), "make should be populated");
    assert!(!meta.model.is_empty(), "model should be populated");
    assert!(
        meta.width > 0 && meta.height > 0,
        "dimensions should be > 0"
    );
}
