//! Job orchestration: recursive folder ingest (Interactive) decodes each file
//! ONCE — metadata + preview together — and generates its thumbnail inline in
//! the parallel producer, streaming the finished thumbnail to the serial
//! consumer for persistence + texture upload. All photo/catalog knowledge lives
//! here; the `ferrolite-jobs` crate stays domain-agnostic.

use crate::events::AppEvent;
use crate::state::AppState;
use ferrolite_catalog::{
    collect_dirs, scan_tree, Catalog, DecodedThumb, FileKind, NewImage, ReadPool, Thumbnail,
};
use ferrolite_jobs::{CancelToken, JobHandle, Priority};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub(crate) fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// How often the background watcher polls the selected folder for new files.
pub const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Ingest consumer batch size: rows are accumulated and committed in one
/// transaction every this-many rows (plus a final partial flush at channel
/// close/cancel), instead of one autocommit INSERT per row (RC-PERF-3).
const INGEST_WRITE_BATCH: usize = 128;

/// Max time a decoded-but-uncommitted row waits before the consumer flushes a
/// partial batch. Caps grid-visibility latency at ~this under slow per-file
/// generation, where 128 rows would otherwise take seconds to accumulate. Small
/// enough to feel live, large enough to keep flushes to a few per second so the
/// batching win is preserved.
const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

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
///
/// Resets the scan/ingest progress counters (`scanned`/`indexed`/
/// `ingest_total`/`ingest_done`) when this call starts a brand-new wave
/// (`active_ingests` transitioning 0→1). These are monotonic `+=`
/// accumulators folded from `Scanned`/`Indexed`/`IngestPlanned` events; without
/// a per-wave reset they inflate across the many ingest passes that touch the
/// same files (per-root startup rescans, the 10s watcher re-scan), since the
/// only prior reset point was a folder switch. Concurrent per-root passes
/// *within* one wave still accumulate together correctly (summing to the real
/// file count) because they all land between one 0→1 and the matching
/// N→0 `IngestDone`; the reset only fires at the very start of a wave.
pub(crate) fn submit_ingest(
    state: &mut AppState,
    ctx: &egui::Context,
    folder: PathBuf,
    mode: ReindexMode,
    priority: Priority,
) -> JobHandle {
    if state.active_ingests == 0 {
        state.reset_ingest_counters();
    }
    state.active_ingests += 1;
    let writer = Arc::clone(&state.writer);
    let reads = Arc::clone(&state.reads);
    let jobs = Arc::clone(&state.jobs);
    let tx = state.tx.clone();
    let ctx = ctx.clone();
    jobs.submit(priority, move |cancel| {
        ingest_job(folder, mode, writer, reads, tx, ctx, cancel);
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
/// `Full` zeroes the ingest-progress counters for a clean status-bar readout.
pub fn spawn_reindex(
    state: &mut AppState,
    ctx: &egui::Context,
    folder_path: PathBuf,
    mode: ReindexMode,
) {
    state.cancel_pending_jobs();
    if matches!(mode, ReindexMode::Full) {
        state.ingest_total = 0;
        state.ingest_done = 0;
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

/// Commit `pending` (draining it) in ONE transaction via
/// [`Catalog::upsert_images_with_thumbnails_batch`], then emit the per-row
/// `Indexed`/`ThumbReady` events using the ids the batch returns (in the same
/// order the rows were pushed). A no-op on an empty batch, so callers can call
/// it unconditionally as a "final flush" without checking emptiness first.
///
/// The writer lock is held only for the duration of the batch commit, not
/// across the channel `recv` — the caller accumulates `pending` outside the
/// lock and this function is the sole place that takes it.
fn flush_batch(
    writer: &Arc<Mutex<Catalog>>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    force: bool,
    pending: &mut Vec<(NewImage, Option<(Thumbnail, DecodedThumb)>)>,
    kept: &mut HashSet<i64>,
    profile: Option<&Arc<crate::diag::IngestProfile>>,
) {
    if pending.is_empty() {
        return;
    }
    let rows = std::mem::take(pending);
    let t_batch = profile.map(|_| std::time::Instant::now());

    // Split each row into the (NewImage, Option<Thumbnail>) the batch write
    // needs and the DecodedThumb kept aside for the post-commit texture-upload
    // event — moved, not cloned, since NewImage/Thumbnail aren't cheap to copy.
    let mut decoded_thumbs: Vec<Option<DecodedThumb>> = Vec::with_capacity(rows.len());
    let batch_input: Vec<(NewImage, Option<Thumbnail>)> = rows
        .into_iter()
        .map(|(img, thumb)| match thumb {
            Some((t, decoded)) => {
                decoded_thumbs.push(Some(decoded));
                (img, Some(t))
            }
            None => {
                decoded_thumbs.push(None);
                (img, None)
            }
        })
        .collect();

    let ids = match writer
        .lock()
        .expect("writer")
        .upsert_images_with_thumbnails_batch(&batch_input)
    {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!(
                "ferrolite: batched ingest write failed ({} rows): {e}",
                batch_input.len()
            );
            return;
        }
    };
    if let (Some(t), Some(p)) = (t_batch, profile) {
        p.record_upsert(t.elapsed().as_micros() as u64);
    }

    for (id, decoded) in ids.into_iter().zip(decoded_thumbs) {
        if force {
            kept.insert(id);
        }
        // One ingested row == one thumbnail (when the decode succeeded).
        let _ = tx.send(AppEvent::Indexed { added: 1 });
        if let Some(decoded) = decoded {
            let _ = tx.send(AppEvent::ThumbReady {
                image_id: id,
                rgba: decoded.rgba,
                w: decoded.w,
                h: decoded.h,
            });
        }
    }
    ctx.request_repaint();
}

fn ingest_job(
    folder: PathBuf,
    mode: ReindexMode,
    writer: Arc<Mutex<Catalog>>,
    reads: Arc<ReadPool>,
    tx: Sender<AppEvent>,
    ctx: egui::Context,
    cancel: &CancelToken,
) {
    let profile =
        crate::diag::enabled().then(|| std::sync::Arc::new(crate::diag::IngestProfile::default()));
    let t_job = profile.as_ref().map(|_| std::time::Instant::now());
    // Phase wall-clocks (seconds), filled when profiling.
    let mut scan_s = 0.0f64;
    let mut phase_a_s = 0.0f64;
    let mut filter_s = 0.0f64;
    let mut producer_done_s = 0.0f64;
    // Adaptive read-concurrency gate: caps how many ingest worker threads may be
    // inside the decode's disk read at once, growing/shrinking the cap from
    // observed read latency (see `read_gate` module docs). Sized off the rayon
    // pool the parallel producer below actually runs on. Declared outside the
    // `thread::scope` below so the post-scope `[ingest-concurrency]` emission
    // can read its final snapshot. The gate is functional regardless of diag;
    // only that emission is diag-gated.
    let read_gate = std::sync::Arc::new(crate::read_gate::AdaptiveReadGate::new(
        rayon::current_num_threads().max(1),
    ));
    let mut producer_done_at_s = 0.0f64;
    let mut consumer_done_at_s = 0.0f64;

    if profile.is_some() {
        crate::diag::set_ingest_phase(crate::diag::IngestPhase::Scan);
    }
    let t_scan = profile.as_ref().map(|_| std::time::Instant::now());
    let files = scan_tree(&folder);
    if let Some(t) = t_scan {
        scan_s = t.elapsed().as_secs_f64();
    }
    let file_count = files.len();
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

    // Phase A — instant index. Insert a stat-only `Pending` row for every scanned
    // file not already in the catalog (no file reads: filename/mtime/size/kind
    // come straight from the scan). The grid shows every filename within a moment
    // instead of being gated by the ~metadata rate; dimensions and thumbnails
    // stream in during Phase B below. `insert_pending` leaves already-indexed rows
    // untouched, so re-opening a folder stays instant.
    if profile.is_some() {
        crate::diag::set_ingest_phase(crate::diag::IngestPhase::PhaseA);
    }
    let t_phase_a = profile.as_ref().map(|_| std::time::Instant::now());
    {
        let added_at = now_epoch_secs();
        let mut inserted = 0usize;
        for f in &files {
            if cancel.is_cancelled() {
                return;
            }
            let Some(folder_id) = f.path.parent().and_then(|p| dir_ids.get(p)).copied() else {
                continue;
            };
            let pending = NewImage::pending(
                folder_id,
                f.filename.clone(),
                f.mtime,
                f.size,
                f.kind,
                added_at,
            );
            if writer
                .lock()
                .expect("writer")
                .insert_pending(&pending)
                .is_ok()
            {
                inserted += 1;
                // Refresh the grid periodically so rows appear as they're inserted.
                if inserted.is_multiple_of(256) {
                    let _ = tx.send(AppEvent::Scanned { added: 256 });
                    ctx.request_repaint();
                }
            }
        }
        let _ = tx.send(AppEvent::Scanned {
            added: inserted % 256,
        });
        ctx.request_repaint();
    }
    if let Some(t) = t_phase_a {
        phase_a_s = t.elapsed().as_secs_f64();
    }

    // Streaming ingest. The expensive part — one decode per file yielding BOTH
    // metadata AND the embedded preview, then resize/encode into a thumbnail —
    // runs in parallel in the producer below and feeds finished rows (each with
    // its already-generated thumbnail) over a channel. A single consumer batches
    // the row upsert + thumbnail persist into one transaction every
    // `INGEST_WRITE_BATCH` rows (RC-PERF-3: this replaces two autocommit INSERTs
    // per row — `upsert_image` then `put_thumbnail` — each of which took the
    // writer lock and committed individually). `Indexed`/`ThumbReady` are still
    // emitted per row, right after its batch commits, so the grid fills in
    // progressively instead of staying empty behind a whole-folder barrier.
    // Because the thumbnail is generated inline, the file is opened ONCE — the
    // old separate re-opening Background thumbnail job is gone.
    type Row = (NewImage, Option<(Thumbnail, DecodedThumb)>);
    let (row_tx, row_rx) = std::sync::mpsc::channel::<Row>();
    let mut kept_image_ids: HashSet<i64> = HashSet::new();

    std::thread::scope(|scope| {
        // Consumer: batches row upsert + thumbnail persist into one transaction
        // per `INGEST_WRITE_BATCH` rows. Clones the shareable handles so the
        // originals remain usable for the prune/IngestDone below.
        let consumer = {
            let writer = Arc::clone(&writer);
            let tx = tx.clone();
            let ctx = ctx.clone();
            let profile = profile.clone();
            scope.spawn(move || {
                use std::sync::mpsc::RecvTimeoutError;
                // For Full, collect every present file's id so prune can delete the rest.
                let mut kept: HashSet<i64> = HashSet::new();
                let mut pending: Vec<Row> = Vec::with_capacity(INGEST_WRITE_BATCH);

                // Flush when the batch fills OR when `FLUSH_INTERVAL` elapses,
                // whichever comes first. The time trigger bounds worst-case
                // visibility latency: without it, slow per-file generation (large
                // RAW / slow disk) could leave rows uncommitted — and invisible
                // in the grid — for seconds until 128 accumulate, violating the
                // load-bearing progressive-fill rule. `recv_timeout` lets the
                // loop wake to flush a partial batch instead of blocking forever.
                loop {
                    if cancel.is_cancelled() {
                        break;
                    }
                    match row_rx.recv_timeout(FLUSH_INTERVAL) {
                        Ok(row) => {
                            if let Some(p) = &profile {
                                p.on_recv();
                                crate::diag::note_ingest_chan(p.chan_inflight());
                            }
                            pending.push(row);
                            if pending.len() >= INGEST_WRITE_BATCH {
                                flush_batch(
                                    &writer,
                                    &tx,
                                    &ctx,
                                    force,
                                    &mut pending,
                                    &mut kept,
                                    profile.as_ref(),
                                );
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            // No new row within the interval: commit whatever has
                            // accumulated so the grid keeps filling smoothly.
                            flush_batch(
                                &writer,
                                &tx,
                                &ctx,
                                force,
                                &mut pending,
                                &mut kept,
                                profile.as_ref(),
                            );
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                // Final flush: whatever is left when the channel closes (producer
                // finished) or the loop broke on cancel — either way, already-decoded
                // rows in `pending` must not be silently dropped. `flush_batch` is a
                // no-op on an empty batch, so this is always safe.
                flush_batch(
                    &writer,
                    &tx,
                    &ctx,
                    force,
                    &mut pending,
                    &mut kept,
                    profile.as_ref(),
                );
                kept
            })
        };

        // Determine, up front, exactly which scanned files this pass will process
        // (Incremental skips unchanged files by (mtime,size); Full forces all), so
        // the status bar knows the true denominator. `to_process` pairs each file
        // with its resolved folder_id. Emit the planned total once.
        if profile.is_some() {
            crate::diag::set_ingest_phase(crate::diag::IngestPhase::Filter);
        }
        let t_filter = profile.as_ref().map(|_| std::time::Instant::now());
        let to_process: Vec<(&ferrolite_catalog::ScannedFile, i64)> = files
            .iter()
            .filter_map(|f| {
                let folder_id = f.path.parent().and_then(|p| dir_ids.get(p).copied())?;
                if !force
                    && !matches!(
                        reads.needs_reingest(folder_id, &f.filename, f.mtime, f.size),
                        Ok(true)
                    )
                {
                    return None;
                }
                Some((f, folder_id))
            })
            .collect();
        if let Some(t) = t_filter {
            filter_s = t.elapsed().as_secs_f64();
        }
        let _ = tx.send(AppEvent::IngestPlanned {
            total: to_process.len(),
        });

        // Producer: parallel single-decode ingest. For each file it decodes
        // metadata + preview in ONE pass (`decode_meta_and_preview`) and, on
        // success, generates the thumbnail inline; the finished row + thumbnail is
        // streamed to the consumer. When the parallel pass ends, every cloned
        // sender drops and the channel closes, draining the consumer to completion.
        if profile.is_some() {
            crate::diag::set_ingest_phase(crate::diag::IngestPhase::Decode);
        }
        let t_producer = profile.as_ref().map(|_| std::time::Instant::now());
        to_process.par_iter().for_each_with(
            (row_tx, read_gate.clone()),
            |(sender, read_gate), &(f, folder_id)| {
                if cancel.is_cancelled() {
                    return;
                }
                let added_at = now_epoch_secs();
                let rating =
                    ferrolite_catalog::read_rating(&ferrolite_catalog::sidecar_path(&f.path))
                        .unwrap_or_default();
                let is_raw = matches!(f.kind, ferrolite_catalog::FileKind::Raw);
                let measure = crate::diag::enabled();
                // Hold the permit only across the disk-read-bound decode; release
                // it before the CPU-heavy resize/encode below so the gate caps
                // read concurrency without throttling CPU parallelism. The decode
                // timer starts AFTER the permit is acquired so a worker blocked
                // waiting for a permit is not miscounted as decode time.
                let _permit = read_gate.acquire();
                let t_meta = profile.as_ref().map(|_| std::time::Instant::now());
                let decoded = ferrolite_decode::decode_meta_and_preview(
                    &f.path,
                    f.kind,
                    measure,
                    ferrolite_catalog::THUMB_MAX_EDGE,
                );
                let decode_us = t_meta.map(|t| t.elapsed().as_micros() as u64);
                drop(_permit); // release before generate_thumbnail (CPU work runs permit-free)
                if let (Some(us), Some(p)) = (decode_us, profile.as_ref()) {
                    p.record_decode(us, is_raw);
                }
                match decoded {
                    Ok((meta, preview, info)) => {
                        if let Some(p) = profile.as_ref() {
                            // Count the byte-source tier for every RAW decode (not
                            // just slow ones): grown+full are the files that missed
                            // the 1 MiB prefix (the former slow tail).
                            if let Some(kind) = info.source_kind {
                                p.record_source(kind);
                            }
                            if let Some(us) = decode_us {
                                let decode_ms = us as f64 / 1000.0;
                                if decode_ms >= crate::diag::slow_threshold_ms() {
                                    let to_ms = |d: Option<std::time::Duration>| {
                                        d.map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
                                    };
                                    let sample = crate::diag::SlowSample {
                                        decode_ms,
                                        source_kind: info.source_kind,
                                        source_bytes: info.source_bytes.unwrap_or(0),
                                        source_acquire_ms: to_ms(info.source_acquire),
                                        get_decoder_ms: to_ms(info.get_decoder),
                                        raw_metadata_ms: to_ms(info.raw_metadata),
                                        raw_dims_ms: to_ms(info.raw_dims),
                                        extract_ms: to_ms(info.extract),
                                        orient_ms: to_ms(info.orient),
                                        is_raw,
                                        src_w: info.src_w,
                                        src_h: info.src_h,
                                        source: info.source,
                                        model: meta.model.clone(),
                                        path: f.path.display().to_string(),
                                        std_decode_ms: to_ms(info.std_decode),
                                        dct_scale: info.dct_scale,
                                    };
                                    crate::diag::write_log(&crate::diag::format_slow_line(&sample));
                                    p.record_slow(sample);
                                }
                            }
                        }
                        let new_image = NewImage::from_metadata(
                            folder_id,
                            f.filename.clone(),
                            f.mtime,
                            f.size,
                            &meta,
                            f.kind,
                            rating,
                            added_at,
                        );
                        // Mid-file checkpoint: a slow decode (large/RAW file) can by
                        // itself exceed the bounded shutdown wait in `App::on_exit`.
                        // Re-check cancellation here so a shutdown/reindex request
                        // skips the CPU-heavy resize/encode below instead of paying
                        // for it after the result is already unwanted.
                        if cancel.is_cancelled() {
                            return;
                        }
                        // Resize + JPEG-encode the preview into a thumbnail inline.
                        let t_enc = profile.as_ref().map(|_| std::time::Instant::now());
                        let thumb = match ferrolite_catalog::generate_thumbnail(&preview) {
                            Ok(pair) => Some(pair),
                            Err(e) => {
                                eprintln!(
                                    "ferrolite: thumbnail generation failed for {}: {e}",
                                    f.path.display()
                                );
                                None
                            }
                        };
                        if let (Some(t), Some(p)) = (t_enc, profile.as_ref()) {
                            p.record_encode(t.elapsed().as_micros() as u64);
                        }
                        if let Some(p) = profile.as_ref() {
                            p.on_send();
                            crate::diag::note_ingest_chan(p.chan_inflight());
                        }
                        let _ = sender.send((new_image, thumb));
                    }
                    Err(_) => {
                        let new_image = NewImage::failed(
                            folder_id,
                            f.filename.clone(),
                            f.mtime,
                            f.size,
                            f.kind,
                            added_at,
                        );
                        if let Some(p) = profile.as_ref() {
                            p.on_send();
                            crate::diag::note_ingest_chan(p.chan_inflight());
                        }
                        let _ = sender.send((new_image, None));
                    }
                }
            },
        );
        if let Some(t) = t_producer {
            producer_done_s = t.elapsed().as_secs_f64();
        }
        producer_done_at_s = t_job.map_or(0.0, |t| t.elapsed().as_secs_f64());

        kept_image_ids = consumer.join().expect("ingest consumer thread panicked");
        consumer_done_at_s = t_job.map_or(0.0, |t| t.elapsed().as_secs_f64());
    });

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

    if let (Some(p), Some(t)) = (&profile, t_job) {
        let wall_s = t.elapsed().as_secs_f64();
        let raw = p.raw_samples();
        let std_s = p.std_samples();
        let all = p.decode_samples();
        let us_to_ms = |u: u32| u as f64 / 1000.0;
        let encode_count = all.len().max(1) as f64;
        let summary = crate::diag::IngestSummary {
            files: file_count,
            wall_s,
            scan_s,
            phase_a_s,
            filter_s,
            decode_par_s: producer_done_s,
            decode_sum_s: p.decode_sum_us() as f64 / 1e6,
            cores: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            decode_p50_ms: us_to_ms(crate::diag::percentile(&all, 0.5)),
            decode_p95_ms: us_to_ms(crate::diag::percentile(&all, 0.95)),
            decode_max_ms: p.decode_max_us() as f64 / 1000.0,
            encode_sum_s: p.encode_sum_us() as f64 / 1e6,
            encode_avg_ms: (p.encode_sum_us() as f64 / 1000.0) / encode_count,
            upsert_batches: p.upsert_batches(),
            upsert_avg_ms: if p.upsert_batches() > 0 {
                (p.upsert_sum_us() as f64 / 1000.0) / p.upsert_batches() as f64
            } else {
                0.0
            },
            upsert_sum_s: p.upsert_sum_us() as f64 / 1e6,
            chan_depth_max: p.chan_depth_max(),
            producer_done_s: producer_done_at_s,
            consumer_done_s: consumer_done_at_s,
            raw_count: raw.len(),
            raw_p50_ms: us_to_ms(crate::diag::percentile(&raw, 0.5)),
            std_count: std_s.len(),
            std_p50_ms: us_to_ms(crate::diag::percentile(&std_s, 0.5)),
        };
        crate::diag::emit_ingest_summary(&summary);
        crate::diag::emit_slow_aggregate(&p.slow_samples(), file_count);
        crate::diag::emit_source_split(p.prefix_hits(), p.directed(), p.grown(), p.full());

        // [ingest-concurrency]: the gate doesn't track a hot-path in-flight
        // peak (that would add an extra atomic to every acquire/release just
        // for this one-shot diag line), so `inflight_peak` is best-effort: the
        // final converged limit, which is also the practical upper bound on
        // in-flight reads once the controller has settled.
        let gate_snapshot = read_gate.snapshot();
        crate::diag::emit_ingest_concurrency(&crate::diag::ConcurrencySnapshot {
            limit: gate_snapshot.limit,
            rtt_min_us: gate_snapshot.rtt_min_us,
            rtt_recent_us: gate_snapshot.rtt_recent_us,
            gradient: gate_snapshot.gradient,
            inflight_peak: gate_snapshot.limit,
            pinned: read_gate.is_pinned(),
        });
    }

    if profile.is_some() {
        crate::diag::set_ingest_phase(crate::diag::IngestPhase::Idle);
    }
    let _ = tx.send(AppEvent::IngestDone);
    ctx.request_repaint();
}

/// Headless thumbnail helper: decode preview → resize/encode → persist BLOB.
/// Returns the persisted JPEG [`Thumbnail`] plus the already-resized RGBA8
/// [`DecodedThumb`] so the caller can upload a texture without re-decoding.
///
/// The interactive ingest path no longer calls this — it generates thumbnails
/// inline in the parallel producer (one decode per file). This remains for the
/// `bench_browse` binary and the `ingest_tree` integration test, which exercise
/// the profiled decode+resize+persist pipeline headlessly via the lib target;
/// it is therefore unused in the main app binary (hence `#[allow(dead_code)]`).
#[allow(dead_code)]
pub fn thumbnail_blocking(
    writer: &Arc<Mutex<Catalog>>,
    image_id: i64,
    path: &Path,
    kind: FileKind,
) -> Result<(Thumbnail, DecodedThumb), String> {
    // Opt-in profiling (FERROLITE_DIAG): time the disk read separately from
    // decode/encode. `measure_read` also warms the cache, so the decode timed
    // next reflects CPU only. Off by default with zero overhead.
    let profile = crate::diag::enabled();
    let io_us = if profile {
        crate::diag::measure_read(path)
    } else {
        0
    };

    let t_decode = profile.then(std::time::Instant::now);
    let preview = match kind {
        FileKind::Standard => ferrolite_decode::decode_thumb_source_standard(
            path,
            ferrolite_catalog::THUMB_MAX_EDGE,
            false,
        )
        .map(|d| d.image)
        .map_err(|e| e.to_string())?,
        FileKind::Raw => ferrolite_decode::decode_preview(path, kind).map_err(|e| e.to_string())?,
    };
    let decode_us = t_decode.map_or(0, |t| t.elapsed().as_micros() as u64);

    let t_encode = profile.then(std::time::Instant::now);
    let (thumb, decoded) =
        ferrolite_catalog::generate_thumbnail(&preview).map_err(|e| e.to_string())?;
    let encode_us = t_encode.map_or(0, |t| t.elapsed().as_micros() as u64);

    // Time the writer-lock acquisition + SQLite commit together, so contention on
    // the shared writer and the DB write cost show up separately in the profile.
    let t_write = profile.then(std::time::Instant::now);
    {
        use ferrolite_catalog::ThumbnailStore;
        writer
            .lock()
            .expect("writer")
            .put_thumbnail(image_id, &thumb)
            .map_err(|e| e.to_string())?;
    }
    let write_us = t_write.map_or(0, |t| t.elapsed().as_micros() as u64);

    if profile {
        crate::diag::record_blocking(io_us, decode_us, encode_us, write_us);
    }
    Ok((thumb, decoded))
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
