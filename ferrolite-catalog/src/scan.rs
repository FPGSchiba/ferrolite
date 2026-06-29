//! Filesystem scan: enumerate RAW files in a folder with their stat info.
//! No DB access — reusable by the synchronous `ingest_folder` and by the app's
//! job-driven ingest.

use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// RAW extensions we ingest (lowercased). Extend as camera coverage grows.
const RAW_EXTS: &[&str] = &[
    "nef", "nrw", "cr2", "cr3", "crw", "arw", "sr2", "srf", "raf", "rw2", "orf", "pef", "dng",
    "raw", "rwl", "iiq", "3fr", "erf", "mef", "mos", "kdc", "dcr",
];

pub fn is_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| RAW_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// One RAW file with the stat fields the catalog keys incremental rescan on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFile {
    pub path: PathBuf,
    pub filename: String,
    pub mtime: i64,
    pub size: i64,
}

/// Enumerate RAW files directly in `folder` (depth 1, like the synchronous path).
pub fn scan_raw_files(folder: &Path) -> Vec<RawFile> {
    let mut out = Vec::new();
    for entry in WalkDir::new(folder)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        if !p.is_file() || !is_raw(p) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(p) else {
            continue;
        };
        let size = meta.len() as i64;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(RawFile {
            path: p.to_path_buf(),
            filename: entry.file_name().to_string_lossy().to_string(),
            mtime,
            size,
        });
    }
    out
}
