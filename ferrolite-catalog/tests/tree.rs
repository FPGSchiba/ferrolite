//! Recursive ingest tree: parent wiring, per-directory keying, recursive list,
//! and subtree removal. Uses tiny generated PNGs (no rawler needed).

use ferrolite_catalog::{Catalog, ReadPool};
use std::path::PathBuf;

fn make_png(path: &std::path::Path) {
    let img = image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3]));
    img.save(path).unwrap();
}

fn nested_fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ferro-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("2024")).unwrap();
    std::fs::create_dir_all(root.join("2025")).unwrap();
    // Same filename in two sibling subfolders — must NOT collide.
    make_png(&root.join("2024").join("IMG_001.png"));
    make_png(&root.join("2025").join("IMG_001.png"));
    make_png(&root.join("top.png"));
    root
}

#[test]
fn recursive_ingest_wires_tree_and_keys_per_directory() {
    let root = nested_fixture();
    let db = std::env::temp_dir().join(format!("ferro-tree-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let cat = Catalog::open(&db).unwrap();

    let summary = cat.ingest_folder(&root).unwrap();
    assert_eq!(summary.added, 3, "two nested + one top-level");

    let reads = ReadPool::open(&db, 1).unwrap();
    let folders = reads.list_folders().unwrap();
    // root + 2024 + 2025 = 3 folder rows.
    assert_eq!(folders.len(), 3);
    let root_row = folders.iter().find(|f| f.parent_id.is_none()).unwrap();
    let children: Vec<_> = folders
        .iter()
        .filter(|f| f.parent_id == Some(root_row.id))
        .collect();
    assert_eq!(children.len(), 2, "2024 and 2025 are children of root");

    // Duplicate filename in two folders both ingested.
    let total: u64 = folders.iter().map(|f| f.image_count).sum();
    assert_eq!(total, 3);

    // Recursive list over root returns the whole subtree; direct returns 1.
    let recursive = reads.list_images_recursive(root_row.id).unwrap();
    assert_eq!(recursive.len(), 3);
    let direct = reads.list_images(root_row.id).unwrap();
    assert_eq!(direct.len(), 1, "only top.png is directly in root");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn remove_folder_deletes_subtree_only() {
    let root = nested_fixture();
    let db = std::env::temp_dir().join(format!("ferro-rm-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let cat = Catalog::open(&db).unwrap();
    cat.ingest_folder(&root).unwrap();

    let reads = ReadPool::open(&db, 1).unwrap();
    let folders = reads.list_folders().unwrap();
    let child_2024 = folders
        .iter()
        .find(|f| f.path.ends_with("2024"))
        .unwrap()
        .id;

    cat.remove_folder(child_2024).unwrap();

    let after = reads.list_folders().unwrap();
    assert_eq!(after.len(), 2, "2024 removed; root + 2025 remain");
    assert!(after.iter().all(|f| !f.path.ends_with("2024")));
    // Its image is gone; total drops from 3 to 2.
    let total: u64 = after.iter().map(|f| f.image_count).sum();
    assert_eq!(total, 2);

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&db);
}
