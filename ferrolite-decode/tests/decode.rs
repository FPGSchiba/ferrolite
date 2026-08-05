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
            // Require a real file extension that isn't a known sidecar/export
            // type. Extensionless files (e.g. `.gitignore`, which lives in this
            // dir) must be skipped: `read_dir` order is unspecified, and on
            // Windows `.gitignore` sorted first and got fed to the decoder,
            // failing the whole suite with "No decoder found".
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| !NON_RAW.contains(&e.to_ascii_lowercase().as_str()))
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
fn focal_length_35mm_defaults_none_when_absent() {
    // The RAW route (rawler-derived) does not expose FocalLengthIn35mmFilm, so
    // it must always report `None` — never a fabricated value.
    let meta = ferrolite_decode::read_metadata(&fixture(), FileKind::Raw).expect("metadata");
    assert!(
        meta.focal_length_35mm.is_none(),
        "RAW route does not populate focal_length_35mm"
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
fn decode_full_crops_within_metadata_dimensions_and_buffer() {
    let meta = ferrolite_decode::read_metadata(&fixture(), FileKind::Raw).expect("metadata");
    let full = ferrolite_decode::decode_full(&fixture()).expect("full decode");
    // `decode_full` crops to the camera's recommended image rectangle
    // (`crop_area`/`active_area`), dropping the masked/optically-black sensor
    // border, so its dims are <= the full-sensor dims `read_metadata` reports.
    // (They were equal before the active-area crop was applied.)
    assert!(full.width > 0 && full.width <= meta.width);
    assert!(full.height > 0 && full.height <= meta.height);
    assert!(full.cpp >= 1);
    assert_eq!(
        full.pixels.len(),
        full.width as usize * full.height as usize * full.cpp
    );
}

/// A RAW that makes `rawler` PANIC must surface as a `DecodeError`, never as an
/// unwind escaping into the caller.
///
/// This is not hypothetical tidiness: every decode runs inside a
/// `ferrolite-jobs` worker (CLAUDE.md's threading rule), so an unguarded panic
/// kills that worker on ingest. Canon sRAW1/mRAW stores YCbCr rather than a CFA
/// mosaic and trips `assertion failed: self.initialized` in rawler 0.7.2's
/// `pixarray.rs`. Any user with such a file in their library hits it.
///
/// Fixture-gated (see `fixtures/raw-broken/README.md`); skips when absent.
#[test]
fn a_decoder_panic_becomes_an_error_not_an_unwind() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures/raw-broken/nolens-canon-eos60d-iso100-50mm-mraw.CR2");
    if !path.exists() {
        eprintln!("no sRAW fixture; skipping decoder-panic containment test");
        return;
    }

    // The two entry points that reach rawler's CFA path must contain the panic.
    let e = ferrolite_decode::read_metadata(&path, FileKind::Raw)
        .expect_err("rawler panics on this file; read_metadata must return Err");
    assert!(
        matches!(e, ferrolite_decode::DecodeError::DecoderPanicked(_)),
        "expected DecoderPanicked, got {e}"
    );

    // `let ... else`, NOT `expect_err`: this call's Ok type contains an
    // `ImageBuffer`, and `expect_err` Debug-prints the Ok value on failure —
    // every pixel, a 240 MB test log (hit while writing this test). Since the
    // whole point of this assertion is to START failing when rawler fixes the
    // bug, the failure path is the one that must stay readable.
    let Err(e) = ferrolite_decode::decode_meta_and_preview(&path, FileKind::Raw, false, 256) else {
        panic!("decode_meta_and_preview must return Err while rawler panics on sRAW");
    };
    assert!(
        matches!(e, ferrolite_decode::DecodeError::DecoderPanicked(_)),
        "expected DecoderPanicked, got {e}"
    );

    // `decode_preview` is DIFFERENT and must keep working: it extracts the
    // embedded JPEG, which needs no CFA decode, so it never reaches the
    // assertion. Asserted rather than left implicit because it is the reason a
    // blanket "sRAW is unsupported" rejection would be wrong — the thumbnail is
    // perfectly readable, only the raw-image path is not.
    let preview = ferrolite_decode::decode_preview(&path, FileKind::Raw)
        .map_err(|e| format!("{e}"))
        .expect("embedded preview must still decode");
    assert!(
        preview.width > 0 && preview.height > 0,
        "preview must be non-empty"
    );
}

/// The Olympus ORF fixture fails cleanly with `NoPreview` — no panic, no hang.
///
/// Pinned so the day a rawler release starts extracting a preview from it, this
/// test fails and tells us to move the file back into `fixtures/raw/` and claim
/// its coverage slot. Fixture-gated; skips when absent.
#[test]
fn the_orf_fixture_still_fails_with_a_clean_nopreview() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures/raw-broken/orf-olympus-em1mk2-iso400-28mm.ORF");
    if !path.exists() {
        eprintln!("no ORF fixture; skipping");
        return;
    }
    let e = ferrolite_decode::decode_meta_and_preview(&path, FileKind::Raw, false, 256)
        .expect_err("this ORF has no rawler-extractable preview");
    assert!(
        matches!(e, ferrolite_decode::DecodeError::NoPreview(_)),
        "expected a clean NoPreview (not a panic), got {e}"
    );
}
