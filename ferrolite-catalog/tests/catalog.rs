use ferrolite_catalog::{Catalog, DecodeStatus, FileKind, NewImage, Rating};
use ferrolite_image::Orientation;

#[test]
fn fresh_db_is_migrated_to_current_version() {
    let cat = Catalog::open_in_memory().expect("open");
    assert_eq!(
        cat.schema_version().expect("version"),
        ferrolite_catalog::SCHEMA_VERSION
    );
}

#[test]
fn migrate_is_idempotent_on_reopen() {
    let dir = tempdir();
    let path = dir.join("catalog.db");
    {
        let _ = Catalog::open(&path).expect("first open");
    }
    // Reopening an already-migrated DB must not error or downgrade.
    let cat = Catalog::open(&path).expect("second open");
    assert_eq!(
        cat.schema_version().expect("version"),
        ferrolite_catalog::SCHEMA_VERSION
    );
}

fn sample_image(folder_id: i64, filename: &str) -> NewImage {
    NewImage {
        folder_id,
        filename: filename.to_string(),
        mtime: 1000,
        size: 2000,
        make: Some("Nikon".into()),
        model: Some("Z f".into()),
        width: Some(6048),
        height: Some(4032),
        orientation: Orientation::Rotate90,
        capture_time: Some("2026:06:29 12:00:00".into()),
        iso: Some(100),
        decode_status: DecodeStatus::Done,
        kind: FileKind::Raw,
        rating: Rating::default(),
        added_at: 0,
    }
}

#[test]
fn upsert_and_query_round_trip() {
    let cat = Catalog::open_in_memory().unwrap();
    let folder = cat
        .upsert_folder(std::path::Path::new("/photos/a"), None)
        .unwrap();
    let id = cat
        .upsert_image(&sample_image(folder, "DSC_0001.NEF"))
        .unwrap();

    let rec = cat
        .image_by_name(folder, "DSC_0001.NEF")
        .unwrap()
        .expect("row");
    assert_eq!(rec.id, id);
    assert_eq!(rec.width, Some(6048));
    assert_eq!(rec.orientation, Orientation::Rotate90);
    assert_eq!(rec.decode_status, DecodeStatus::Done);

    assert_eq!(cat.list_images(folder).unwrap().len(), 1);
    assert_eq!(cat.image_count().unwrap(), 1);
}

#[test]
fn upsert_is_idempotent_on_folder_and_filename() {
    let cat = Catalog::open_in_memory().unwrap();
    let folder = cat
        .upsert_folder(std::path::Path::new("/photos/a"), None)
        .unwrap();
    assert_eq!(
        folder,
        cat.upsert_folder(std::path::Path::new("/photos/a"), None)
            .unwrap()
    );

    let first = cat
        .upsert_image(&sample_image(folder, "DSC_0001.NEF"))
        .unwrap();
    let second = cat
        .upsert_image(&sample_image(folder, "DSC_0001.NEF"))
        .unwrap();
    assert_eq!(
        first, second,
        "same (folder, filename) updates the same row"
    );
    assert_eq!(cat.image_count().unwrap(), 1);
}

#[test]
fn needs_reingest_detects_changes() {
    let cat = Catalog::open_in_memory().unwrap();
    let folder = cat
        .upsert_folder(std::path::Path::new("/photos/a"), None)
        .unwrap();
    cat.upsert_image(&sample_image(folder, "DSC_0001.NEF"))
        .unwrap();

    assert!(
        !cat.needs_reingest(folder, "DSC_0001.NEF", 1000, 2000)
            .unwrap(),
        "unchanged"
    );
    assert!(
        cat.needs_reingest(folder, "DSC_0001.NEF", 1001, 2000)
            .unwrap(),
        "mtime changed"
    );
    assert!(
        cat.needs_reingest(folder, "DSC_0001.NEF", 1000, 9999)
            .unwrap(),
        "size changed"
    );
    assert!(
        cat.needs_reingest(folder, "NEW.NEF", 1, 1).unwrap(),
        "new file"
    );
}

use ferrolite_catalog::{generate_thumbnail, ThumbnailStore, THUMB_MAX_EDGE};
use ferrolite_image::{ImageBuffer, PixelFormat};

fn solid_rgb(width: u32, height: u32) -> ImageBuffer {
    let pixels = vec![120u8; (width * height * 3) as usize];
    ImageBuffer::new(width, height, PixelFormat::Rgb8, pixels).unwrap()
}

#[test]
fn generate_thumbnail_fits_within_max_edge_and_is_decodable_jpeg() {
    let thumb = generate_thumbnail(&solid_rgb(1024, 512)).expect("thumb");
    assert!(thumb.width <= THUMB_MAX_EDGE && thumb.height <= THUMB_MAX_EDGE);
    assert_eq!(thumb.format, "jpeg");
    // Aspect ratio preserved: 2:1 source → wider than tall.
    assert!(thumb.width > thumb.height);
    // Bytes decode as a JPEG of the reported size.
    let decoded = image::load_from_memory(&thumb.bytes)
        .expect("decodes")
        .to_rgb8();
    assert_eq!(decoded.width(), thumb.width);
    assert_eq!(decoded.height(), thumb.height);
}

#[test]
fn thumbnail_store_blob_round_trip() {
    let cat = Catalog::open_in_memory().unwrap();
    let folder = cat
        .upsert_folder(std::path::Path::new("/photos/a"), None)
        .unwrap();
    let id = cat
        .upsert_image(&sample_image(folder, "DSC_0001.NEF"))
        .unwrap();

    let thumb = generate_thumbnail(&solid_rgb(640, 480)).unwrap();
    cat.put_thumbnail(id, &thumb).unwrap();

    let got = cat.get_thumbnail(id).unwrap().expect("stored thumb");
    assert_eq!(got.width, thumb.width);
    assert_eq!(got.height, thumb.height);
    assert_eq!(got.format, "jpeg");
    assert_eq!(got.bytes, thumb.bytes);
    assert!(
        cat.get_thumbnail(999_999).unwrap().is_none(),
        "missing → None"
    );
}

/// Path to the shared fixture folder (contains the CC0 RAW from Task 2).
fn fixture_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/raw")
}

#[test]
fn ingest_folder_indexes_images_and_thumbnails() {
    let dir = tempdir();
    let cat = Catalog::open(&dir.join("catalog.db")).unwrap();

    let summary = cat.ingest_folder(&fixture_dir()).expect("ingest");
    assert!(summary.scanned >= 1, "should scan the fixture RAW");
    assert!(summary.added >= 1, "should add at least one image");
    assert_eq!(summary.failed, 0, "fixture must decode cleanly");
    assert!(cat.image_count().unwrap() >= 1);

    // Every indexed image has a decodable thumbnail within the size cap.
    let folder = cat.upsert_folder(&fixture_dir(), None).unwrap();
    let images = cat.list_images(folder).unwrap();
    assert!(!images.is_empty());
    for rec in images {
        let thumb = cat
            .get_thumbnail(rec.id)
            .unwrap()
            .expect("thumbnail present");
        assert!(thumb.width <= 256 && thumb.height <= 256);
        image::load_from_memory(&thumb.bytes).expect("thumb decodes");
    }
}

#[test]
fn second_ingest_skips_unchanged_files() {
    let dir = tempdir();
    let cat = Catalog::open(&dir.join("catalog.db")).unwrap();

    let first = cat.ingest_folder(&fixture_dir()).unwrap();
    assert!(first.added >= 1);

    let second = cat.ingest_folder(&fixture_dir()).unwrap();
    assert_eq!(second.added, 0, "nothing changed → no adds");
    assert_eq!(
        second.skipped, first.added,
        "all previously-added files skipped"
    );
}

#[test]
fn kind_round_trips_and_schema_is_v2() {
    use ferrolite_catalog::FileKind;
    let cat = ferrolite_catalog::Catalog::open_in_memory().unwrap();
    assert_eq!(cat.schema_version().unwrap(), 3);
    let folder = cat
        .upsert_folder(std::path::Path::new("/photos/a"), None)
        .unwrap();
    let raw = ferrolite_catalog::NewImage::failed(folder, "r.nef".into(), 1, 1, FileKind::Raw, 0);
    let std_ =
        ferrolite_catalog::NewImage::failed(folder, "s.jpg".into(), 1, 1, FileKind::Standard, 0);
    cat.upsert_image(&raw).unwrap();
    cat.upsert_image(&std_).unwrap();
    let mut rows = cat.list_images(folder).unwrap();
    rows.sort_by(|a, b| a.filename.cmp(&b.filename));
    assert_eq!(rows[0].kind, FileKind::Raw); // r.nef
    assert_eq!(rows[1].kind, FileKind::Standard); // s.jpg
}

#[test]
fn folder_path_round_trips() {
    let cat = ferrolite_catalog::Catalog::open_in_memory().unwrap();
    let id = cat
        .upsert_folder(std::path::Path::new("/photos/a"), None)
        .unwrap();
    assert_eq!(cat.folder_path(id).unwrap().as_deref(), Some("/photos/a"));
    assert_eq!(cat.folder_path(999_999).unwrap(), None);
}

/// Minimal temp dir without an extra dependency: unique path under the OS temp
/// dir using the test thread name + a process-unique counter.
fn tempdir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("ferrolite-cat-{pid}-{n}"));
    std::fs::create_dir_all(&dir).expect("mkdir temp");
    dir
}
