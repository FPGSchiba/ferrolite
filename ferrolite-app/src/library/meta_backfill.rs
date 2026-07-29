//! Task 14: background EXIF metadata backfill for images cataloged before the
//! v7 migration (`ferrolite-catalog::schema`) added the `lens`/`aperture`/
//! `focal_length` columns. Every such pre-v7 row reads back all-NULL for the
//! three columns, so the metadata range/lens filters added alongside v7
//! silently exclude them from an existing library until they are backfilled.
//!
//! This module walks the NULL-metadata backlog off the UI thread in small
//! batches, re-reading each file's EXIF via the SAME `ferrolite_decode`
//! metadata-only call `develop::meta_read::spawn_meta_read` uses (no pixel
//! decode), and delivers each finished batch as ONE `AppEvent::MetaBackfillReady`
//! (CLAUDE.md rule 1: even a "cheap" header read is real file I/O and must
//! never run on the UI/update thread). The listing read goes through the
//! `ReadPool` from this job thread (mirrors
//! `develop::warm_prefetch::spawn_warm_sources`'s off-thread-catalog-read
//! pattern); the batch WRITE happens later, on the UI thread, inside
//! `AppState::apply`'s `MetaBackfillReady` arm — see that variant's doc
//! comment for why.
//!
//! **Skip semantics (load-bearing):** a row whose EXIF read fails (missing or
//! corrupt file) OR whose metadata has no lens/aperture/focal data at all is
//! written back with `lens = Some(String::new())` — an empty string, not
//! `NULL`. This is the "attempted, found nothing" sentinel documented on
//! `ferrolite_catalog::images_needing_metadata_backfill`: without it such a
//! row would stay all-NULL forever and be re-read on every subsequent launch.
//! A PARTIAL read (e.g. aperture found but no lens) needs no sentinel — it
//! already has a non-NULL column and drops out of the backlog on its own.

use std::sync::mpsc::Sender;
use std::sync::Arc;

use ferrolite_catalog::{BackfillCandidate, BackfillResult, ReadPool};
use ferrolite_jobs::{JobHandle, JobSystem, Priority};

use crate::events::AppEvent;

/// Rows fetched per catalog round-trip and delivered per event (brief: 64).
pub const BATCH_SIZE: i64 = 64;

/// Read one candidate's EXIF and resolve it to a `BackfillResult`. Applies
/// the empty-string sentinel (see the module doc) when the read errors out
/// or comes back with all three fields absent.
fn backfill_one(candidate: &BackfillCandidate) -> BackfillResult {
    let meta = ferrolite_decode::read_metadata(&candidate.path, candidate.kind).ok();
    let (lens, aperture, focal_length) = match meta {
        Some(m) if m.lens.is_some() || m.aperture.is_some() || m.focal_length.is_some() => {
            (m.lens, m.aperture, m.focal_length)
        }
        _ => (Some(String::new()), None, None),
    };
    BackfillResult {
        id: candidate.id,
        lens,
        aperture,
        focal_length,
    }
}

/// Spawn the one-shot `Background` job that walks the ENTIRE NULL-metadata
/// backlog in `BATCH_SIZE` chunks, oldest id first, until none remain or
/// `cancel` fires. Cancellation is checked between batches AND between
/// individual files within a batch, so a shutdown mid-batch stops promptly
/// instead of finishing the whole backlog first.
///
/// Uses the id-cursor listing (`ReadPool::images_needing_metadata_backfill`)
/// rather than re-querying the same "all NULL" predicate from a fixed start:
/// the write-back for a batch happens later, on the UI thread, so a naive
/// re-query issued before that write lands would just re-fetch (and
/// re-decode) the same rows instead of making forward progress.
pub fn spawn_meta_backfill(
    jobs: &Arc<JobSystem>,
    reads: Arc<ReadPool>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
) -> JobHandle {
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Background, move |cancel| {
        let mut cursor = 0i64;
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let candidates = match reads.images_needing_metadata_backfill(cursor, BATCH_SIZE) {
                Ok(rows) => rows,
                Err(e) => {
                    eprintln!("meta backfill: listing failed: {e}");
                    return;
                }
            };
            let Some(last) = candidates.last() else {
                return; // caught up
            };
            cursor = last.id;

            let mut results = Vec::with_capacity(candidates.len());
            for candidate in &candidates {
                if cancel.is_cancelled() {
                    return;
                }
                results.push(backfill_one(candidate));
            }
            if cancel.is_cancelled() {
                return;
            }
            let _ = tx.send(AppEvent::MetaBackfillReady { results });
            ctx.request_repaint();
        }
    })
}

/// One-shot startup gate: spawn the backfill job only when the cheap `COUNT`
/// query (`metadata_backfill_pending_count`) says there is work, so a
/// fully-backfilled catalog never pays even one `Background` job submission
/// on a later launch. Called once from `FerroliteApp::update`'s first-frame
/// block (mirrors the `did_restore` one-shot guard); the returned handle is
/// stored on `AppState::meta_backfill_handle` so it can be cancelled on
/// shutdown like the app's other long-lived job handles.
pub fn maybe_spawn(state: &mut crate::state::AppState, ctx: &egui::Context) {
    let pending = state.reads.metadata_backfill_pending_count().unwrap_or(0);
    if pending == 0 {
        return;
    }
    let handle = spawn_meta_backfill(&state.jobs, Arc::clone(&state.reads), &state.tx, ctx);
    state.meta_backfill_handle = Some(handle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_catalog::{Catalog, FileKind, NewImage};
    use std::path::PathBuf;

    fn temp_catalog(name: &str) -> (Catalog, PathBuf) {
        let tid = format!("{:?}", std::thread::current().id()).replace(['(', ')'], "");
        let dir = std::env::temp_dir().join(format!(
            "frl-meta-backfill-{name}-{}-{tid}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.db");
        let _ = std::fs::remove_file(&path);
        (Catalog::open(&path).unwrap(), path)
    }

    #[test]
    fn backfill_one_returns_sentinel_for_a_missing_file() {
        let candidate = BackfillCandidate {
            id: 1,
            path: PathBuf::from("/does/not/exist.nef"),
            kind: FileKind::Raw,
        };
        let result = backfill_one(&candidate);
        assert_eq!(result.id, 1);
        assert_eq!(
            result.lens.as_deref(),
            Some(""),
            "a decode failure must write the empty-string sentinel"
        );
        assert_eq!(result.aperture, None);
        assert_eq!(result.focal_length, None);
    }

    /// End-to-end (no GPU / egui context needed): the listing + batching +
    /// write-back loop drains a real catalog's NULL-metadata backlog to
    /// zero. Exercises `spawn_meta_backfill`'s job body directly via
    /// `backfill_one` + the catalog's own batch-write, since running the
    /// actual job needs a `JobSystem` + `egui::Context` the unit test would
    /// rather not spin up — the job's internals are otherwise exercised by
    /// the `ferrolite-catalog` listing/update tests.
    #[test]
    fn missing_files_are_marked_sentinel_and_drop_out_of_the_backlog() {
        let (cat, _path) = temp_catalog("drain");
        let f = cat.upsert_folder(std::path::Path::new("/p"), None).unwrap();
        cat.upsert_image(&NewImage::pending(
            f,
            "a.nef".into(),
            1,
            1,
            FileKind::Raw,
            0,
        ))
        .unwrap();
        cat.upsert_image(&NewImage::pending(
            f,
            "b.nef".into(),
            1,
            1,
            FileKind::Raw,
            0,
        ))
        .unwrap();
        assert_eq!(cat.metadata_backfill_pending_count().unwrap(), 2);

        let candidates = cat.images_needing_metadata_backfill(0, BATCH_SIZE).unwrap();
        let results: Vec<BackfillResult> = candidates.iter().map(backfill_one).collect();
        cat.apply_metadata_backfill_batch(&results).unwrap();

        assert_eq!(
            cat.metadata_backfill_pending_count().unwrap(),
            0,
            "both missing-file rows must be sentinel-marked, not left NULL"
        );
    }
}
