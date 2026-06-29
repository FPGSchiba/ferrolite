use crate::catalog::Catalog;
use crate::error::CatalogError;
use crate::model::{DecodeStatus, IngestSummary, NewImage};
use crate::thumbnail::{generate_thumbnail, Thumbnail, ThumbnailStore};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// RAW extensions we ingest (lowercased). Extend as camera coverage grows.
const RAW_EXTS: &[&str] = &[
    "nef", "nrw", "cr2", "cr3", "crw", "arw", "sr2", "srf", "raf", "rw2", "orf", "pef", "dng",
    "raw", "rwl", "iiq", "3fr", "erf", "mef", "mos", "kdc", "dcr",
];

fn is_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| RAW_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// One file's CPU-heavy decode result, produced off the DB thread.
struct Decoded {
    filename: String,
    mtime: i64,
    size: i64,
    outcome: Result<(NewImage, Thumbnail), String>,
}

impl Catalog {
    /// Ingest a folder of RAWs (non-recursive into subfolders for this plan).
    /// New/changed files (by mtime+size) are decoded + thumbnailed in parallel;
    /// rows and thumbnail blobs are written serially (rusqlite Connection is
    /// single-threaded). Structured so Plan 3 can submit each file as a job.
    pub fn ingest_folder(&self, path: &Path) -> Result<IngestSummary, CatalogError> {
        let folder_id = self.upsert_folder(path)?;
        let mut summary = IngestSummary::default();

        // 1) Walk + stat (cheap, serial). Decide which files need (re)ingest.
        let mut to_process: Vec<(PathBuf, String, i64, i64)> = Vec::new();
        for entry in WalkDir::new(path)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() || !is_raw(p) {
                continue;
            }
            summary.scanned += 1;
            let filename = entry.file_name().to_string_lossy().to_string();
            let meta = std::fs::metadata(p)?;
            let size = meta.len() as i64;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if self.needs_reingest(folder_id, &filename, mtime, size)? {
                to_process.push((p.to_path_buf(), filename, mtime, size));
            } else {
                summary.skipped += 1;
            }
        }

        // 2) Decode + thumbnail in parallel (no DB access here).
        // folder_id is resolved up-front (upsert_folder above) and copied into each
        // parallel task; the NewImage is fully built off-thread, so no Catalog/DB
        // handle crosses a thread boundary.
        let decoded: Vec<Decoded> = to_process
            .into_par_iter()
            .map(|(path, filename, mtime, size)| Decoded {
                filename,
                mtime,
                size,
                outcome: decode_one(&path, folder_id, mtime, size),
            })
            .collect();

        // 3) Write rows + thumbnails serially.
        for d in decoded {
            match d.outcome {
                Ok((new_image, thumb)) => {
                    let id = self.upsert_image(&new_image)?;
                    self.put_thumbnail(id, &thumb)?;
                    summary.added += 1;
                }
                Err(msg) => {
                    eprintln!("ferrolite-catalog: decode failed for {}: {msg}", d.filename);
                    // Record a failed row so the grid shows a placeholder and we
                    // don't retry forever. One bad file never downs the pass.
                    let failed = NewImage {
                        folder_id,
                        filename: d.filename,
                        mtime: d.mtime,
                        size: d.size,
                        make: None,
                        model: None,
                        width: None,
                        height: None,
                        orientation: ferrolite_image::Orientation::Normal,
                        capture_time: None,
                        iso: None,
                        decode_status: DecodeStatus::Failed,
                    };
                    self.upsert_image(&failed)?;
                    summary.failed += 1;
                }
            }
        }

        self.conn().execute(
            "UPDATE folders SET last_scanned = ?1 WHERE id = ?2",
            rusqlite::params![now_secs(), folder_id],
        )?;
        Ok(summary)
    }
}

/// Decode one file into a (row, thumbnail) pair. Returns Err(message) on any
/// decode/thumbnail failure so the caller can mark the row Failed.
fn decode_one(
    path: &Path,
    folder_id: i64,
    mtime: i64,
    size: i64,
) -> Result<(NewImage, Thumbnail), String> {
    let meta = ferrolite_decode::read_metadata(path).map_err(|e| e.to_string())?;
    let preview = ferrolite_decode::decode_preview(path).map_err(|e| e.to_string())?;
    let thumb = generate_thumbnail(&preview).map_err(|e| e.to_string())?;
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let new_image = NewImage {
        folder_id,
        filename,
        mtime,
        size,
        make: Some(meta.make),
        model: Some(meta.model),
        width: Some(meta.width),
        height: Some(meta.height),
        orientation: meta.orientation,
        capture_time: meta.capture_time,
        iso: meta.iso,
        decode_status: DecodeStatus::Done,
    };
    Ok((new_image, thumb))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
