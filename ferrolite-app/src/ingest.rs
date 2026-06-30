//! Job orchestration: recursive folder ingest (Interactive) fans out per-image
//! thumbnail jobs (Background). All photo/catalog knowledge lives here; the
//! `ferrolite-jobs` crate stays domain-agnostic.

use crate::events::AppEvent;
use crate::state::AppState;
use ferrolite_catalog::{
    collect_dirs, scan_tree, Catalog, DecodeStatus, FileKind, NewImage, ReadPool, Thumbnail,
};

pub(crate) fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
use ferrolite_jobs::{CancelToken, JobHandle, JobSystem, Priority};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// How often the background watcher polls the selected folder for new files.
pub const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Pure predicate: should the periodic watcher fire this frame? True iff a
/// folder is selected, no ingest is in flight, and at least `interval` has
/// elapsed since `last_check` (or there has been no check yet).
pub fn should_watch(
    now: std::time::Instant,
    last_check: Option<std::time::Instant>,
    interval: std::time::Duration,
    current_folder: Option<i64>,
    active_ingests: usize,
) -> bool {
    if current_folder.is_none() || active_ingests != 0 {
        return false;
    }
    match last_check {
        None => true,
        Some(t) => now.duration_since(t) >= interval,
    }
}

/// How a (re)ingest treats already-indexed files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReindexMode {
    /// Skip files whose (mtime, size) are unchanged (default / soft).
    Incremental,
    /// Force re-decode + re-thumbnail every file, and prune catalog rows for
    /// files/folders no longer on disk (hard / full rebuild).
    Full,
}

/// Submit one ingest job for `folder` at `priority` with `mode`, incrementing
/// the in-flight counter. Returns the handle so the caller can store it for
/// cancellation. Does NOT reset the view — callers decide that.
pub(crate) fn submit_ingest(
    state: &mut AppState,
    ctx: &egui::Context,
    folder: PathBuf,
    mode: ReindexMode,
    priority: Priority,
) -> JobHandle {
    state.active_ingests += 1;
    let writer = Arc::clone(&state.writer);
    let reads = Arc::clone(&state.reads);
    let jobs = Arc::clone(&state.jobs);
    let jobs_for_closure = Arc::clone(&jobs);
    let tx = state.tx.clone();
    let ctx = ctx.clone();
    jobs.submit(priority, move |cancel| {
        ingest_job(
            folder,
            mode,
            writer,
            reads,
            jobs_for_closure,
            tx,
            ctx,
            cancel,
        );
    })
}

pub fn spawn_ingest(state: &mut AppState, ctx: &egui::Context, folder: PathBuf) {
    state.reset_for_new_folder();

    let folder_id = match state
        .writer
        .lock()
        .expect("writer")
        .upsert_folder(&folder, None)
    {
        Ok(id) => id,
        Err(e) => {
            eprintln!("ferrolite: upsert_folder failed: {e}");
            return;
        }
    };
    state.current_folder = Some(folder_id);

    let handle = submit_ingest(
        state,
        ctx,
        folder,
        ReindexMode::Incremental,
        Priority::Interactive,
    );
    state.ingest_handle = Some(handle);
}

/// Reindex a folder's subtree in place (does not clear the grid like Open Folder).
/// `Full` zeroes the thumbnail-progress counters for a clean status-bar readout.
pub fn spawn_reindex(
    state: &mut AppState,
    ctx: &egui::Context,
    folder_path: PathBuf,
    mode: ReindexMode,
) {
    state.cancel_pending_jobs();
    if matches!(mode, ReindexMode::Full) {
        state.thumb_total = 0;
        state.thumb_done = 0;
    }
    state.dirty = true;
    let handle = submit_ingest(state, ctx, folder_path, mode, Priority::Interactive);
    state.ingest_handle = Some(handle);
}

/// Spawn a silent Background incremental scan of the currently-selected folder's
/// subtree (picks up newly-added files). No view/counter reset.
pub fn spawn_watch_scan(state: &mut AppState, ctx: &egui::Context) {
    let Some(folder_id) = state.current_folder else {
        return;
    };
    let path = match state.reads.folder_path(folder_id) {
        Ok(Some(p)) => PathBuf::from(p),
        _ => return,
    };
    let handle = submit_ingest(
        state,
        ctx,
        path,
        ReindexMode::Incremental,
        Priority::Background,
    );
    // Safe to overwrite without cancelling the prior handle: the only caller
    // (the per-frame tick) is gated by `should_watch`, which requires
    // `active_ingests == 0`, so any previously-stored ingest has already
    // drained. Storing it (rather than discarding like the startup sweep) lets
    // a subsequent folder switch cancel this watcher scan of the now-stale folder.
    state.ingest_handle = Some(handle);
}

/// One-time startup sweep: a Background incremental scan of every root folder
/// (parent_id is NULL) so on-disk changes since last launch appear immediately.
/// A recursive scan per root covers all descendants.
pub fn spawn_startup_rescan(state: &mut AppState, ctx: &egui::Context) {
    let roots: Vec<PathBuf> = state
        .reads
        .list_folders()
        .unwrap_or_default()
        .into_iter()
        .filter(|f| f.parent_id.is_none())
        .map(|f| PathBuf::from(f.path))
        .collect();
    for root in roots {
        // Each increments active_ingests; handles are not individually tracked
        // (cheap, silent, idempotent incremental scans).
        let _ = submit_ingest(
            state,
            ctx,
            root,
            ReindexMode::Incremental,
            Priority::Background,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn ingest_job(
    folder: PathBuf,
    mode: ReindexMode,
    writer: Arc<Mutex<Catalog>>,
    reads: Arc<ReadPool>,
    jobs: Arc<JobSystem>,
    tx: Sender<AppEvent>,
    ctx: egui::Context,
    cancel: &CancelToken,
) {
    let files = scan_tree(&folder);
    let force = matches!(mode, ReindexMode::Full);

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
    let root_folder_id = dir_ids.get(&folder).copied();

    // Parallel metadata decode. Incremental skips unchanged files; Full forces all.
    let rows: Vec<(NewImage, PathBuf, FileKind)> = files
        .par_iter()
        .filter(|_| !cancel.is_cancelled())
        .filter_map(|f| {
            let folder_id = *f.path.parent().and_then(|p| dir_ids.get(p))?;
            if !force {
                match reads.needs_reingest(folder_id, &f.filename, f.mtime, f.size) {
                    Ok(true) => {}
                    _ => return None,
                }
            }
            let added_at = now_epoch_secs();
            let rating = ferrolite_catalog::read_rating(&ferrolite_catalog::sidecar_path(&f.path))
                .unwrap_or_default();
            let new_image = match ferrolite_decode::read_metadata(&f.path, f.kind) {
                Ok(meta) => NewImage::from_metadata(
                    folder_id,
                    f.filename.clone(),
                    f.mtime,
                    f.size,
                    &meta,
                    f.kind,
                    rating,
                    added_at,
                ),
                Err(_) => NewImage::failed(
                    folder_id,
                    f.filename.clone(),
                    f.mtime,
                    f.size,
                    f.kind,
                    added_at,
                ),
            };
            Some((new_image, f.path.clone(), f.kind))
        })
        .collect();

    // Serial row upserts under the writer lock; enqueue a thumbnail job per row.
    // For Full, collect every present file's id so prune can delete the rest.
    let mut kept_image_ids: HashSet<i64> = HashSet::new();
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
        if force {
            kept_image_ids.insert(id);
        }
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

    // Full: prune catalog rows for files/folders no longer on disk. Skip if
    // cancelled (kept set would be incomplete).
    if force && !cancel.is_cancelled() {
        if let Some(root) = root_folder_id {
            let kept_folder_ids: HashSet<i64> = dir_ids.values().copied().collect();
            if let Err(e) = writer.lock().expect("writer").prune_subtree(
                root,
                &kept_folder_ids,
                &kept_image_ids,
            ) {
                eprintln!("ferrolite: prune_subtree failed: {e}");
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn now_epoch_secs_is_positive() {
        assert!(super::now_epoch_secs() > 1_000_000_000);
    }

    #[test]
    fn should_watch_fires_only_when_idle_selected_and_elapsed() {
        let iv = Duration::from_secs(10);
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(11);
        let soon = t0 + Duration::from_secs(3);

        // Happy path: folder selected, no ingest, interval elapsed.
        assert!(should_watch(later, Some(t0), iv, Some(1), 0));
        // First-ever check (no last_check) fires.
        assert!(should_watch(t0, None, iv, Some(1), 0));
        // Not enough time elapsed.
        assert!(!should_watch(soon, Some(t0), iv, Some(1), 0));
        // No folder selected.
        assert!(!should_watch(later, Some(t0), iv, None, 0));
        // An ingest is in flight.
        assert!(!should_watch(later, Some(t0), iv, Some(1), 2));
    }
}
