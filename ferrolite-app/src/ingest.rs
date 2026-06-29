//! Job orchestration: recursive folder ingest (Interactive) fans out per-image
//! thumbnail jobs (Background). All photo/catalog knowledge lives here; the
//! `ferrolite-jobs` crate stays domain-agnostic.

use crate::events::AppEvent;
use crate::state::AppState;
use ferrolite_catalog::{
    collect_dirs, scan_tree, Catalog, DecodeStatus, FileKind, NewImage, ReadPool, Thumbnail,
};
use ferrolite_jobs::{CancelToken, JobSystem, Priority};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub fn spawn_ingest(state: &mut AppState, ctx: &egui::Context, folder: PathBuf) {
    state.reset_for_new_folder();

    let writer = Arc::clone(&state.writer);
    let reads = Arc::clone(&state.reads);
    let jobs = Arc::clone(&state.jobs);
    let tx = state.tx.clone();
    let ctx = ctx.clone();

    let folder_id = match writer.lock().expect("writer").upsert_folder(&folder, None) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("ferrolite: upsert_folder failed: {e}");
            return;
        }
    };
    state.current_folder = Some(folder_id);

    let jobs_for_closure = Arc::clone(&jobs);
    let handle = jobs.submit(Priority::Interactive, move |cancel| {
        ingest_job(folder, writer, reads, jobs_for_closure, tx, ctx, cancel);
    });
    state.ingest_handle = Some(handle);
}

#[allow(clippy::too_many_arguments)]
fn ingest_job(
    folder: PathBuf,
    writer: Arc<Mutex<Catalog>>,
    reads: Arc<ReadPool>,
    jobs: Arc<JobSystem>,
    tx: Sender<AppEvent>,
    ctx: egui::Context,
    cancel: &CancelToken,
) {
    let files = scan_tree(&folder);

    // Create folder rows top-down, wiring parent_id.
    let mut dir_ids: HashMap<PathBuf, i64> = HashMap::new();
    for dir in collect_dirs(&files, &folder) {
        if cancel.is_cancelled() {
            return;
        }
        let parent_id = dir.parent().and_then(|p| dir_ids.get(p).copied());
        match writer
            .lock()
            .expect("writer")
            .upsert_folder(&dir, parent_id)
        {
            Ok(id) => {
                dir_ids.insert(dir, id);
            }
            Err(e) => eprintln!("ferrolite: upsert_folder failed: {e}"),
        }
    }

    // Parallel metadata decode for files needing (re)ingest. No DB writes here.
    let rows: Vec<(NewImage, PathBuf, FileKind)> = files
        .par_iter()
        .filter(|_| !cancel.is_cancelled())
        .filter_map(|f| {
            let folder_id = *f.path.parent().and_then(|p| dir_ids.get(p))?;
            match reads.needs_reingest(folder_id, &f.filename, f.mtime, f.size) {
                Ok(true) => {}
                _ => return None,
            }
            let new_image = match ferrolite_decode::read_metadata(&f.path, f.kind) {
                Ok(meta) => NewImage::from_metadata(
                    folder_id,
                    f.filename.clone(),
                    f.mtime,
                    f.size,
                    &meta,
                    f.kind,
                ),
                Err(_) => NewImage::failed(folder_id, f.filename.clone(), f.mtime, f.size, f.kind),
            };
            Some((new_image, f.path.clone(), f.kind))
        })
        .collect();

    for (new_image, path, kind) in rows {
        if cancel.is_cancelled() {
            break;
        }
        let id = match writer.lock().expect("writer").upsert_image(&new_image) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("ferrolite: upsert_image failed: {e}");
                continue;
            }
        };
        let _ = tx.send(AppEvent::Indexed { added: 1 });
        if new_image.decode_status != DecodeStatus::Failed {
            let job_id = spawn_thumbnail(&jobs, &writer, &tx, &ctx, id, path, kind);
            let _ = tx.send(AppEvent::ThumbRegistered {
                image_id: id,
                job_id,
            });
        }
        ctx.request_repaint();
    }
    let _ = tx.send(AppEvent::IngestDone);
    ctx.request_repaint();
}

/// Headless thumbnail helper: decode preview → resize/encode → persist BLOB.
pub fn thumbnail_blocking(
    writer: &Arc<Mutex<Catalog>>,
    image_id: i64,
    path: &Path,
    kind: FileKind,
) -> Result<Thumbnail, String> {
    let preview = ferrolite_decode::decode_preview(path, kind).map_err(|e| e.to_string())?;
    let thumb = ferrolite_catalog::generate_thumbnail(&preview).map_err(|e| e.to_string())?;
    {
        use ferrolite_catalog::ThumbnailStore;
        writer
            .lock()
            .expect("writer")
            .put_thumbnail(image_id, &thumb)
            .map_err(|e| e.to_string())?;
    }
    Ok(thumb)
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_thumbnail(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
    kind: FileKind,
) -> ferrolite_jobs::JobId {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Background, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        match thumbnail_blocking(&writer, image_id, &path, kind) {
            Ok(thumb) => {
                let _ = tx.send(AppEvent::ThumbReady {
                    image_id,
                    jpeg: thumb.bytes,
                });
            }
            Err(msg) => {
                eprintln!("ferrolite: thumbnail failed for #{image_id}: {msg}");
                let _ = writer
                    .lock()
                    .expect("writer")
                    .set_decode_status(image_id, DecodeStatus::Failed);
                let _ = tx.send(AppEvent::ThumbFailed { image_id });
            }
        }
        ctx.request_repaint();
    })
    .id()
}
