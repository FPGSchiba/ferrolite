//! Recursive ingest tree: parent wiring, per-directory keying, recursive list,
//! and subtree removal. Uses tiny generated PNGs (no rawler needed).

use ferrolite_catalog::{Catalog, ReadPool};
use std::path::PathBuf;

fn make_png(path: &std::path::Path) {
    let img = image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3]));
    img.save(path).unwrap();
}

fn nested_fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ferro-tree-{}-{}", tag, std::process::id()));
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
    let root = nested_fixture("ingest");
    let db = std::env::temp_dir().join(format!("ferro-tree-ingest-{}.db", std::process::id()));
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
fn prune_subtree_deletes_absent_files_and_folders() {
    use std::collections::HashSet;
    let root = nested_fixture("prune");
    let db = std::env::temp_dir().join(format!("ferro-prune-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let cat = Catalog::open(&db).unwrap();
    cat.ingest_folder(&root).unwrap();

    let reads = ReadPool::open(&db, 1).unwrap();
    let folders = reads.list_folders().unwrap();
    let root_id = folders.iter().find(|f| f.parent_id.is_none()).unwrap().id;
    let folder_2025 = folders
        .iter()
        .find(|f| f.path.ends_with("2025"))
        .unwrap()
        .id;
    let all = reads.list_images_recursive(root_id).unwrap();
    assert_eq!(all.len(), 3);

    // Simulate a Full rescan where 2025 (folder + its image) vanished from disk
    // and one 2024 image was deleted: keep everything else.
    let drop_2024_img = all
        .iter()
        .find(|i| i.folder_id != root_id && i.folder_id != folder_2025)
        .map(|i| i.id)
        .unwrap();
    // The image inside 2025, whose whole folder vanishes (folder-level prune path).
    let drop_2025_img = all
        .iter()
        .find(|i| i.folder_id == folder_2025)
        .map(|i| i.id)
        .unwrap();
    let kept_folders: HashSet<i64> = folders
        .iter()
        .map(|f| f.id)
        .filter(|id| *id != folder_2025)
        .collect();
    let kept_images: HashSet<i64> = all
        .iter()
        .map(|i| i.id)
        .filter(|id| *id != drop_2024_img)
        .filter(|id| {
            // also drop 2025's image (its folder vanished)
            all.iter()
                .find(|i| i.id == *id)
                .map(|i| i.folder_id != folder_2025)
                .unwrap_or(false)
        })
        .collect();

    cat.prune_subtree(root_id, &kept_folders, &kept_images)
        .unwrap();

    let after_folders = reads.list_folders().unwrap();
    assert!(
        after_folders.iter().all(|f| f.id != folder_2025),
        "vanished folder pruned"
    );
    let after_images = reads.list_images_recursive(root_id).unwrap();
    assert!(
        after_images.iter().all(|i| i.id != drop_2024_img),
        "deleted file pruned"
    );
    assert_eq!(
        after_images.len(),
        1,
        "only the kept top-level image remains"
    );
    assert!(reads.get_thumbnail(drop_2024_img).unwrap().is_none());
    // Folder-level prune must also drop the thumbnail of an image in a vanished folder.
    assert!(
        reads.get_thumbnail(drop_2025_img).unwrap().is_none(),
        "thumbnail of an image in a pruned folder is removed"
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn remove_folder_deletes_subtree_only() {
    let root = nested_fixture("rm");
    let db = std::env::temp_dir().join(format!("ferro-tree-rm-{}.db", std::process::id()));
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
