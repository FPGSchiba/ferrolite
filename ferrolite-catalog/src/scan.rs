//! Filesystem scan: enumerate supported image files with their stat info.
//! No DB access — reusable by the synchronous `ingest_folder` and by the app's
//! job-driven ingest.

use ferrolite_image::FileKind;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// RAW extensions we ingest (lowercased). Extend as camera coverage grows.
const RAW_EXTS: &[&str] = &[
    "nef", "nrw", "cr2", "cr3", "crw", "arw", "sr2", "srf", "raf", "rw2", "orf", "pef", "dng",
    "raw", "rwl", "iiq", "3fr", "erf", "mef", "mos", "kdc", "dcr", "srw", "x3f", "gpr", "fff",
    "cap", "rwz", "bay", "cs1", "ari", "dcs",
];

/// Standard raster extensions decoded via the `image` crate (lowercased).
const STANDARD_EXTS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp", "bmp", "gif"];

fn ext_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

pub fn is_raw(path: &Path) -> bool {
    ext_lower(path)
        .map(|e| RAW_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

/// Classify a path as a RAW or standard raster, or `None` if unsupported.
pub fn classify(path: &Path) -> Option<FileKind> {
    let e = ext_lower(path)?;
    if RAW_EXTS.contains(&e.as_str()) {
        Some(FileKind::Raw)
    } else if STANDARD_EXTS.contains(&e.as_str()) {
        Some(FileKind::Standard)
    } else {
        None
    }
}

/// One supported file with the stat fields the catalog keys incremental rescan
/// on, plus its classified kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub filename: String,
    pub mtime: i64,
    pub size: i64,
    pub kind: FileKind,
}

fn stat(path: &Path) -> Option<(i64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    let size = meta.len() as i64;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((mtime, size))
}

fn to_scanned(path: &Path, kind: FileKind) -> Option<ScannedFile> {
    let (mtime, size) = stat(path)?;
    Some(ScannedFile {
        path: path.to_path_buf(),
        filename: path.file_name()?.to_string_lossy().to_string(),
        mtime,
        size,
        kind,
    })
}

/// Enumerate RAW files directly in `folder` (depth 1). Kept for the headless
/// benchmark and back-compat; all results have `kind == Raw`.
pub fn scan_raw_files(folder: &Path) -> Vec<ScannedFile> {
    WalkDir::new(folder)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file() && is_raw(e.path()))
        .filter_map(|e| to_scanned(e.path(), FileKind::Raw))
        .collect()
}

/// Recursively enumerate every supported image in the subtree rooted at `root`,
/// each tagged with its classified `FileKind`.
pub fn scan_tree(root: &Path) -> Vec<ScannedFile> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| classify(e.path()).and_then(|k| to_scanned(e.path(), k)))
        .collect()
}

/// All directories that must become `folders` rows: every file's parent plus
/// its ancestors up to (and including) `root`, deduplicated and ordered
/// parent-first (so a parent row exists before its child upserts).
pub fn collect_dirs(files: &[ScannedFile], root: &Path) -> Vec<PathBuf> {
    let mut set: BTreeSet<PathBuf> = BTreeSet::new();
    set.insert(root.to_path_buf());
    for f in files {
        let mut cur = f.path.parent();
        while let Some(dir) = cur {
            set.insert(dir.to_path_buf());
            if dir == root {
                break;
            }
            cur = dir.parent();
        }
    }
    let mut dirs: Vec<PathBuf> = set.into_iter().collect();
    dirs.sort_by_key(|p| p.components().count());
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn classify_recognizes_raw_standard_and_skips_others() {
        assert_eq!(classify(Path::new("a.NEF")), Some(FileKind::Raw));
        assert_eq!(classify(Path::new("a.cr3")), Some(FileKind::Raw));
        assert_eq!(classify(Path::new("b.JPG")), Some(FileKind::Standard));
        assert_eq!(classify(Path::new("b.png")), Some(FileKind::Standard));
        assert_eq!(classify(Path::new("c.txt")), None);
        assert_eq!(classify(Path::new("noext")), None);
    }

    #[test]
    fn scan_tree_walks_subfolders_and_tags_kind() {
        let root = std::env::temp_dir().join(format!("ferro-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("top.NEF"), b"x").unwrap();
        fs::write(root.join("sub").join("nested.jpg"), b"x").unwrap();
        fs::write(root.join("sub").join("note.txt"), b"x").unwrap();

        let mut files = scan_tree(&root);
        files.sort_by(|a, b| a.filename.cmp(&b.filename));
        assert_eq!(files.len(), 2, "txt skipped, two images found");
        assert_eq!(files[0].filename, "nested.jpg");
        assert_eq!(files[0].kind, FileKind::Standard);
        assert_eq!(files[1].filename, "top.NEF");
        assert_eq!(files[1].kind, FileKind::Raw);

        let dirs = collect_dirs(&files, &root);
        assert!(dirs.contains(&root));
        assert!(dirs.contains(&root.join("sub")));
        // Parent-first ordering: root precedes its child.
        let root_pos = dirs.iter().position(|d| d == &root).unwrap();
        let sub_pos = dirs.iter().position(|d| d == &root.join("sub")).unwrap();
        assert!(root_pos < sub_pos);

        let _ = fs::remove_dir_all(&root);
    }
}
