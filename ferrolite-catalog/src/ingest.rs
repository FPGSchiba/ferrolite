use crate::catalog::Catalog;
use crate::error::CatalogError;
use crate::model::{IngestSummary, NewImage};
use crate::thumbnail::{generate_thumbnail, Thumbnail, ThumbnailStore};
use crate::ScannedFile;
use ferrolite_image::FileKind;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct Decoded {
    folder_id: i64,
    filename: String,
    mtime: i64,
    size: i64,
    kind: FileKind,
    outcome: Result<(NewImage, Thumbnail), String>,
}

impl Catalog {
    /// Ingest a folder and **all its subfolders** (Model B). Every directory in
    /// the subtree becomes a `folders` row with `parent_id` wired; each image is
    /// keyed to its actual directory.
    pub fn ingest_folder(&self, path: &Path) -> Result<IngestSummary, CatalogError> {
        let mut summary = IngestSummary::default();
        let files = crate::scan_tree(path);

        // 1) Create folder rows top-down, wiring parent_id.
        let mut dir_ids: HashMap<PathBuf, i64> = HashMap::new();
        for dir in crate::collect_dirs(&files, path) {
            let parent_id = dir.parent().and_then(|p| dir_ids.get(p).copied());
            let id = self.upsert_folder(&dir, parent_id)?;
            dir_ids.insert(dir, id);
        }

        // 2) Decide which files need (re)ingest.
        let mut to_process: Vec<(&ScannedFile, i64)> = Vec::new();
        for f in &files {
            summary.scanned += 1;
            let folder_id = match f.path.parent().and_then(|p| dir_ids.get(p)) {
                Some(id) => *id,
                None => continue,
            };
            if self.needs_reingest(folder_id, &f.filename, f.mtime, f.size)? {
                to_process.push((f, folder_id));
            } else {
                summary.skipped += 1;
            }
        }

        // 3) Decode + thumbnail in parallel (no DB access).
        let decoded: Vec<Decoded> = to_process
            .into_par_iter()
            .map(|(f, folder_id)| Decoded {
                folder_id,
                filename: f.filename.clone(),
                mtime: f.mtime,
                size: f.size,
                kind: f.kind,
                outcome: decode_one(&f.path, folder_id, &f.filename, f.mtime, f.size, f.kind),
            })
            .collect();

        // 4) Write rows + thumbnails serially.
        for d in decoded {
            match d.outcome {
                Ok((new_image, thumb)) => {
                    let id = self.upsert_image(&new_image)?;
                    self.put_thumbnail(id, &thumb)?;
                    summary.added += 1;
                }
                Err(msg) => {
                    eprintln!("ferrolite-catalog: decode failed for {}: {msg}", d.filename);
                    let failed = NewImage::failed(d.folder_id, d.filename, d.mtime, d.size, d.kind);
                    self.upsert_image(&failed)?;
                    summary.failed += 1;
                }
            }
        }

        for id in dir_ids.values() {
            self.conn().execute(
                "UPDATE folders SET last_scanned = ?1 WHERE id = ?2",
                rusqlite::params![now_secs(), id],
            )?;
        }
        Ok(summary)
    }
}

fn decode_one(
    path: &std::path::Path,
    folder_id: i64,
    filename: &str,
    mtime: i64,
    size: i64,
    kind: FileKind,
) -> Result<(NewImage, Thumbnail), String> {
    let meta = ferrolite_decode::read_metadata(path, kind).map_err(|e| e.to_string())?;
    let preview = ferrolite_decode::decode_preview(path, kind).map_err(|e| e.to_string())?;
    let thumb = generate_thumbnail(&preview).map_err(|e| e.to_string())?;
    let new_image =
        NewImage::from_metadata(folder_id, filename.to_string(), mtime, size, &meta, kind);
    Ok((new_image, thumb))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
