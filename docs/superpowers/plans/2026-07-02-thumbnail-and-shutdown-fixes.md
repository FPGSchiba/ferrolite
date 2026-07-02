# Thumbnail & Shutdown Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix three pre-existing defects in the base thumbnail/jobs subsystem: (C) the app hangs "Not Responding" on close, (B) thumbnails re-decode/re-spawn jobs on every scroll into an evicted region, and (A) the status counter shows a misleading runaway value like `Thumbnails 121351/0`.

**Architecture:** (C) Add a cooperative, bounded shutdown to `ferrolite-jobs::JobSystem` and drive it from an eframe close hook so the UI thread never blocks unboundedly on worker joins. (B) Add a session-only CPU-side LRU of decoded thumbnail pixels so a re-revealed grid cell re-uploads its texture directly — no new job, no DB read, no JPEG re-decode. (A) Scope the thumbnail progress counter to actual ingest generation (de-duplicated) and stop the status bar from showing misleading session-scoped fractions when no generation is running.

**Tech Stack:** Rust, egui/eframe 0.29.1, `ferrolite-jobs` (own fixed-size worker pool + priority queue), `ferrolite-app` Library UI + catalog plumbing.

**Root-cause reference:** [docs/superpowers/investigations/2026-07-02-thumbnail-and-shutdown-bugs.md](../investigations/2026-07-02-thumbnail-and-shutdown-bugs.md). Read it — every task below traces back to a confirmed finding there.

## Global Constraints

- **egui/eframe 0.29.1.** Verify any framework API (e.g. the exact `eframe::App::on_exit` signature for this wgpu build, or `close_requested()`) against the vendored source / `Cargo.lock` before relying on it — do not guess.
- **Never block the UI/update thread (CLAUDE.md, load-bearing).** All decode/DB/file/GPU-heavy work stays on `ferrolite-jobs` workers. The shutdown join on the UI thread MUST be bounded (≤ ~500 ms) and detach on timeout. The grid stays virtualized — no new per-frame O(all-images) work.
- **Immutable-by-default, typed.** No `unwrap()`/`expect()` outside tests and outside the *existing* established `.lock().expect("…")` mutex idiom in this codebase (match the surrounding style; do not add new `expect`s in new logic paths where a graceful path exists).
- **Reuse existing plumbing:** the `ThumbReady` event + `AppState::upload_thumbnail` (state.rs:228) + the `MAX_THUMB_UPLOADS_PER_FRAME` upload budget (app.rs:1197-1315) are the ONLY UI-thread texture-upload path; route cache hits through it, don't add a second upload path. Persisted `thumbnails` table (`put_thumbnail`/`get_thumbnail`) stays the source of truth; new caches are session-only accelerators.
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean.
- **Gate (necessary, not sufficient):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → then **hold for Jann's hands-on visual test** (prompt close with a large library + in-flight ingest; scroll back-and-forth without the counter climbing or thumbnails re-loading slowly; sensible status text) before finishing the branch.
- **Branch:** `fix/thumbnail-and-shutdown` (off `main`, already created; investigation doc already committed).

---

## File Structure

**Created:**
- `ferrolite-app/src/library/thumb_pixel_cache.rs` — the session-only CPU-side LRU of decoded thumbnail pixels (`ThumbPixelCache`). Pure, unit-tested (Task 2).

**Modified:**
- `ferrolite-jobs/src/system.rs` — `workers` behind a `Mutex`; add `request_shutdown`, `is_shutting_down`, `join_with_timeout`; make `Drop` reuse them (Task 1).
- `ferrolite-app/src/app.rs` — drive shutdown from the eframe close hook; wire the pixel-cache hit path into the per-frame upload loop (Tasks 1, 2).
- `ferrolite-app/src/state.rs` — add the `ThumbPixelCache` field + its wiring in `request_thumbnail` and `upload_thumbnail`; counter de-dup support (Tasks 2, 3).
- `ferrolite-app/src/events.rs` — scope `thumb_done` to tracked ingest thumbnails only (Task 3).
- `ferrolite-app/src/status_bar.rs` — `activity_text` / status formatting so it never shows a misleading `X/0` (Task 3).

---

## Task 1: Graceful, bounded shutdown (Bug C)

**Files:**
- Modify: `ferrolite-jobs/src/system.rs`
- Modify: `ferrolite-app/src/app.rs` (eframe close hook)
- Verify (may modify): `ferrolite-app/src/ingest.rs` (ingest cancel-checkpoint granularity)

**Interfaces:**
- Produces:
  - `JobSystem::request_shutdown(&self)` — sets the shutdown flag + `notify_all()`; workers stop pulling new jobs. Idempotent, `&self`.
  - `JobSystem::is_shutting_down(&self) -> bool` — for long job bodies to poll at checkpoints.
  - `JobSystem::join_with_timeout(&self, timeout: std::time::Duration) -> bool` — joins all workers off the calling thread, waits up to `timeout`; returns `true` if all joined, `false` on timeout (remaining handles are detached — reclaimed at process exit). `&self` (drains handles from an internal `Mutex`).
- Consumes: existing `Shared.shutdown: AtomicBool`, `Shared.cvar`, `CancelToken`, `AppState::cancel_pending_jobs` (state.rs:443).

**Design notes (confirmed against source):** `JobSystem` currently holds `workers: Vec<JoinHandle<()>>` and only `Drop` sets `shutdown` + joins — unbounded, on whatever thread drops the last `Arc<JobSystem>` (the UI thread, since eframe drops the app there; investigation §C). We move `workers` behind a `Mutex` so a `&self` method can drain them, add an explicit bounded shutdown, and call it from the eframe close hook BEFORE the implicit drop. Because workers are already stopped by then, the later `Drop` (and the lingering `Arc<JobSystem>` clone held by the viewer's `VirtualTexture`, view.rs:95) join instantly.

- [ ] **Step 1: Write the failing tests in `ferrolite-jobs/src/system.rs`.** Add to the existing `#[cfg(test)] mod tests`:
```rust
    #[test]
    fn join_with_timeout_returns_true_when_idle() {
        let sys = JobSystem::new(2);
        sys.request_shutdown();
        assert!(sys.join_with_timeout(Duration::from_secs(5)));
    }

    #[test]
    fn join_with_timeout_returns_false_when_a_worker_is_busy() {
        let sys = JobSystem::new(1);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        // Occupy the single worker with a job that blocks until released.
        sys.submit(Priority::Background, move |_| {
            let _ = gate_rx.recv(); // never released within the test window
        });
        // Give the worker a moment to pick up the job, then ask to shut down.
        std::thread::sleep(Duration::from_millis(50));
        sys.request_shutdown();
        // The busy worker can't be joined; we must NOT hang — bounded false.
        assert!(!sys.join_with_timeout(Duration::from_millis(200)));
        let _ = gate_tx; // keep the sender alive until here
    }

    #[test]
    fn no_new_jobs_dispatch_after_request_shutdown() {
        let sys = JobSystem::new(1);
        sys.request_shutdown();
        let (tx, rx) = mpsc::channel();
        sys.submit(Priority::Background, move |_| tx.send(()).unwrap());
        // Shutdown flag is set, so the worker returns instead of running it.
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }
```
Note: the existing test module already imports `mpsc` and `Duration`.

- [ ] **Step 2: Run the tests to confirm they fail to compile.**

Run: `cargo test -p ferrolite-jobs system::tests`
Expected: FAIL — `request_shutdown` / `join_with_timeout` not found.

- [ ] **Step 3: Implement the API in `ferrolite-jobs/src/system.rs`.**
  1. Change the field: `workers: Vec<JoinHandle<()>>` → `workers: Mutex<Vec<JoinHandle<()>>>`. In `new`, store `workers: Mutex::new(handles)`.
  2. Add the methods to `impl JobSystem`:
```rust
    /// Signal all workers to stop pulling new jobs. Idempotent. In-flight jobs
    /// keep running until they return (or observe cancellation cooperatively).
    pub fn request_shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.cvar.notify_all();
    }

    /// True once shutdown has been requested (or the pool is being dropped).
    /// Long job bodies poll this at checkpoints to bail promptly at exit.
    pub fn is_shutting_down(&self) -> bool {
        self.shared.shutdown.load(Ordering::SeqCst)
    }

    /// Join all workers off the calling thread, waiting at most `timeout`.
    /// Returns true if every worker exited in time; false on timeout, in which
    /// case the still-running workers are detached (reclaimed at process exit)
    /// so the caller (e.g. the UI thread at close) never blocks unboundedly.
    pub fn join_with_timeout(&self, timeout: std::time::Duration) -> bool {
        let handles: Vec<JoinHandle<()>> = {
            let mut w = self.workers.lock().expect("workers mutex");
            w.drain(..).collect()
        };
        if handles.is_empty() {
            return true;
        }
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        // Detached joiner thread: owns the handles, so if we time out they are
        // simply reclaimed when the process exits rather than joined here.
        std::thread::spawn(move || {
            for h in handles {
                let _ = h.join();
            }
            let _ = done_tx.send(());
        });
        done_rx.recv_timeout(timeout).is_ok()
    }
```
  3. Update `Drop` to reuse the shared path and the `Mutex`:
```rust
impl Drop for JobSystem {
    fn drop(&mut self) {
        self.request_shutdown();
        // If join_with_timeout already drained the handles (normal exit path),
        // this is empty and returns immediately. Otherwise join here.
        let handles: Vec<JoinHandle<()>> = self
            .workers
            .get_mut()
            .map(|w| w.drain(..).collect())
            .unwrap_or_default();
        for h in handles {
            let _ = h.join();
        }
    }
}
```
Note: `Mutex::get_mut` in `Drop` avoids a lock (we have `&mut self`). Keep `use std::sync::{Arc, Condvar, Mutex};` (already present).

- [ ] **Step 4: Run the tests to confirm they pass.**

Run: `cargo test -p ferrolite-jobs system::tests`
Expected: PASS (all three new tests + the existing three).

- [ ] **Step 5: Verify ingest cancel-checkpoint granularity in `ferrolite-app/src/ingest.rs`.** Read `ingest_job` (≈ lines 184-382). Confirm the per-file work loop polls `cancel.is_cancelled()` frequently enough that a cancelled ingest bails within a few files (not only once per whole phase). If a long inner loop (e.g. the metadata/decode pass over all files) has NO per-iteration cancel check, add one at the top of that loop:
```rust
    if cancel.is_cancelled() {
        break; // shutdown/reindex requested — stop scanning further files
    }
```
Do not restructure the ingest otherwise. (Confirm before adding: the investigation noted checks at ~200/227/283/319; if those are already inside the per-file loop, no change is needed — say so in your report.)

- [ ] **Step 6: Drive shutdown from the eframe close hook in `ferrolite-app/src/app.rs`.**
First VERIFY the framework API for this eframe 0.29 + wgpu build. Prefer implementing `on_exit` on `impl eframe::App for FerroliteApp` (the same impl block that has `fn update`, app.rs:1169). Check the exact signature against the vendored eframe (it is `fn on_exit(&mut self, _gl: Option<&glow::Context>)` when the glow feature is present; confirm the form that compiles for this build). Implement:
```rust
    fn on_exit(&mut self /*, _gl: Option<&glow::Context> */) {
        // Prevent the UI thread from blocking unboundedly on worker joins at
        // close (see docs/superpowers/investigations/2026-07-02-...). Cancel
        // in-flight tracked work, stop new dispatch, then bounded-join.
        self.state.cancel_pending_jobs();
        self.state.jobs.request_shutdown();
        let _ = self
            .state
            .jobs
            .join_with_timeout(std::time::Duration::from_millis(500));
    }
```
If `on_exit`'s signature can't be made to compile cleanly for this build, fall back to detecting close inside `update` (backend-agnostic) — add near the very top of `update`, and guard it so it runs once:
```rust
    if ctx.input(|i| i.viewport().close_requested()) {
        self.state.cancel_pending_jobs();
        self.state.jobs.request_shutdown();
        let _ = self
            .state
            .jobs
            .join_with_timeout(std::time::Duration::from_millis(500));
    }
```
Report which form you used and why. Either way, the subsequent implicit `Drop for JobSystem` finds workers already stopped and returns instantly.

- [ ] **Step 7: Gate.**

Run: `cargo clippy --workspace --all-targets -- -D warnings` (clean) and `cargo test -p ferrolite-jobs` (green). Also `cargo fmt` + `cargo fmt --check` clean.

- [ ] **Step 8: Commit.**
```bash
git add ferrolite-jobs/src/system.rs ferrolite-app/src/app.rs ferrolite-app/src/ingest.rs
git commit -m "fix(jobs): bounded, cooperative shutdown so the app closes promptly"
```

---

## Task 2: Two-tier thumbnail cache (Bug B)

**Files:**
- Create: `ferrolite-app/src/library/thumb_pixel_cache.rs`
- Modify: `ferrolite-app/src/library/mod.rs` (declare `pub mod thumb_pixel_cache;`)
- Modify: `ferrolite-app/src/state.rs` (add the cache field + wire `request_thumbnail` / `upload_thumbnail`)

**Interfaces:**
- Produces:
  - `pub struct ThumbPixelCache` — session-only LRU keyed by `image_id`, storing decoded pixels `(rgba: Vec<u8>, w: u32, h: u32)`. Bounded capacity.
  - `ThumbPixelCache::new(capacity: usize) -> Self`
  - `ThumbPixelCache::get(&mut self, id: i64) -> Option<(Vec<u8>, u32, u32)>` — LRU-touch on hit; returns a clone of the pixels ready to upload.
  - `ThumbPixelCache::insert(&mut self, id: i64, rgba: Vec<u8>, w: u32, h: u32)` — inserts + evicts LRU over capacity.
  - `ThumbPixelCache::contains(&self, id: i64) -> bool`
- Consumes: `AppState::request_thumbnail` (state.rs:255), `AppState::upload_thumbnail` (state.rs:228), the `pending_uploads` backlog + `MAX_THUMB_UPLOADS_PER_FRAME` budget (app.rs).

**Design notes:** Root cause (investigation §B): the 512-entry GPU texture LRU (state.rs:186) is far smaller than a 3320-image library, so scrolling evicts textures and every re-reveal re-submits a `Visible` job that re-reads the DB blob and re-decodes the JPEG. The fix keeps a larger **CPU-side** LRU of the already-decoded pixels: on a re-reveal, we upload straight from it (through the existing ≤8/frame budget) with **no job, no DB read, no decode**. GPU textures remain the scarce resource (kept modest); CPU pixels are cheap-ish and bounded.

**Memory budget (conscious choice):** thumbnails are ≤256px; decoded RGBA8 ≈ 256×256×4 = 256 KB worst case (smaller for non-square/smaller thumbs). Capacity **1024** → ≤ ~256 MB worst case, typically far less. This comfortably covers many screens of scroll for a large library so normal back-and-forth never evicts, while staying bounded. Use `const THUMB_PIXEL_CACHE_CAP: usize = 1024;` (define it in `state.rs` next to the cache field, documented). On a cache miss the existing `Visible` job path still runs and repopulates the cache.

- [ ] **Step 1: Write the failing tests.** Create `ferrolite-app/src/library/thumb_pixel_cache.rs`:
```rust
//! Session-only CPU-side LRU of decoded thumbnail pixels, keyed by image id.
//! Lets a grid cell re-revealed after GPU-texture eviction re-upload its
//! texture directly — no re-submitted job, no DB read, no JPEG re-decode
//! (see docs/superpowers/investigations/2026-07-02-thumbnail-and-shutdown-bugs.md,
//! Bug B). Bounded so memory stays in check on large libraries.

use std::collections::HashMap;

struct Entry {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
}

pub struct ThumbPixelCache {
    capacity: usize,
    order: Vec<i64>, // front = least recently used
    map: HashMap<i64, Entry>,
}

impl ThumbPixelCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            order: Vec::new(),
            map: HashMap::new(),
        }
    }

    fn touch(&mut self, id: i64) {
        if let Some(pos) = self.order.iter().position(|&x| x == id) {
            self.order.remove(pos);
        }
        self.order.push(id);
    }

    pub fn contains(&self, id: i64) -> bool {
        self.map.contains_key(&id)
    }

    pub fn get(&mut self, id: i64) -> Option<(Vec<u8>, u32, u32)> {
        if self.map.contains_key(&id) {
            self.touch(id);
            let e = self.map.get(&id)?;
            Some((e.rgba.clone(), e.w, e.h))
        } else {
            None
        }
    }

    pub fn insert(&mut self, id: i64, rgba: Vec<u8>, w: u32, h: u32) {
        self.touch(id);
        self.map.insert(id, Entry { rgba, w, h });
        while self.order.len() > self.capacity {
            let evict = self.order.remove(0);
            self.map.remove(&evict);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_miss_then_hit_returns_pixels() {
        let mut c = ThumbPixelCache::new(4);
        assert!(c.get(1).is_none());
        c.insert(1, vec![1, 2, 3, 4], 1, 1);
        assert_eq!(c.get(1), Some((vec![1, 2, 3, 4], 1, 1)));
        assert!(c.contains(1));
    }

    #[test]
    fn evicts_least_recently_used_over_capacity() {
        let mut c = ThumbPixelCache::new(2);
        c.insert(1, vec![0; 4], 1, 1);
        c.insert(2, vec![0; 4], 1, 1);
        let _ = c.get(1); // 1 now most-recent, 2 is LRU
        c.insert(3, vec![0; 4], 1, 1); // over cap → evict 2
        assert!(c.contains(1));
        assert!(!c.contains(2));
        assert!(c.contains(3));
    }

    #[test]
    fn reinsert_same_id_updates_without_growing() {
        let mut c = ThumbPixelCache::new(2);
        c.insert(5, vec![9; 4], 1, 1);
        c.insert(5, vec![7; 4], 1, 1);
        assert_eq!(c.get(5), Some((vec![7; 4], 1, 1)));
        // capacity still allows one more distinct id without evicting 5
        c.insert(6, vec![0; 4], 1, 1);
        assert!(c.contains(5));
        assert!(c.contains(6));
    }
}
```

- [ ] **Step 2: Run the tests to verify they pass.**

Run: `cargo test -p ferrolite-app thumb_pixel_cache`
Expected: 3 tests PASS. Declare the module: in `ferrolite-app/src/library/mod.rs` add `pub mod thumb_pixel_cache;` alongside the other `pub mod` lines.

- [ ] **Step 3: Add the cache to `AppState` (`ferrolite-app/src/state.rs`).**
  1. Near the `textures` field (state.rs:53-54), add:
```rust
    /// Session-only CPU cache of decoded thumbnail pixels so re-revealed cells
    /// re-upload without a new job / DB read / JPEG decode (Bug B).
    pub thumb_pixels: crate::library::thumb_pixel_cache::ThumbPixelCache,
```
  2. Add the capacity constant near the top of the `impl AppState` / module (with the other consts), documented:
```rust
/// CPU thumbnail-pixel cache capacity. ≤256px RGBA8 ≈ 256 KB each → ~256 MB
/// worst case at this cap; covers many screens of scroll on large libraries.
const THUMB_PIXEL_CACHE_CAP: usize = 1024;
```
  3. In `AppState::new` (state.rs:171-223), initialise it alongside `textures`:
```rust
            thumb_pixels: crate::library::thumb_pixel_cache::ThumbPixelCache::new(
                THUMB_PIXEL_CACHE_CAP,
            ),
```

- [ ] **Step 4: Serve re-reveals from the pixel cache in `request_thumbnail` (state.rs:255).** At the top of `request_thumbnail`, after the existing `contains`/`thumb_pending` guard (state.rs:256-258), add a pixel-cache fast path that re-uploads directly instead of submitting a job:
```rust
        // Fast path: pixels already decoded this session → re-upload directly,
        // no job / DB read / JPEG decode (Bug B). Routed through the same
        // per-frame upload budget as ThumbReady via `pending_uploads`.
        if let Some((rgba, w, h)) = self.thumb_pixels.get(image_id) {
            self.pending_uploads.push((image_id, rgba, w, h));
            ctx.request_repaint();
            return;
        }
```
(`pending_uploads` is drained under `MAX_THUMB_UPLOADS_PER_FRAME` at app.rs:1199-1214, so this respects the budget and never spikes the frame. Verify `pending_uploads` is `Vec<(i64, Vec<u8>, u32, u32)>` — it is, per app.rs usage.)

- [ ] **Step 5: Populate the pixel cache on every upload (`upload_thumbnail`, state.rs:228).** In `upload_thumbnail`, after the length guard (state.rs:238-240) and before/after building the texture, insert the pixels into the cache so future re-reveals hit it. Insert a clone BEFORE `rgba` is moved into `ColorImage`:
```rust
        self.thumb_pixels.insert(image_id, rgba.clone(), w, h);
```
Place this line immediately after the `if rgba.len() != … { return; }` guard (so malformed buffers are still rejected first). This covers both ingest-generated and lazy-load thumbnails, and the pixel-cache fast-path re-uploads (Step 4) — which also call `upload_thumbnail` — are idempotent (re-inserting the same id is a no-op-sized update).

- [ ] **Step 6: Gate.**

Run: `cargo clippy --workspace --all-targets -- -D warnings` (clean); `cargo test -p ferrolite-app thumb_pixel_cache` (green); `cargo fmt` + `cargo fmt --check` clean.
(Behavior — that scrolling back into an evicted region no longer re-spawns jobs — is confirmed in the visual test.)

- [ ] **Step 7: Commit.**
```bash
git add ferrolite-app/src/library/thumb_pixel_cache.rs ferrolite-app/src/library/mod.rs ferrolite-app/src/state.rs
git commit -m "fix(app): CPU thumbnail-pixel cache to stop scroll re-decode churn"
```

---

## Task 3: View-meaningful, de-duplicated counter (Bug A)

**Files:**
- Modify: `ferrolite-app/src/events.rs` (scope `thumb_done` to tracked ingest thumbnails)
- Modify: `ferrolite-app/src/status_bar.rs` (`activity_text` never shows a misleading `X/0`)

**Interfaces:**
- Consumes: `AppState.thumb_jobs: HashMap<i64, JobId>` (state.rs:50), `thumb_done`/`thumb_total` (state.rs:46-47), the `apply` fold (events.rs:103-134), `activity_text` (status_bar.rs:6-17).

**Design notes:** Root cause (investigation §A): `thumb_done` is bumped on **every** `ThumbReady`/`ThumbFailed` including lazy-load scroll re-decodes (events.rs:119,125), with no de-dup and no relation to `thumb_total` (which is ingest-only and stays 0 in the "All Photographs" view). Two changes: (1) only count a thumbnail toward `thumb_done` when it was a **tracked ingest** thumbnail (i.e. its id was registered in `thumb_jobs` via `ThumbRegistered`); lazy-load completions still clear `thumb_pending` but don't touch `thumb_done`. This both de-duplicates (each ingest thumbnail is tracked once, counted once on completion) and stops scroll from inflating it. (2) `activity_text` must not render `Thumbnails <done>/0` — when no generation total is tracked, show `Idle`. Bug B's fix already removes the scroll-driven job churn; this makes the number correct and honest regardless.

- [ ] **Step 1: Write/adjust the failing test for `activity_text` (`ferrolite-app/src/status_bar.rs`).** Replace the existing `activity_shows_progress_when_busy` expectation is fine; ADD a test for the misleading-fraction guard:
```rust
    #[test]
    fn activity_idle_when_total_is_zero_even_if_jobs_active() {
        // Lazy-load scroll jobs are active but no ingest generation is tracked
        // (thumb_total == 0): must NOT show a misleading "Thumbnails N/0".
        assert_eq!(activity_text(2, 3, 17, 0), "Idle");
    }
```

- [ ] **Step 2: Run it to verify it fails.**

Run: `cargo test -p ferrolite-app status_bar`
Expected: FAIL — current `activity_text` returns `"Thumbnails 17/0"` because `active + pending != 0`.

- [ ] **Step 3: Fix `activity_text` (status_bar.rs:6-17).** Guard the zero-total case:
```rust
pub fn activity_text(
    active: usize,
    pending: usize,
    thumb_done: usize,
    thumb_total: usize,
) -> String {
    // Only show generation progress while an ingest is actually generating
    // thumbnails (thumb_total > 0). Lazy-load scroll jobs keep `active`/`pending`
    // non-zero but are not generation progress — showing "N/0" would mislead.
    if thumb_total == 0 || thumb_done >= thumb_total {
        "Idle".to_string()
    } else {
        format!("Thumbnails {thumb_done}/{thumb_total}")
    }
}
```

- [ ] **Step 4: Run the status_bar tests.**

Run: `cargo test -p ferrolite-app status_bar`
Expected: PASS (`activity_idle_when_no_jobs`, `activity_shows_progress_when_busy` [12/40 → still "Thumbnails 12/40"], and the new zero-total guard).

- [ ] **Step 5: Scope `thumb_done` to tracked ingest thumbnails (`ferrolite-app/src/events.rs`).** In `apply`, change the `ThumbReady` and `ThumbFailed` arms (events.rs:113-129) so `thumb_done` is incremented ONLY when the id was a tracked ingest thumbnail. `thumb_jobs.remove` returns the removed entry, so use it as the condition:
```rust
            AppEvent::ThumbReady {
                image_id,
                rgba,
                w,
                h,
            } => {
                // Only ingest-generated (tracked) thumbnails count toward the
                // generation progress; lazy-load scroll re-decodes must not
                // inflate it (Bug A).
                if self.thumb_jobs.remove(&image_id).is_some() {
                    self.thumb_done += 1;
                }
                self.thumb_pending.remove(&image_id);
                Some((image_id, rgba, w, h))
            }
            AppEvent::ThumbFailed { image_id } => {
                if self.thumb_jobs.remove(&image_id).is_some() {
                    self.thumb_done += 1;
                }
                self.thumb_pending.remove(&image_id);
                None
            }
```
(Leave the `ThumbRegistered` arm — events.rs:130-134 — unchanged; it is the sole place `thumb_total` grows and `thumb_jobs` is populated.)

- [ ] **Step 6: Gate.**

Run: `cargo clippy --workspace --all-targets -- -D warnings` (clean); `cargo test -p ferrolite-app` (green); `cargo fmt` + `cargo fmt --check` clean.

- [ ] **Step 7: Commit.**
```bash
git add ferrolite-app/src/events.rs ferrolite-app/src/status_bar.rs
git commit -m "fix(app): scope thumbnail counter to ingest generation (no scroll inflation)"
```

---

## Final gate (before holding for the author's visual test)

- [ ] **Step 1:** `cargo fmt --check` — no diff.
- [ ] **Step 2:** `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- [ ] **Step 3:** `cargo test --workspace` — green (new: `ferrolite-jobs` shutdown ×3, `thumb_pixel_cache` ×3, `status_bar` zero-total guard ×1, plus existing).
- [ ] **Step 4: STOP and hold for Jann's visual test:**
  - Open a large library (thousands of images), let ingest/scroll create a backlog, then close the window → the app exits promptly (no lasting "Not Responding"); closing mid-ingest also exits within ~a second.
  - Scroll far down and back up repeatedly → thumbnails reappear instantly (re-uploaded from the pixel cache), and the status counter does NOT climb into the tens/hundreds of thousands.
  - In the "All Photographs" view with no active ingest, the activity text reads "Idle" (not `Thumbnails N/0`).
  - Ingest a fresh folder → the counter shows a sane `done/total` that rises to `total` and then reads "Idle".

---

## Self-Review (checked against the design + codebase)

**Coverage:** Bug C (bounded cooperative shutdown + eframe close hook) ✓ Task 1; Bug B (two-tier CPU pixel cache, fast-path re-upload, populate on upload) ✓ Task 2; Bug A (ingest-scoped, de-duplicated counter + honest status text) ✓ Task 3. Each maps to a confirmed investigation finding.

**Placeholder scan:** the only "verify against actual code" markers are (1) the `eframe::App::on_exit` signature for this wgpu build — genuinely version/feature-specific, with a concrete `close_requested()` fallback given; and (2) the ingest cancel-checkpoint granularity (Task 1 Step 5) — a read-and-confirm with the exact line to add if missing. Both are directed checks, not open TODOs.

**Type consistency:** `ThumbPixelCache` produced in Task 2 (`new`/`get`/`insert`/`contains`) is consumed only by `state.rs`; `get` returns `(Vec<u8>, u32, u32)` matching `pending_uploads`' element type and `upload_thumbnail`'s params. `request_shutdown`/`is_shutting_down`/`join_with_timeout` (Task 1) are used by app.rs exactly as declared. `thumb_jobs.remove(&i64).is_some()` (Task 3) matches the `HashMap<i64, JobId>` at state.rs:50. `activity_text`'s signature is unchanged (Task 3), only its body.

**Ordering rationale:** Task 1 (shutdown) first so Jann can close the app cleanly while validating Tasks 2-3. Tasks 2 and 3 are independent; 2 before 3 because 2's fix removes the scroll job churn that 3's counter change also depends on for a clean visual result.
