//! End-to-end: a nested, mixed-format folder ingests into a wired tree with
//! per-directory keying and a working thumbnail for a standard raster — all
//! headless (no egui), mirroring the interactive path's helpers.

use ferrolite_app::ingest::thumbnail_blocking;
use ferrolite_catalog::{collect_dirs, scan_tree, Catalog, FileKind, NewImage, Rating, ReadPool};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn make_png(path: &std::path::Path) {
    image::RgbImage::from_pixel(6, 4, image::Rgb([9, 9, 9]))
        .save(path)
        .unwrap();
}

#[test]
fn nested_mixed_ingest_builds_tree_and_thumbnails_standard() {
    let root = std::env::temp_dir().join(format!("ferro-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("a")).unwrap();
    make_png(&root.join("top.png"));
    make_png(&root.join("a").join("inner.jpeg"));

    let db = std::env::temp_dir().join(format!("ferro-e2e-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let writer: Arc<Mutex<Catalog>> = Arc::new(Mutex::new(Catalog::open(&db).unwrap()));

    let files = scan_tree(&root);
    assert_eq!(files.len(), 2);
    let mut dir_ids: HashMap<PathBuf, i64> = HashMap::new();
    for dir in collect_dirs(&files, &root) {
        let parent = dir.parent().and_then(|p| dir_ids.get(p).copied());
        let id = writer.lock().unwrap().upsert_folder(&dir, parent).unwrap();
        dir_ids.insert(dir, id);
    }
    let mut first_id = None;
    for f in &files {
        let folder_id = dir_ids[f.path.parent().unwrap()];
        let meta = ferrolite_decode::read_metadata(&f.path, f.kind).unwrap();
        let row = NewImage::from_metadata(
            folder_id,
            f.filename.clone(),
            f.mtime,
            f.size,
            &meta,
            f.kind,
            Rating::default(),
            0,
        );
        let id = writer.lock().unwrap().upsert_image(&row).unwrap();
        first_id.get_or_insert((id, f.path.clone(), f.kind));
    }

    // A standard-raster thumbnail decodes + persists.
    let (id, path, kind) = first_id.unwrap();
    assert_eq!(kind, FileKind::Standard);
    thumbnail_blocking(&writer, id, &path, kind).expect("thumbnail");

    let reads = ReadPool::open(&db, 1).unwrap();
    let root_id = reads
        .list_folders()
        .unwrap()
        .into_iter()
        .find(|f| f.parent_id.is_none())
        .unwrap()
        .id;
    assert_eq!(reads.list_images_recursive(root_id).unwrap().len(), 2);
    assert!(reads.get_thumbnail(id).unwrap().is_some());

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&db);
}
