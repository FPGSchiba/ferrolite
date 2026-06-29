use crate::catalog::Catalog;
use crate::error::CatalogError;
use crate::model::{IngestSummary, NewImage};
use crate::thumbnail::{generate_thumbnail, Thumbnail, ThumbnailStore};
use rayon::prelude::*;
use std::path::Path;

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

        let mut to_process: Vec<crate::ScannedFile> = Vec::new();
        for f in crate::scan_raw_files(path) {
            summary.scanned += 1;
            if self.needs_reingest(folder_id, &f.filename, f.mtime, f.size)? {
                to_process.push(f);
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
            .map(|f| Decoded {
                filename: f.filename.clone(),
                mtime: f.mtime,
                size: f.size,
                outcome: decode_one(&f.path, folder_id, &f.filename, f.mtime, f.size),
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
                    let failed = NewImage::failed(folder_id, d.filename, d.mtime, d.size);
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
    path: &std::path::Path,
    folder_id: i64,
    filename: &str,
    mtime: i64,
    size: i64,
) -> Result<(NewImage, Thumbnail), String> {
    // Opens the RAW more than once (metadata + preview); a single-open decode_all is deferred to Plan 4 (two-tier load).
    let meta = ferrolite_decode::read_metadata(path).map_err(|e| e.to_string())?;
    let preview = ferrolite_decode::decode_preview(path).map_err(|e| e.to_string())?;
    let thumb = generate_thumbnail(&preview).map_err(|e| e.to_string())?;
    let new_image = NewImage::from_metadata(folder_id, filename.to_string(), mtime, size, &meta);
    Ok((new_image, thumb))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
