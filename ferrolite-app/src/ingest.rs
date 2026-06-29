//! Job orchestration: folder ingest (Interactive) fans out per-image thumbnail
//! jobs (Background). All photo/catalog knowledge lives here in the app; the
//! `ferrolite-jobs` crate stays domain-agnostic.

use crate::events::AppEvent;
use crate::state::AppState;
use ferrolite_catalog::{scan_raw_files, Catalog, DecodeStatus, NewImage, ReadPool};
use ferrolite_jobs::{CancelToken, JobSystem, Priority};
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// Start ingesting `folder`: cancels any in-flight ingest + pending thumbnails,
/// resets counters, then submits the Interactive walk/upsert job.
pub fn spawn_ingest(state: &mut AppState, ctx: &egui::Context, folder: PathBuf) {
    // Cancel superseded work (contract §5.1).
    if let Some(h) = state.ingest_handle.take() {
        h.cancel();
    }
    for (_image_id, job_id) in state.thumb_jobs.drain() {
        state.jobs.cancel(job_id);
    }
    state.indexed = 0;
    state.thumb_total = 0;
    state.thumb_done = 0;
    state.images.clear();

    let writer = Arc::clone(&state.writer);
    let reads = Arc::clone(&state.reads);
    let jobs = Arc::clone(&state.jobs);
    let tx = state.tx.clone();
    let ctx = ctx.clone();

    // Resolve folder_id up front (quick write) so the job can key rows.
    let folder_id = match writer.lock().expect("writer").upsert_folder(&folder) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("ferrolite: upsert_folder failed: {e}");
            return;
        }
    };
    state.current_folder = Some(folder_id);

    let jobs_for_closure = Arc::clone(&jobs);
    let handle = jobs.submit(Priority::Interactive, move |cancel| {
        ingest_job(folder, folder_id, writer, reads, jobs_for_closure, tx, ctx, cancel);
    });
    state.ingest_handle = Some(handle);
}

#[allow(clippy::too_many_arguments)]
fn ingest_job(
    folder: PathBuf,
    folder_id: i64,
    writer: Arc<Mutex<Catalog>>,
    reads: Arc<ReadPool>,
    jobs: Arc<JobSystem>,
    tx: Sender<AppEvent>,
    ctx: egui::Context,
    cancel: &CancelToken,
) {
    let files = scan_raw_files(&folder);

    // Parallel metadata decode for files needing (re)ingest. No DB writes here.
    let rows: Vec<(NewImage, PathBuf)> = files
        .par_iter()
        .filter(|_| !cancel.is_cancelled())
        .filter_map(|f| {
            match reads.needs_reingest(folder_id, &f.filename, f.mtime, f.size) {
                Ok(true) => {}
                Ok(false) => return None,
                Err(_) => return None,
            }
            match ferrolite_decode::read_metadata(&f.path) {
                Ok(meta) => Some((
                    NewImage::from_metadata(folder_id, f.filename.clone(), f.mtime, f.size, &meta),
                    f.path.clone(),
                )),
                Err(_) => Some((
                    NewImage::failed(folder_id, f.filename.clone(), f.mtime, f.size),
                    f.path.clone(),
                )),
            }
        })
        .collect();

    // Serial row upserts under the writer lock; enqueue a thumbnail job per row.
    for (new_image, path) in rows {
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
            spawn_thumbnail(&jobs, &writer, &tx, &ctx, id, path);
        }
        ctx.request_repaint();
    }
    let _ = tx.send(AppEvent::IngestDone);
    ctx.request_repaint();
}

/// Submit one Background thumbnail job: decode preview → resize/encode → write
/// BLOB → send JPEG bytes for immediate display. Returns the JobId so the caller
/// can record it for reprioritization. (Called from `ingest_job`; the returned
/// id is recorded by the app via the `ThumbReady`/registration path in Task 7.)
pub fn spawn_thumbnail(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
) -> ferrolite_jobs::JobId {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Background, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        let result = ferrolite_decode::decode_preview(&path)
            .map_err(|e| e.to_string())
            .and_then(|preview| {
                ferrolite_catalog::generate_thumbnail(&preview).map_err(|e| e.to_string())
            });
        match result {
            Ok(thumb) => {
                {
                    use ferrolite_catalog::ThumbnailStore;
                    if let Err(e) = writer.lock().expect("writer").put_thumbnail(image_id, &thumb) {
                        eprintln!("ferrolite: put_thumbnail failed: {e}");
                    }
                }
                let _ = tx.send(AppEvent::ThumbReady { image_id, jpeg: thumb.bytes });
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
