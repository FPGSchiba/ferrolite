# Cancel Off-Screen Thumbnail Fetches Plan (Round 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`).

**Goal:** Cancel lazy-load thumbnail fetch jobs when their cells scroll out of view (and at shutdown), so scrolling fully down then back up loads the now-visible cells immediately instead of grinding through a stale backlog — and so closing after such a scroll is prompt.

**Root cause (confirmed, HEAD 4ce6da9):** `request_thumbnail` (state.rs:273-327) submits a `Visible` job and **discards the returned `JobHandle`** — nothing can cancel a fetch. The grid computes `now_visible` (grid.rs:76-83) but uses it only for tag prefetch; the old visibility-reprioritize pass was removed in R2. So scrolling enqueues one `Visible` job per `Done` cell passed, none are cancelled on scroll-out, and the whole queue must drain. Scrolling back up, the wanted cells queue *behind* the backlog (don't load), and the backlog's completion flood (→ `pending_uploads` + per-frame repaint) saturates the UI (→ close hangs).

**Fix:** track each lazy-load job's `JobHandle` by `image_id`; every frame, cancel + drop the handles for ids no longer visible (removing them from the queue and clearing the in-flight/pending guards so they can be re-requested if scrolled back); also cancel all lazy-load handles in `cancel_pending_jobs` (which `on_exit` already calls) so close is prompt.

**Tech Stack:** Rust, egui/eframe 0.29.1, ferrolite-app, ferrolite-jobs.

## Global Constraints
- Never block the UI thread; grid stays virtualized. The per-frame cancel is O(tracked fetches) = O(recently-visible cells), not O(all-images).
- Must NOT cancel jobs for still-visible cells; a cancelled (scrolled-off) cell must be re-requestable when scrolled back into view (clear it from `thumb_pending`/handles, and do NOT add it to `thumb_missing`).
- Reuse `ferrolite_jobs::JobSystem::cancel(JobId)` (drops a still-queued job from the queue) + `JobHandle::cancel()` (cooperative flag for an in-flight one). `JobHandle` is `Clone` with `.id()`.
- No unwrap/expect outside tests except existing idioms. fmt + clippy --workspace --all-targets -D warnings clean.
- Gate green → hold for Jann's visual test (scroll down-full then up-full → visible thumbs load promptly; close prompt afterwards) before finishing.
- Branch: fix/thumbnail-and-shutdown (continues R1-R3).

---

## Task 1: Cancel off-screen (and shutdown) lazy-load thumbnail fetches

**Files:** Modify `ferrolite-app/src/state.rs`, `ferrolite-app/src/events.rs`, `ferrolite-app/src/library/grid.rs`.

**Interfaces:**
- Add `AppState.thumb_handles: HashMap<i64, ferrolite_jobs::JobHandle>` — the in-flight lazy-load fetch handles, keyed by image_id.
- Add `AppState::retain_visible_thumbnail_jobs(&mut self, visible: &HashSet<i64>)` — cancels + drops every tracked fetch whose id is not in `visible`.

- [ ] **Step 1:** Add the field `thumb_handles: HashMap<i64, ferrolite_jobs::JobHandle>` to `AppState` (init empty in `new` + `for_test`; clear in `reset_for_new_folder`). Verify `JobHandle` is exported from `ferrolite_jobs` and is `Clone`.

- [ ] **Step 2:** In `request_thumbnail` (state.rs:288-327), capture the submitted handle and store it:
```rust
        self.thumb_pending.insert(image_id);
        let reads = Arc::clone(&self.reads);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let handle = self
            .jobs
            .submit(ferrolite_jobs::Priority::Visible, move |cancel| { /* unchanged body */ });
        self.thumb_handles.insert(image_id, handle);
```
(The fast-path `thumb_pixels` hit and the early-return guards above are unchanged — they return before submitting.)

- [ ] **Step 3:** Remove the handle when a fetch completes. In `events.rs`, in the `ThumbReady`, `ThumbFailed`, and `ThumbMissing` folds, also `self.thumb_handles.remove(&image_id);` (alongside the existing `thumb_pending.remove`). This keeps the map bounded to genuinely in-flight fetches.

- [ ] **Step 4:** Add the retain helper to `impl AppState`:
```rust
    /// Cancel and drop lazy-load thumbnail fetches whose cells are no longer
    /// visible, so a big scroll doesn't leave a stale backlog that blocks the
    /// now-visible cells (and saturates the UI at close). Cancelled ids are
    /// removed from the in-flight guards so they can be re-requested if scrolled
    /// back into view; they are NOT marked missing.
    pub fn retain_visible_thumbnail_jobs(&mut self, visible: &std::collections::HashSet<i64>) {
        let offscreen: Vec<i64> = self
            .thumb_handles
            .keys()
            .copied()
            .filter(|id| !visible.contains(id))
            .collect();
        for id in offscreen {
            if let Some(handle) = self.thumb_handles.remove(&id) {
                self.jobs.cancel(handle.id()); // drop it from the queue if still pending
                handle.cancel();               // signal it if already running
            }
            self.thumb_pending.remove(&id);
        }
    }
```
Note: a cancelled-but-already-running job will still send `ThumbReady`/`ThumbFailed`/`ThumbMissing` when it finishes (its body checks `cancel.is_cancelled()` and sends `ThumbFailed`); those folds are idempotent on the now-absent `thumb_handles`/`thumb_pending` entries, and a `ThumbReady` that still arrives for a briefly-offscreen cell simply uploads a usable texture — harmless.

- [ ] **Step 5:** Call it once per frame from the grid. In `grid.rs`, inside the `scroll.show_viewport` closure, right after `state.ensure_tags_for(&now_visible);` (grid.rs:83) — OR after the cell paint loop — add:
```rust
        state.retain_visible_thumbnail_jobs(&now_visible);
```
(`now_visible` already holds exactly the currently-visible ids. Placing it after `ensure_tags_for` is fine; the paint loop below then (re)issues `request_thumbnail` only for visible cells, which are retained.)

- [ ] **Step 6:** Cancel lazy-load fetches at shutdown too. In `AppState::cancel_pending_jobs` (the fn `on_exit` calls), after the existing ingest/thumb cancellation, cancel all lazy-load handles:
```rust
        for (_id, handle) in self.thumb_handles.drain() {
            self.jobs.cancel(handle.id());
            handle.cancel();
        }
        self.thumb_pending.clear();
```
So a close right after a big scroll doesn't wait on a backlog of `Visible` fetches. (Verify the exact current body of `cancel_pending_jobs`; add without breaking the ingest-handle cancellation.)

- [ ] **Step 7: Test.** Add a unit test for `retain_visible_thumbnail_jobs`: seed `thumb_pending` + `thumb_handles` with a few ids by calling `request_thumbnail` (or by directly inserting a real submitted handle via a test `JobSystem`), then call `retain_visible_thumbnail_jobs(&{subset})` and assert: (a) ids not in the visible subset are removed from BOTH `thumb_handles` and `thumb_pending`; (b) ids in the subset are retained; (c) the job queue shrank for the cancelled ones (`jobs.pending_count()` reflects the drop, if a real `JobSystem` is used). Use the existing test helpers/`for_test` pattern; if submitting real jobs in a test is awkward, insert handles obtained from a small local `JobSystem::new(1)` submitting no-op closures.

- [ ] **Step 8: Gate.** `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test -p ferrolite-app` green; `cargo fmt` + `cargo fmt --check` clean. NOTE: a stray test binary (PID 33572) may lock the default target dir → on `LNK1104: cannot open ...ferrolite_app-<hash>.exe`, re-run with `CARGO_TARGET_DIR=C:/Users/JANNER~1/AppData/Local/Temp/claude/c--Users-JannErhardt-Projects-ferrolite/4afe879f-a04e-40d1-a0bc-6959481b052f/scratchpad/gate-target` (do NOT kill the process); report if you did.

- [ ] **Step 9: Commit.** `fix(app): cancel off-screen (and shutdown) lazy-load thumbnail fetches`.

---

## Final gate + hold
- [ ] fmt + clippy + `cargo test --workspace` green.
- [ ] STOP and hold for Jann's visual test: scroll fully down then fully up → the now-visible thumbnails load promptly (no waiting behind a stale backlog); the counter stays sane; closing right after such a scroll is prompt (no "Not Responding").

## Self-Review
Coverage: root cause (uncancelled off-screen fetches → backlog) → tracked handles + per-frame `retain_visible_thumbnail_jobs` (Steps 1-5) + shutdown drain (Step 6). Preserves: still-visible cells keep their jobs; scrolled-back cells re-request (cleared from pending, not marked missing); the R3 `thumb_missing`/`Done`-guard storm fix is untouched. Types: `thumb_handles: HashMap<i64, JobHandle>` mirrors `thumb_pending`; `JobHandle` is Clone with `.id()`/`.cancel()`; `JobSystem::cancel(JobId)` drops queued jobs. Risk: a cancelled-then-completing job still emits an event — handled idempotently (folds remove absent keys; a late `ThumbReady` just uploads a valid texture).
