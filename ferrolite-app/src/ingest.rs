//! Job orchestration: folder ingest (Interactive) fans out per-image thumbnail
//! jobs (Background). All photo/catalog knowledge lives here in the app; the
//! `ferrolite-jobs` crate stays domain-agnostic.

use crate::events::AppEvent;
use crate::state::AppState;
use ferrolite_catalog::{scan_raw_files, Catalog, DecodeStatus, NewImage, ReadPool, Thumbnail};
use ferrolite_jobs::{CancelToken, JobSystem, Priority};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// Start ingesting `folder`: cancels any in-flight ingest + pending thumbnails,
/// resets counters, then submits the Interactive walk/upsert job.
pub fn spawn_ingest(state: &mut AppState, ctx: &egui::Context, folder: PathBuf) {
    // Cancel superseded work and zero all per-folder counters (contract §5.1).
    state.reset_for_new_folder();
    // Note: a late `ThumbRegistered` or `ThumbReady` from the just-cancelled
    // prior ingest may transiently re-touch `thumb_jobs`/counters before the
    // cancel propagates. This is benign: textures are keyed by globally-unique
    // `image_id`, so a stale thumbnail is cached-but-never-painted and only
    // causes a transient status-bar count drift until the next reset.

    let writer = Arc::clone(&state.writer);
    let reads = Arc::clone(&state.reads);
    let jobs = Arc::clone(&state.jobs);
    let tx = state.tx.clone();
    let ctx = ctx.clone();

    // Resolve folder_id up front (quick write) so the job can key rows.
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
        ingest_job(
            folder,
            folder_id,
            writer,
            reads,
            jobs_for_closure,
            tx,
            ctx,
            cancel,
        );
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
            let kind = f.kind;
            match ferrolite_decode::read_metadata(&f.path, kind) {
                Ok(meta) => Some((
                    NewImage::from_metadata(
                        folder_id,
                        f.filename.clone(),
                        f.mtime,
                        f.size,
                        &meta,
                        kind,
                    ),
                    f.path.clone(),
                )),
                Err(_) => Some((
                    NewImage::failed(folder_id, f.filename.clone(), f.mtime, f.size, kind),
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
            let job_id = spawn_thumbnail(&jobs, &writer, &tx, &ctx, id, path);
            state_total_inc(&tx, id, job_id);
        }
        ctx.request_repaint();
    }
    let _ = tx.send(AppEvent::IngestDone);
    ctx.request_repaint();
}

fn state_total_inc(tx: &Sender<AppEvent>, image_id: i64, job_id: ferrolite_jobs::JobId) {
    let _ = tx.send(AppEvent::ThumbRegistered { image_id, job_id });
}

/// Headless thumbnail helper: decode preview → resize/encode → persist BLOB.
/// Returns the committed `Thumbnail` on success, or an error string on failure.
/// Called by both `spawn_thumbnail` (inside a job closure) and `bench_browse`
/// (directly, without an egui context). This keeps the real decode path DRY.
pub fn thumbnail_blocking(
    writer: &Arc<Mutex<Catalog>>,
    image_id: i64,
    path: &Path,
) -> Result<Thumbnail, String> {
    // thumbnail_blocking doesn't know the kind at this call site; default to Raw
    // (the bench and job paths that use this function work on RAW-only folders).
    let preview = ferrolite_decode::decode_preview(path, ferrolite_image::FileKind::Raw)
        .map_err(|e| e.to_string())?;
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
        match thumbnail_blocking(&writer, image_id, &path) {
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
