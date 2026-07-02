# Investigation R2 — thumbnail generation speed + shutdown/scroll/counter/sort follow-ups (2026-07-02)

> **Status:** Root cause established (systematic-debugging Phase 1). No fixes applied yet.
> **Branch:** `fix/thumbnail-and-shutdown`. Builds on the R1 fixes (commits 531e931 shutdown, 91324bb pixel cache, db62a8f counter) which Jann visual-tested and found incomplete.
> **Trigger:** Jann's hands-on test with ~3320 real RAW images: (1) thumbnails don't reappear instantly on scroll; (2) counter flickers Idle↔generating during reindex; (3) shutdown STILL hangs "Not Responding" while thumbnailing; (4) newly-thumbnailed images sort to the bottom + too little progress feedback; (5) generation takes 5+ min for 3320 images — "can we speed it up? I think the meta read is the bottleneck."

Three parallel read-only investigations. All claims confirmed from code unless labeled hypothesis.

---

## HEADLINE — why generation is slow (#5, #1)

### RC-PERF-1 (primary, confirmed): every RAW is opened and rawler-parsed TWICE
- **Metadata pass** (ingest producer, rayon-parallel): [ingest.rs:318](../../../ferrolite-app/src/ingest.rs#L318) `files.par_iter()` → [ingest.rs:335](../../../ferrolite-app/src/ingest.rs#L335) `read_metadata` → `read_metadata_raw` ([decode/lib.rs:58](../../../ferrolite-decode/src/lib.rs#L58)) → `with_ingest_source` → `read_prefix` (fresh **1 MiB** read, [decode/source.rs:22](../../../ferrolite-decode/src/source.rs#L22)) → `rawler::get_decoder` → `raw_metadata` + `raw_image(dummy)`.
- **Preview pass** (Background thumbnail job, later): [ingest.rs:404](../../../ferrolite-app/src/ingest.rs#L404) `decode_preview` → `decode_preview_raw` ([decode/preview.rs:10](../../../ferrolite-decode/src/preview.rs#L10)) → `with_ingest_source` **again** → another **1 MiB** `read_prefix` → another `get_decoder` → `preview_image` + a **third** redundant `raw_metadata` call just for orientation ([decode/preview.rs:23](../../../ferrolite-decode/src/preview.rs#L23)).
- The two passes share nothing and are decoupled in time, so the second 1 MiB read is usually cache-cold. For 3320 RAWs: ~6640 opens, ~6.6 GB of prefix reads, 3320 redundant container parses. **This is the primary bottleneck.** The user's "meta read is the bottleneck" is directionally right but mis-framed — metadata is already parallel; the waste is the *double open + double parse*, not serial metadata.
- **Fix:** read metadata + preview in ONE `get_decoder` pass (or at minimum cache & reuse the 1 MiB prefix `Vec<u8>` from pass 1 in pass 2). Removes ~half the decode-side I/O + parsing. Highest leverage.

### RC-PERF-2 (secondary, confirmed): reduced + starved thumbnail concurrency during ingest
- Worker pool = `available_parallelism()-1` ([state.rs:174](../../../ferrolite-app/src/state.rs#L174)). The `ingest_job` runs at `Priority::Interactive` and **occupies one worker for the entire scan** (its `thread::scope` blocks on `consumer.join()`, [ingest.rs:271-363](../../../ferrolite-app/src/ingest.rs#L271)) → only N-1 workers left for thumbnails.
- The rayon metadata `par_iter` ([ingest.rs:318](../../../ferrolite-app/src/ingest.rs#L318)) runs on rayon's **own global pool** (all cores), competing with the job-pool workers for CPU + disk during the exact window thumbnails want to run.
- Thumbnails submit at `Priority::Background` ([ingest.rs:444](../../../ferrolite-app/src/ingest.rs#L444)); `pop_highest` ([jobs/queue.rs:53](../../../ferrolite-jobs/src/queue.rs#L53)) is strict priority, so any Interactive/Visible work (incl. lazy-load Visible thumbnails, folder switches) preempts them.
- **Fix:** unify the two pools or bound rayon; don't let ingest hog a worker; consider reserving Background headroom or promoting ingest thumbnails.

### RC-PERF-3 (tertiary, confirmed): per-image, unbatched, single-mutex DB writes
- Every `thumbnail_blocking` ends with `writer.lock().put_thumbnail(...)` ([ingest.rs:417](../../../ferrolite-app/src/ingest.rs#L417)) on one `Arc<Mutex<Catalog>>`; `put_thumbnail` is a single autocommit `INSERT` ([catalog/thumbnail.rs:106](../../../ferrolite-catalog/src/thumbnail.rs#L106)). The consumer's per-row `upsert_image` ([ingest.rs:287](../../../ferrolite-app/src/ingest.rs#L287)) contends on the **same** lock → metadata + thumbnail writes serialize. No transaction batching (the pattern exists elsewhere: `unchecked_transaction()` at [catalog.rs:164](../../../ferrolite-catalog/src/catalog.rs#L164)).
- **Fix:** batch writes in transactions (e.g. dedicated writer thread draining a channel, committing every ~64-256 rows), flushed on ingest end/cancel.

Per-thumbnail decode uses the **embedded preview** (not demosaic) — [decode/preview.rs:15](../../../ferrolite-decode/src/preview.rs#L15) `preview_image` first. Good; not a bottleneck. `needs_reingest` correctly skips unchanged files ([queries.rs:84](../../../ferrolite-catalog/src/queries.rs#L84)) — re-open is cheap. (Exact decode/encode/IO split is measurable via `FERROLITE_PROFILE_THUMBS`.)

---

## #3 Shutdown still hangs (confirmed root cause)
- `on_exit` **is** called by eframe 0.29.1 wgpu — confirmed in vendored source: `wgpu_integration.rs:502-503` calls `self.app.on_exit()` (no-arg, glow-off variant, matching [app.rs on_exit](../../../ferrolite-app/src/app.rs)) synchronously inside `save_and_destroy()`, on the UI/event-loop thread, then `run.rs:142` `std::process::exit(0)`. Our fix runs. `Drop for JobSystem` is NOT reached (`process::exit` skips destructors).
- **The gap:** `cancel_pending_jobs` ([state.rs:462](../../../ferrolite-app/src/state.rs#L462)) only removes *queued* jobs (`Queue::cancel` is a map-remove, [jobs/queue.rs:48](../../../ferrolite-jobs/src/queue.rs#L48)); a job already dispatched to a worker is gone from the pending map and unaffected. `thumbnail_blocking` ([ingest.rs:387-429](../../../ferrolite-app/src/ingest.rs#L387)) has **no mid-job cancel checkpoint** (only one check before it starts, [ingest.rs:444](../../../ferrolite-app/src/ingest.rs#L444)). So with N workers mid-thumbnail at close, `join_with_timeout` blocks the UI thread for the full 500 ms budget — and a ~500 ms synchronous freeze of the sole event-loop thread is enough for Windows to flag "Not Responding" on essentially every close during bulk thumbnailing.
- **Fix direction:** (1) add mid-job cancel checkpoints in `thumbnail_blocking` (check between decode/encode/write); (2) don't block the UI thread on join at all — `request_shutdown` + cancel, then return and let eframe's `process::exit(0)` reclaim threads (SQLite WAL + `synchronous=NORMAL`, [catalog.rs:22](../../../ferrolite-catalog/src/catalog.rs#L22), is crash-safe against a killed mid-write; the worst case is a missing thumbnail, regenerated later). Optionally a tiny (~50 ms) grace wait. This makes close near-instant.

---

## #2 Counter flicker (confirmed; narrower race than R1 bug)
- R1's `thumb_jobs`-scoped `thumb_done` (db62a8f) fixed the lazy-load inflation. Residual flicker: `thumb_total` grows **per-file** as ingest discovers files (`ThumbRegistered`, [events.rs:135](../../../ferrolite-app/src/events.rs#L135), fired in the consumer loop [ingest.rs:301](../../../ferrolite-app/src/ingest.rs#L301)) while `thumb_done` chases it; a burst of fast completions can make `thumb_done == stale thumb_total` for a frame → `activity_text`'s `done>=total` branch shows "Idle", then flips back. 
- **Fix:** gate `activity_text` on the existing stable `active_ingests: usize` ([state.rs:77](../../../ferrolite-app/src/state.rs#L77), ++ per ingest spawn, -- on `IngestDone`) — show "Generating…" for the whole ingest duration instead of the frame-by-frame `done/total` comparison. `active_ingests` only flips at ingest start/end → no flicker.

---

## #4 Sort order + feedback (confirmed)
- Default sort is `SortKey::CaptureTime, ASC` ([catalog/query.rs:64](../../../ferrolite-catalog/src/query.rs#L64), [filter.rs:61](../../../ferrolite-app/src/library/filter.rs#L61)) → `ORDER BY capture_time ASC` where `capture_time` = EXIF `DateTimeOriginal` ([decode/lib.rs:80](../../../ferrolite-decode/src/lib.rs#L80)). So imports land by **shoot date**, not ingest recency — never predictably "at the top" where a user watching a live scan expects new activity.
- A purpose-built `ViewSource::RecentlyAdded` → `ORDER BY added_at DESC` ([query.rs:104](../../../ferrolite-catalog/src/query.rs#L104)) exists and is UI-wired ([panel.rs:28](../../../ferrolite-app/src/library/panel.rs#L28)) but isn't the default.
- Feedback during ingest is **status-text only** ([status_bar.rs](../../../ferrolite-app/src/library/status_bar.rs)); no progress bar anywhere. Per-cell `cell_state` has 3 states ([cell_state.rs:12](../../../ferrolite-app/src/library/cell_state.rs#L12)); a generating cell is a flat gray `Placeholder` ([grid.rs:257](../../../ferrolite-app/src/library/grid.rs#L257)) **indistinguishable** from an untouched one.
- **Fix options:** a 4th `Generating` cell state (spinner/shimmer while `thumb_pending`/`thumb_jobs` holds the id); an `egui::ProgressBar` in the status bar bound to `active_ingests`-gated `done/total`; and/or make in-progress work visible via ordering (default a fresh ingest toward `added_at`, or surface "N generating").

---

## #1 Scroll not instant (confirmed: not a bug)
- The `ThumbPixelCache` fast path is correctly implemented and unconditionally checked before job submission ([state.rs:266-311](../../../ferrolite-app/src/state.rs#L266)). "Not instant" is cold-cache + cap math: cap 1024 vs 3320 images (GPU texture cap 512), so scrolling past the window evicts, and a re-reveal beyond it is a full job (DB read + decode). First-ever reveals are always a job. The cache is session-only, warmed lazily.
- **Fix:** a bounded **background warm-load of decoded thumbnails from the persisted `thumbnails` table** for the current view (off-thread), so first reveals feel instant too — not just re-reveals. Largely downstream of the perf fix (once generation is fast, cold reveals are cheap anyway).

---

## Relationship / sequencing
RC-PERF-1/2/3 are the headline (Jann's explicit ask). #3 shutdown is a real always-repro bug. #2 counter is a cheap one-line-ish gate. #4 feedback + #1 warm-load are UX polish that partly resolve once generation is fast. Scope/priority to be decided with Jann.
