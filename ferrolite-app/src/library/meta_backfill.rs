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
//! never run on the UI/update thread — and that rule covers the backlog
//! CHECK too, not just the EXIF reads: `has_backlog`'s `COUNT(*)` is an
//! unindexed sequential scan of `images`, so it runs as the job's first step,
//! off the UI thread, not as a spawn-time gate on the UI thread). Every read
//! (the backlog check AND the listing) goes through the `ReadPool` from this
//! job thread (mirrors `develop::warm_prefetch::spawn_warm_sources`'s
//! off-thread-catalog-read pattern); the batch WRITE happens later, on the
//! UI thread, inside `AppState::apply`'s `MetaBackfillReady` arm — see that
//! variant's doc comment for why.
//!
//! **Actual per-launch cost:** the job is spawned unconditionally on every
//! app run's first frame (`spawn_once`); a fully-backfilled library pays
//! exactly one off-thread `COUNT(*)` per launch, forever, and zero UI-thread
//! work either way.
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

/// Whether there is any Task-14 backlog left at all — the job's first step,
/// run OFF the UI thread (see the module doc: this `COUNT(*)` has no
/// supporting index, so it's a full sequential scan of `images` and must
/// never run synchronously at spawn time). `Err` (a broken catalog handle)
/// is treated the same as "nothing to do": the job simply exits, and a
/// healthy catalog will retry on the next launch.
fn has_backlog(reads: &ReadPool) -> bool {
    reads.metadata_backfill_pending_count().unwrap_or(0) > 0
}

/// Spawn the one-shot `Background` job that walks the ENTIRE NULL-metadata
/// backlog in `BATCH_SIZE` chunks, oldest id first, until none remain or
/// `cancel` fires. Its FIRST action, before any listing or decode work, is
/// `has_backlog` — so a fully-backfilled catalog does exactly one off-thread
/// `COUNT(*)` and returns, never touching `images_needing_metadata_backfill`
/// or `ferrolite_decode::read_metadata` at all. Cancellation is then checked
/// between batches AND between individual files within a batch, so a
/// shutdown mid-batch stops promptly instead of finishing the whole backlog
/// first.
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
        if cancel.is_cancelled() {
            return;
        }
        if !has_backlog(&reads) {
            return; // nothing to do this launch — one COUNT(*), no more work
        }

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

/// One-shot-per-run startup spawn: submits the backfill job UNCONDITIONALLY
/// on the first `update()` frame (the caller's `did_meta_backfill_spawn` flag
/// ensures that). There is deliberately NO gate here on the UI thread — the
/// job itself checks `has_backlog` as its first off-thread step, so a
/// fully-backfilled library still pays exactly one `Background` job
/// submission per launch, but zero UI-thread catalog work. The returned
/// handle is stored on `AppState::meta_backfill_handle` so it can be
/// cancelled on shutdown like the app's other long-lived job handles.
pub fn spawn_once(state: &mut crate::state::AppState, ctx: &egui::Context) {
    let handle = spawn_meta_backfill(&state.jobs, Arc::clone(&state.reads), &state.tx, ctx);
    state.meta_backfill_handle = Some(handle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_catalog::{Catalog, FileKind, NewImage, ReadPool};
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

    /// `has_backlog` is the job's first, off-thread-only step (see the
    /// reviewed fix: no UI-thread COUNT call anymore). This is the retargeted
    /// gate test: an empty/fully-backfilled catalog must report no backlog
    /// (the job would return here, before any listing/decode work), and a
    /// catalog with a NULL-metadata row must report backlog present.
    #[test]
    fn has_backlog_false_when_catalog_is_fully_backfilled() {
        let (cat, path) = temp_catalog("gate-empty");
        // A row with real (non-NULL) metadata: nothing left to do.
        let f = cat.upsert_folder(std::path::Path::new("/p"), None).unwrap();
        cat.apply_metadata_backfill_batch(&[BackfillResult {
            id: cat
                .upsert_image(&NewImage::pending(
                    f,
                    "a.nef".into(),
                    1,
                    1,
                    FileKind::Raw,
                    0,
                ))
                .unwrap(),
            lens: Some("50mm f/1.8".to_string()),
            aperture: Some(1.8),
            focal_length: Some(50.0),
        }])
        .unwrap();
        drop(cat);

        let reads = ReadPool::open(&path, 1).unwrap();
        assert!(
            !has_backlog(&reads),
            "a fully-backfilled catalog must report no backlog"
        );
    }

    #[test]
    fn has_backlog_true_when_a_null_metadata_row_exists() {
        let (cat, path) = temp_catalog("gate-pending");
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
        drop(cat);

        let reads = ReadPool::open(&path, 1).unwrap();
        assert!(
            has_backlog(&reads),
            "a NULL-metadata row must report backlog present"
        );
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
