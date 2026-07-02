# Investigation — thumbnail counter / slow scroll / shutdown hang (2026-07-02)

> **Status:** Root cause established (systematic-debugging Phase 1 complete). No fixes applied yet.
> **Branch:** `feat/edited-thumbnails` (investigated here because it's adjacent to the edited-thumbnail work; the bugs themselves are pre-existing and independent of that feature's design).
> **Reported by:** Jann, testing with ~3320 real RAW images. Symptoms: status line `Thumbnails 121351/0 · 0 / 3320 indexed` with the first number climbing on scroll into unloaded regions; thumbnails load slowly "even though they should all be generated"; closing the app always leaves it "Not Responding" on Windows.

Method: two parallel read-only code investigations, evidence traced backward from each symptom. All claims cite file:line in the code as read on this branch.

---

## Bug A — misleading `Thumbnails <done>/<total>` counter (cosmetic/confusing)

**What the status shows:** `activity_text` formats `"Thumbnails {thumb_done}/{thumb_total}"` ([status_bar.rs:6-17](../../../ferrolite-app/src/status_bar.rs#L6)); the `N / M indexed` field is `state.indexed / state.scanned` ([status_bar.rs:29](../../../ferrolite-app/src/status_bar.rs#L29)). So `121351/0 · 0/3320` = `thumb_done=121351`, `thumb_total=0`, `indexed=0`, `scanned=3320`.

**Root cause (confirmed):**
- `thumb_done` is incremented on **every** `ThumbReady` **and** `ThumbFailed` fold, with no ceiling and no de-dup, regardless of the job's origin ([events.rs:119,125](../../../ferrolite-app/src/events.rs#L119)). The lazy grid-scroll load path (`request_thumbnail`) produces `ThumbReady`/`ThumbFailed` too, so every scroll-triggered load bumps it.
- `thumb_total` is only incremented via the **ingest-only** `ThumbRegistered` event ([events.rs:130-134](../../../ferrolite-app/src/events.rs#L130), fired once per image at [ingest.rs:302-306](../../../ferrolite-app/src/ingest.rs#L302)), and is zeroed only in `reset_for_new_folder` / full reindex. In the default "All Photographs" view (`source: ViewSource::All`, [state.rs:206](../../../ferrolite-app/src/state.rs#L206)) with images already ingested in a prior session, **no ingest runs**, so `thumb_total` stays at its `AppState::new()` value of `0` ([state.rs:180-183](../../../ferrolite-app/src/state.rs#L180)). Same reason `indexed` shows `0`.
- Net: the counters are ingest-session-scoped but displayed in a view where no ingest ran, and `thumb_done` conflates one-time ingest generation with repeated lazy-load re-decodes → the runaway `121351/0`.

## Bug B — slow thumbnails + the counter climb (real perf issue; drives Bug A's climb)

**Root cause (confirmed):** the in-memory LRU **texture cache is capped at 512** ([state.rs:186](../../../ferrolite-app/src/state.rs#L186), [texture_cache.rs:54](../../../ferrolite-app/src/library/texture_cache.rs#L54)) while the library holds **3320** images. Scrolling past ~512 unique cells evicts LRU textures ([texture_cache.rs:72-81](../../../ferrolite-app/src/library/texture_cache.rs#L72)); re-revealing an evicted cell finds `textures.contains(id)==false`, so `paint_cell` re-calls `request_thumbnail` ([grid.rs:228-232](../../../ferrolite-app/src/library/grid.rs#L228)), which spawns a **fresh job** each time.

Nuance: thumbnails **are** persisted (`put_thumbnail`, [thumbnail.rs:106-122](../../../ferrolite-catalog/src/thumbnail.rs#L106)) and **are** read back (`request_thumbnail` → `get_thumbnail`, [state.rs:270](../../../ferrolite-app/src/state.rs#L270), [queries.rs:107-124](../../../ferrolite-catalog/src/queries.rs#L107)) — so it is **not** a full RAW re-decode. It is redundant **JPEG-blob decode + job scheduling + texture upload** churn on every re-reveal, plus each completion bumps `thumb_done` (Bug A). The only miss-guard is in-memory `textures`/`thumb_pending` ([state.rs:256-258](../../../ferrolite-app/src/state.rs#L256)), which the undersized LRU keeps invalidating; there is no de-dup of "already decoded once" and no cache of decoded thumbnail bytes.

**Hypothesis (needs runtime confirmation):** which stage dominates the "slow" feel (JPEG decode vs jobs scheduling/channel latency vs SQLite blob-read contention on the shared `ReadPool`). The opt-in `FERROLITE_PROFILE_THUMBS` diagnostic ([thumb_profile.rs:64-88](../../../ferrolite-app/src/thumb_profile.rs#L64), wired [app.rs:1324-1330](../../../ferrolite-app/src/app.rs#L1324)) can attribute it.

## Bug C — app "Not Responding" on close (real, UI-blocking)

**Root cause (confirmed):** there is **no exit/shutdown hook** anywhere in `ferrolite-app` — no `on_exit`, no `on_close_event`, no `Drop for FerroliteApp`/`AppState` (grepped; [main.rs:19-40](../../../ferrolite-app/src/main.rs#L19), [app.rs:6-22](../../../ferrolite-app/src/app.rs#L6), [app.rs:1169](../../../ferrolite-app/src/app.rs#L1169)). When the window closes, eframe drops `FerroliteApp` **on the UI/main thread**, dropping the last `Arc<JobSystem>` and running `Drop for JobSystem` ([system.rs:106-114](../../../ferrolite-jobs/src/system.rs#L106)):

```rust
fn drop(&mut self) {
    self.shared.shutdown.store(true, Ordering::SeqCst);
    self.shared.cvar.notify_all();
    for w in self.workers.drain(..) { let _ = w.join(); }   // unbounded, on the UI thread
}
```

- The `shutdown` flag is only checked **between** dequeues in the worker loop ([system.rs:116-144](../../../ferrolite-jobs/src/system.rs#L116), check at :121); an already-popped job runs to completion first. Thumbnail jobs re-check cancel only **once**, before `thumbnail_blocking` ([ingest.rs:444-448](../../../ferrolite-app/src/ingest.rs#L444)) — no mid-job checkpoint across RAW decode → encode → `writer.lock()` DB write ([ingest.rs:387-429](../../../ferrolite-app/src/ingest.rs#L387)).
- `AppState::cancel_pending_jobs()` ([state.rs:433-445](../../../ferrolite-app/src/state.rs#L433)) exists but is **never called at exit** (only from `reset_for_new_folder` and `spawn_reindex`).
- Worse, long-running ingest jobs hold their own `Arc<JobSystem>` clone for the job's whole duration ([ingest.rs:60-86,271-363](../../../ferrolite-app/src/ingest.rs#L60)), so if an ingest/rescan is in flight at close, the refcount can't reach zero and `Drop` can't even **begin** until it finishes (a whole-tree rescan of ~3320 files).
- **Additional confirmed holder:** the viewer's `VirtualTexture` stores a long-lived `Arc<JobSystem>` clone as a struct field ([view.rs:95,131](../../../ferrolite-vt/src/view.rs#L95), constructed via [app.rs:523](../../../ferrolite-app/src/app.rs#L523) → [view.rs:1078-1081](../../../ferrolite-vt/src/view.rs#L1078)), living inside the wgpu render state's `callback_resources` — a **separate ownership chain** from `AppState`. `Drop for JobSystem` fires only when the **last** `Arc` clone drops, so the VT/renderer must be torn down first; any exit path must account for this holder, not just `AppState.jobs`.
- Lazy-load `request_thumbnail` jobs are `Priority::Visible` and are **never inserted into `thumb_jobs`** ([state.rs:255-292](../../../ferrolite-app/src/state.rs#L255)), so even if `cancel_pending_jobs()` were called at exit it could not cancel them.
- **`121351` is confirmed NOT from ingest:** the ingest path spawns exactly one thumbnail job per non-failed row, gated by `needs_reingest` ([ingest.rs:301-307](../../../ferrolite-app/src/ingest.rs#L301), [queries.rs:84-105](../../../ferrolite-catalog/src/queries.rs#L84)) — it cannot produce 100k+ jobs from 3320 files. The figure is the lazy-load/scroll re-decode loop (Bug A + Bug B), also fed by filmstrip ([filmstrip.rs:65](../../../ferrolite-app/src/library/filmstrip.rs#L65)) and the export queue ([queue_list.rs:201](../../../ferrolite-app/src/export_module/queue_list.rs#L201)) calling `request_thumbnail`.
- The blocking `.join()` runs on the UI thread with no timeout → the Win32 message pump stalls → "Not Responding". Not a hard deadlock (it's a `Mutex<Queue>`+`Condvar`, not a hung `recv`; lock critical sections are short, no AB-BA), but an unbounded UI-thread wait.

**Backlog proportionality:** the join does **not** drain the full queue (unpopped jobs are abandoned at process exit) — hang ≈ `worker_count × (time to finish one in-flight job)`, unless an ingest job is still holding the `Arc`. So shrinking the thumbnail backlog only reduces how many threads are simultaneously mid-job; the structural fix is a real exit path.

**Proper fix shape (for planning):** an eframe exit path that (1) calls `cancel_pending_jobs()` / sets `shutdown` and cancels pending work *before* the final `Arc<JobSystem>` drop, (2) does not block the UI thread unboundedly (bounded-timeout join, or join off the UI thread, or detach), and (3) adds a coarse mid-job cancel checkpoint so an in-flight thumbnail/ingest job can bail promptly; and ensure no long-lived `Arc<JobSystem>` clone is held by an ingest job in a way that blocks `Drop`.

---

## Relation to the edited-thumbnail feature
All three are **pre-existing** defects in the base thumbnail/jobs subsystem, independent of the edited-thumbnail regeneration design. But they touch the same code the feature builds on, and Bug C in particular makes the feature's own visual-test loop (edit → leave → close → reopen) painful. Scope/sequencing to be decided with Jann.

## Unconfirmed side-note
The shutdown investigation flagged (not confirmed) that overlapping startup-rescan passes + the periodic watcher scan ([ingest.rs:161-181](../../../ferrolite-app/src/ingest.rs#L161), [app.rs:1336-1357](../../../ferrolite-app/src/app.rs#L1336)) could resubmit ingest/thumbnail jobs for files already in flight. The **confirmed** explanation for `121351` is Bug A + Bug B (counter design × eviction churn); this side-note is a separate, lower-confidence hypothesis worth a runtime check only if the counter climbs even without scrolling.
