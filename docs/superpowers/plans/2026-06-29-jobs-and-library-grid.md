# Ferrolite Speed Core — Plan 3: Jobs & Library Grid Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a photo-agnostic threaded **job scheduler** (`ferrolite-jobs`), give the catalog a **WAL + read-pool / single-writer** concurrency model, and wire the **Library module** UI (virtualized grid, folder tree, live status bar) so a real multi-thousand-image folder ingests off the UI thread and browses fast — proving **G1** and benchmarkable on **M1/M2**.

**Architecture:** `ferrolite-jobs` is a new engine-transferable crate (zero deps): a priority queue (3 buckets + lazy invalidation, reprioritizable) fed to a fixed pool of `std::thread` workers, with cancellation tokens and panic isolation. `ferrolite-catalog` gains WAL mode, a `ReadPool` of read-only connections for the UI, and folder-scan / row-build helpers; the single writer is shared as `Arc<Mutex<Catalog>>`. `ferrolite-app` owns the job system + writer + read pool, drives folder ingest as an `Interactive` job that fans out per-image `Background` thumbnail jobs, promotes the visible window to `Visible` each frame, and renders the grid from a pure layout/LRU/cell-state core.

**Tech Stack:** Rust 2021, `std::thread`/`Mutex`/`Condvar` (jobs), `rusqlite` 0.32 (WAL), `rawler` 0.7.2 (decode, via existing crates), `fast_image_resize` 6.0, `image` 0.25 (JPEG decode for textures), `rayon` 1.10 (data-parallel metadata decode inside the ingest job), `rfd` (folder picker), `eframe`/`egui`/`egui-wgpu` 0.29, `wgpu` 22.

## Global Constraints

- **License:** GPL-3.0-only (`license.workspace = true`) on every crate. (Architecture map §2.)
- **Crate-tier dependency purity:** `ferrolite-jobs` is engine-transferable → **zero non-permissive deps** (this plan gives it **zero deps at all** — pure `std`). `ferrolite-catalog`/`ferrolite-decode`/`ferrolite-app` are photo-domain and may pull LGPL/anything; the binary stays GPL-3.0. (Architecture map §3.)
- **Catalog is a cache, never source of truth:** a corrupt/mismatched DB is rebuilt by re-ingesting. (Architecture map §5.2.)
- **Job submission is universal & cancellable:** every slow operation (ingest, thumbnail) is a `Job` with **priority + cancellation token**; navigation cancels superseded work. Cancellation is **cooperative** (jobs poll their token at checkpoints), never preemptive. (Architecture map §5.1, design §3.)
- **Concurrency model (design §4):** SQLite in **WAL** mode; **one** writer connection shared as `Arc<Mutex<Catalog>>`; a **pool of read-only connections** (`ReadPool`) for UI queries. Readers must never block on the writer mutex.
- **`ferrolite-jobs` is result-type-agnostic (refinement of design §3):** jobs are `FnOnce(&CancelToken) + Send + 'static`; domain results flow over an **app-owned `std::sync::mpsc` channel** the closure captures. The crate exposes `active_count()`/`pending_count()` for the status bar rather than a bespoke `ProgressSink` type. Reason: keeps the crate zero-dep and reusable; the app owns its own progress semantics. (Documented, intentional deviation — same convention prior plans used.)
- **Custom worker pool, not `rayon::spawn` (refinement of design §3):** true priority + reprioritization of *queued* work requires our own queue; rayon does not honor priorities once a task is spawned. `rayon` is still used for data-parallel metadata decode *inside* the ingest job. (Documented, intentional refinement; the architecture map's "rayon for CPU parallelism" still holds for the data-parallel work.)
- **Pinned versions:** `rusqlite` **0.32** (do NOT bump — `libsqlite3-sys` 0.38's `cfg_select!` breaks on stable rustc; see repo memory). `eframe`/`egui`/`egui-wgpu` **0.29**, `wgpu` **22**. Confirm the newest matching patch with `cargo add` during execution; if an egui 0.29 signature differs from what is written here, adjust minimally and note it (same convention as Plans 1–2).
- **Files focused:** 200–400 lines/file target, 800 max. (User coding-style rule.)
- **Immutability / idiomatic Rust:** `let` by default; `Result` + `?`; library errors via `thiserror`; never `unwrap()` outside tests/truly-unreachable. (User Rust rules.)
- **Frequent commits:** one commit per task minimum; conventional-commit messages (`feat:`, `test:`, `refactor:`, `chore:`).
- **Intermediate `dead_code`:** new items are unused until later tasks wire them. `dead_code`/`unused` warnings are EXPECTED in intermediate tasks — do NOT add blanket `#[allow(dead_code)]` and do NOT gate intermediate tasks on `clippy -D warnings`. The **final task** must reach `cargo clippy --all-targets -- -D warnings` clean.
- **No GPU/VT/viewer in this plan** (Plan 4). The status bar's "GPU: idle" slot stays static.

---

## Plan sequence for Spec 1 (this is Plan 3 of 5)

1. ✅ Foundation & Gate 0. 2. ✅ Decode & Catalog. **3. Jobs & Library grid (this plan).** 4. Viewer & VT ladder. 5. Benchmark harness & milestone.

Parent spec: `docs/superpowers/specs/2026-06-29-jobs-and-library-grid-design.md`.

---

## File structure (this plan)

```
Cargo.toml                                    # MODIFY: add ferrolite-jobs + ferrolite-catalog/-jobs/rfd workspace deps
ferrolite-jobs/                               # NEW crate (engine-transferable, zero deps)
  Cargo.toml
  src/
    lib.rs                                    # re-exports
    priority.rs                               # Priority, CancelToken, JobId
    queue.rs                                  # Queue: 3-bucket priority queue + lazy invalidation
    system.rs                                 # JobSystem: worker pool, submit, reprioritize, counts, shutdown
ferrolite-catalog/
  src/
    catalog.rs                                # MODIFY: WAL on open; set_decode_status; delegate reads to queries
    queries.rs                                # NEW: read SQL as free fns over &Connection (DRY between Catalog & ReadPool)
    read_pool.rs                              # NEW: ReadPool — pool of read-only connections
    scan.rs                                   # NEW: scan_raw_files + is_raw (moved from ingest.rs)
    model.rs                                  # MODIFY: NewImage::from_metadata
    ingest.rs                                 # MODIFY: use scan_raw_files + NewImage::from_metadata (DRY)
    lib.rs                                    # MODIFY: re-export ReadPool, scan_raw_files, RawFile
ferrolite-app/
  Cargo.toml                                  # MODIFY: add ferrolite-{jobs,catalog,decode,image}, image, rayon, rfd
  src/
    app.rs                                    # MODIFY: hold JobSystem/writer/ReadPool; wire panels to real state
    state.rs                                  # NEW: AppState (catalog handles, channels, selection, counts)
    events.rs                                 # NEW: AppEvent enum + drain loop
    ingest.rs                                 # NEW: spawn_ingest (Interactive job) + spawn_thumbnail (Background job)
    library/
      mod.rs                                  # NEW: Library module render entry
      grid.rs                                 # NEW: virtualized grid rendering (uses grid_layout + texture_cache + cell_state)
      grid_layout.rs                          # NEW: pure visible-range math
      texture_cache.rs                        # NEW: pure LRU id→TextureHandle cache
      cell_state.rs                           # NEW: pure ImageRecord→CellState mapping
      panel.rs                                # NEW: left panel (catalog + folder tree)
      toolbar.rs                              # NEW: top toolbar (thumb-size slider, search stub)
    status_bar.rs                             # NEW: live status bar (EXIF, N indexed, jobs activity)
benches/ or scripts/                          # NEW: M1/M2 benchmark harness (Task 9)
docs/benchmarks/2026-06-29-m1-m2-method.md    # NEW: methodology + dataset file list pointer
```

---

### Task 1: `ferrolite-jobs` crate + `Priority`, `CancelToken`, `JobId`

**Files:**
- Modify: `Cargo.toml` (workspace members + deps)
- Create: `ferrolite-jobs/Cargo.toml`, `ferrolite-jobs/src/lib.rs`, `ferrolite-jobs/src/priority.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum Priority { Background, Visible, Interactive }` — `Ord`, with `Interactive` greatest. `pub fn index(self) -> usize` (Background=0, Visible=1, Interactive=2).
  - `#[derive(Clone)] pub struct CancelToken` — `new()`, `cancel(&self)`, `is_cancelled(&self) -> bool` (cheap `Arc<AtomicBool>`).
  - `#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)] pub struct JobId(pub u64)`.

- [ ] **Step 1: Add the crate to the workspace**

In `Cargo.toml`, add `"ferrolite-jobs"` to `members`, and under `[workspace.dependencies]` add:

```toml
ferrolite-catalog = { path = "ferrolite-catalog" }
ferrolite-jobs = { path = "ferrolite-jobs" }
rfd = "0.15"
```

(`ferrolite-image`/`ferrolite-decode` are already there; `image`/`rayon` already there.)

- [ ] **Step 2: Create `ferrolite-jobs/Cargo.toml` (zero deps)**

```toml
[package]
name = "ferrolite-jobs"
version = "0.0.1"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
# Engine-transferable tier: pure std, zero dependencies.
```

- [ ] **Step 3: Write failing tests for `priority.rs`**

Create `ferrolite-jobs/src/priority.rs`:

```rust
//! Priority levels, a cooperative cancellation token, and job identifiers.
//! Zero-dependency, photo-agnostic — engine-transferable tier.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Scheduling priority. `Interactive` preempts `Visible` preempts `Background`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Background,
    Visible,
    Interactive,
}

impl Priority {
    /// Dense index for bucketed queues: Background=0, Visible=1, Interactive=2.
    pub fn index(self) -> usize {
        match self {
            Priority::Background => 0,
            Priority::Visible => 1,
            Priority::Interactive => 2,
        }
    }
}

/// Cheaply-cloneable cooperative cancellation flag. Long jobs poll
/// [`CancelToken::is_cancelled`] at checkpoints; cancellation is never preemptive.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Opaque job identifier handed out by the scheduler.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct JobId(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_orders_interactive_highest() {
        assert!(Priority::Interactive > Priority::Visible);
        assert!(Priority::Visible > Priority::Background);
    }

    #[test]
    fn priority_index_is_dense_and_ordered() {
        assert_eq!(Priority::Background.index(), 0);
        assert_eq!(Priority::Visible.index(), 1);
        assert_eq!(Priority::Interactive.index(), 2);
    }

    #[test]
    fn cancel_token_starts_uncancelled_then_latches() {
        let t = CancelToken::new();
        assert!(!t.is_cancelled());
        t.cancel();
        assert!(t.is_cancelled());
    }

    #[test]
    fn cancel_token_clone_shares_state() {
        let t = CancelToken::new();
        let c = t.clone();
        t.cancel();
        assert!(c.is_cancelled(), "clone must observe the same flag");
    }
}
```

- [ ] **Step 4: Create `lib.rs` re-exporting the types**

```rust
//! ferrolite-jobs — a photo-agnostic threaded job scheduler with priorities,
//! cooperative cancellation, and panic isolation. Engine-transferable.

mod priority;

pub use priority::{CancelToken, JobId, Priority};
```

- [ ] **Step 5: Run the tests — expect PASS**

Run: `cargo test -p ferrolite-jobs`
Expected: 4 tests pass; crate builds.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml ferrolite-jobs/
git commit -m "feat(jobs): scaffold ferrolite-jobs crate with Priority/CancelToken/JobId"
```

---

### Task 2: priority `Queue` (3 buckets + lazy invalidation + reprioritize)

**Files:**
- Create: `ferrolite-jobs/src/queue.rs`
- Modify: `ferrolite-jobs/src/lib.rs` (add `mod queue;`)

**Interfaces:**
- Consumes: `Priority`, `JobId`, `CancelToken` from Task 1.
- Produces (crate-internal — `pub(crate)`):
  - `struct QueuedJob { pub priority: Priority, pub token: CancelToken, pub run: Box<dyn FnOnce(&CancelToken) + Send> }`
  - `struct Queue` with: `new()`, `push(JobId, QueuedJob)`, `reprioritize(JobId, Priority)`, `cancel(JobId)` (drop pending), `pop_highest() -> Option<(JobId, QueuedJob)>`, `pending_len() -> usize`.
  - **Semantics:** `pop_highest` returns the highest-priority *live* job; reprioritization and cancel use **lazy invalidation** — stale bucket entries (whose recorded priority no longer matches, or whose job was removed) are skipped at pop.

- [ ] **Step 1: Write failing tests for the queue**

Create `ferrolite-jobs/src/queue.rs`:

```rust
//! A 3-bucket priority queue with lazy invalidation. Single-threaded data
//! structure (the worker pool guards it with a Mutex). No threads here so the
//! ordering logic is unit-testable in isolation.

use crate::priority::{CancelToken, JobId, Priority};
use std::collections::{HashMap, VecDeque};

pub(crate) struct QueuedJob {
    pub priority: Priority,
    pub token: CancelToken,
    pub run: Box<dyn FnOnce(&CancelToken) + Send>,
}

pub(crate) struct Queue {
    jobs: HashMap<JobId, QueuedJob>,
    /// One FIFO of ids per priority index. May contain stale ids (lazy
    /// invalidation): an id is live iff `jobs[id].priority == bucket priority`.
    buckets: [VecDeque<JobId>; 3],
}

impl Queue {
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            buckets: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
        }
    }

    pub fn push(&mut self, id: JobId, job: QueuedJob) {
        self.buckets[job.priority.index()].push_back(id);
        self.jobs.insert(id, job);
    }

    /// Change a pending job's priority. The new bucket gets a fresh entry; the
    /// old bucket's entry becomes stale and is skipped at pop. No-op if the job
    /// already ran / isn't pending.
    pub fn reprioritize(&mut self, id: JobId, priority: Priority) {
        if let Some(job) = self.jobs.get_mut(&id) {
            if job.priority != priority {
                job.priority = priority;
                self.buckets[priority.index()].push_back(id);
            }
        }
    }

    /// Drop a still-pending job (its bucket entry becomes stale). Jobs already
    /// dequeued/running are unaffected — cancel those via their `CancelToken`.
    pub fn cancel(&mut self, id: JobId) {
        self.jobs.remove(&id);
    }

    /// Remove and return the highest-priority live job, or `None` if empty.
    pub fn pop_highest(&mut self) -> Option<(JobId, QueuedJob)> {
        for p in [Priority::Interactive, Priority::Visible, Priority::Background] {
            let bucket = &mut self.buckets[p.index()];
            while let Some(id) = bucket.pop_front() {
                match self.jobs.get(&id) {
                    Some(job) if job.priority == p => {
                        let job = self.jobs.remove(&id).expect("present");
                        return Some((id, job));
                    }
                    _ => continue, // stale entry (reprioritized or cancelled)
                }
            }
        }
        None
    }

    pub fn pending_len(&self) -> usize {
        self.jobs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn record(log: Arc<Mutex<Vec<u64>>>, n: u64) -> QueuedJob {
        QueuedJob {
            priority: Priority::Background,
            token: CancelToken::new(),
            run: Box::new(move |_| log.lock().unwrap().push(n)),
        }
    }

    fn job_at(p: Priority) -> QueuedJob {
        QueuedJob { priority: p, token: CancelToken::new(), run: Box::new(|_| {}) }
    }

    #[test]
    fn pops_in_priority_then_fifo_order() {
        let mut q = Queue::new();
        q.push(JobId(1), job_at(Priority::Background));
        q.push(JobId(2), job_at(Priority::Interactive));
        q.push(JobId(3), job_at(Priority::Visible));
        q.push(JobId(4), job_at(Priority::Interactive));

        let order: Vec<u64> = std::iter::from_fn(|| q.pop_highest().map(|(id, _)| id.0)).collect();
        assert_eq!(order, vec![2, 4, 3, 1]); // Interactive(FIFO), Visible, Background
    }

    #[test]
    fn reprioritize_promotes_a_pending_job() {
        let mut q = Queue::new();
        q.push(JobId(1), job_at(Priority::Background));
        q.push(JobId(2), job_at(Priority::Background));
        q.reprioritize(JobId(2), Priority::Interactive);

        let order: Vec<u64> = std::iter::from_fn(|| q.pop_highest().map(|(id, _)| id.0)).collect();
        assert_eq!(order, vec![2, 1], "promoted job comes first; no duplicate");
    }

    #[test]
    fn cancel_drops_a_pending_job() {
        let mut q = Queue::new();
        q.push(JobId(1), job_at(Priority::Visible));
        q.push(JobId(2), job_at(Priority::Visible));
        q.cancel(JobId(1));
        assert_eq!(q.pending_len(), 1);
        let (id, _) = q.pop_highest().unwrap();
        assert_eq!(id, JobId(2));
        assert!(q.pop_highest().is_none());
    }

    #[test]
    fn runs_carry_the_closure() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut q = Queue::new();
        q.push(JobId(9), record(log.clone(), 42));
        let (_, job) = q.pop_highest().unwrap();
        (job.run)(&job.token);
        assert_eq!(*log.lock().unwrap(), vec![42]);
    }
}
```

- [ ] **Step 2: Wire the module**

In `ferrolite-jobs/src/lib.rs` add `mod queue;` (below `mod priority;`).

- [ ] **Step 3: Run tests — expect PASS**

Run: `cargo test -p ferrolite-jobs queue`
Expected: 4 queue tests pass.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-jobs/src/queue.rs ferrolite-jobs/src/lib.rs
git commit -m "feat(jobs): priority queue with lazy invalidation and reprioritization"
```

---

### Task 3: `JobSystem` worker pool (submit, reprioritize, counts, cancel, panic isolation)

**Files:**
- Create: `ferrolite-jobs/src/system.rs`
- Modify: `ferrolite-jobs/src/lib.rs`

**Interfaces:**
- Consumes: `Queue`/`QueuedJob` (Task 2), `Priority`/`CancelToken`/`JobId` (Task 1).
- Produces:
  - `pub struct JobSystem` — `new(workers: usize) -> Self`, `submit<F>(&self, Priority, F) -> JobHandle where F: FnOnce(&CancelToken) + Send + 'static`, `reprioritize(&self, JobId, Priority)`, `active_count() -> usize`, `pending_count() -> usize`. `Drop` shuts workers down.
  - `pub struct JobHandle { id: JobId, token: CancelToken }` — `id() -> JobId`, `cancel(&self)`.

- [ ] **Step 1: Write failing tests for the system**

Create `ferrolite-jobs/src/system.rs`:

```rust
//! Fixed-size worker pool driving the priority [`Queue`]. We use our own threads
//! (not rayon) so queued work can be reprioritized before it starts; rayon does
//! not expose priorities. Panics in jobs are caught so one bad job never downs
//! the pool.

use crate::priority::{CancelToken, JobId, Priority};
use crate::queue::{QueuedJob, Queue};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

struct Shared {
    queue: Mutex<Queue>,
    cvar: Condvar,
    shutdown: AtomicBool,
    active: AtomicUsize,
    next_id: AtomicUsize,
}

pub struct JobSystem {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

/// Handle to a submitted job: lets the caller cancel it (cooperatively) and
/// identifies it for reprioritization.
#[derive(Clone)]
pub struct JobHandle {
    id: JobId,
    token: CancelToken,
}

impl JobHandle {
    pub fn id(&self) -> JobId {
        self.id
    }
    pub fn cancel(&self) {
        self.token.cancel();
    }
}

impl JobSystem {
    /// Spawn `workers` threads (clamp to ≥1).
    pub fn new(workers: usize) -> Self {
        let workers = workers.max(1);
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::new()),
            cvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            next_id: AtomicUsize::new(0),
        });
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let shared = Arc::clone(&shared);
            handles.push(std::thread::spawn(move || worker_loop(shared)));
        }
        Self { shared, workers: handles }
    }

    pub fn submit<F>(&self, priority: Priority, run: F) -> JobHandle
    where
        F: FnOnce(&CancelToken) + Send + 'static,
    {
        let id = JobId(self.shared.next_id.fetch_add(1, Ordering::Relaxed) as u64);
        let token = CancelToken::new();
        let job = QueuedJob { priority, token: token.clone(), run: Box::new(run) };
        self.shared.queue.lock().expect("queue mutex").push(id, job);
        self.shared.cvar.notify_one();
        JobHandle { id, token }
    }

    pub fn reprioritize(&self, id: JobId, priority: Priority) {
        self.shared.queue.lock().expect("queue mutex").reprioritize(id, priority);
        self.shared.cvar.notify_one();
    }

    /// Jobs currently executing on a worker.
    pub fn active_count(&self) -> usize {
        self.shared.active.load(Ordering::SeqCst)
    }

    /// Jobs queued and not yet started (includes stale entries' live count).
    pub fn pending_count(&self) -> usize {
        self.shared.queue.lock().expect("queue mutex").pending_len()
    }
}

impl Drop for JobSystem {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.cvar.notify_all();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let next = {
            let mut q = shared.queue.lock().expect("queue mutex");
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(job) = q.pop_highest() {
                    break Some(job);
                }
                q = shared.cvar.wait(q).expect("cvar wait");
            }
        };
        if let Some((_id, job)) = next {
            if job.token.is_cancelled() {
                continue; // cancelled between enqueue and dispatch
            }
            shared.active.fetch_add(1, Ordering::SeqCst);
            let token = job.token.clone();
            let run = job.run;
            let result = catch_unwind(AssertUnwindSafe(|| run(&token)));
            shared.active.fetch_sub(1, Ordering::SeqCst);
            if result.is_err() {
                eprintln!("ferrolite-jobs: job panicked; worker continues");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn runs_submitted_jobs() {
        let sys = JobSystem::new(2);
        let (tx, rx) = mpsc::channel();
        for n in 0..5 {
            let tx = tx.clone();
            sys.submit(Priority::Background, move |_| tx.send(n).unwrap());
        }
        drop(tx);
        let mut got: Vec<i32> = rx.iter().collect();
        got.sort();
        assert_eq!(got, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn panic_in_one_job_does_not_down_the_pool() {
        let sys = JobSystem::new(1);
        sys.submit(Priority::Background, |_| panic!("boom"));
        let (tx, rx) = mpsc::channel();
        sys.submit(Priority::Background, move |_| tx.send(()).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(()));
    }

    #[test]
    fn cancelled_job_observes_its_token() {
        let sys = JobSystem::new(1);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        // Occupy the single worker so the next job stays queued.
        sys.submit(Priority::Background, move |_| {
            gate_rx.recv().ok();
        });
        let (tx, rx) = mpsc::channel();
        let handle = sys.submit(Priority::Background, move |token| {
            tx.send(token.is_cancelled()).unwrap();
        });
        handle.cancel(); // cancel while still queued
        gate_tx.send(()).unwrap(); // release the worker
        // Cancelled-before-dispatch jobs are skipped, so we never receive.
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
    }
}
```

- [ ] **Step 2: Wire the module + re-exports**

`ferrolite-jobs/src/lib.rs`:

```rust
//! ferrolite-jobs — a photo-agnostic threaded job scheduler with priorities,
//! cooperative cancellation, and panic isolation. Engine-transferable.

mod priority;
mod queue;
mod system;

pub use priority::{CancelToken, JobId, Priority};
pub use system::{JobHandle, JobSystem};
```

- [ ] **Step 3: Run tests — expect PASS**

Run: `cargo test -p ferrolite-jobs`
Expected: all tests pass (priority + queue + system).

- [ ] **Step 4: Verify clippy clean for the crate**

Run: `cargo clippy -p ferrolite-jobs --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-jobs/src/system.rs ferrolite-jobs/src/lib.rs
git commit -m "feat(jobs): worker-pool JobSystem with cancel, reprioritize, panic isolation"
```

---

### Task 4: `ferrolite-catalog` — WAL, read-query extraction, `ReadPool`, scan/build helpers

**Files:**
- Create: `ferrolite-catalog/src/queries.rs`, `ferrolite-catalog/src/read_pool.rs`, `ferrolite-catalog/src/scan.rs`
- Modify: `ferrolite-catalog/src/catalog.rs`, `ferrolite-catalog/src/model.rs`, `ferrolite-catalog/src/ingest.rs`, `ferrolite-catalog/src/lib.rs`
- Test: `ferrolite-catalog/tests/read_pool.rs`

**Interfaces:**
- Consumes: existing `Catalog`, `ImageRecord`, `NewImage`, `Thumbnail`, `ferrolite_decode::Metadata`, `ferrolite_image::Orientation`.
- Produces:
  - `Catalog::open` now enables WAL + `synchronous=NORMAL`.
  - `Catalog::set_decode_status(&self, image_id: i64, status: DecodeStatus) -> Result<(), CatalogError>`.
  - `pub struct ReadPool` — `open(path: &Path, size: usize) -> Result<Self, CatalogError>`, and read methods mirroring the writer: `list_images(folder_id) -> Vec<ImageRecord>`, `image_count() -> u64`, `get_thumbnail(image_id) -> Option<Thumbnail>`, `needs_reingest(folder_id, &str, i64, i64) -> bool`, `list_folders() -> Vec<FolderRecord>`.
  - `pub struct FolderRecord { pub id: i64, pub path: String, pub image_count: u64 }`.
  - `pub struct RawFile { pub path: PathBuf, pub filename: String, pub mtime: i64, pub size: i64 }` + `pub fn scan_raw_files(folder: &Path) -> Vec<RawFile>` + `pub fn is_raw(path: &Path) -> bool`.
  - `NewImage::from_metadata(folder_id: i64, filename: String, mtime: i64, size: i64, meta: &ferrolite_decode::Metadata) -> NewImage` and `NewImage::failed(folder_id, filename, mtime, size) -> NewImage`.

- [ ] **Step 1: Add `ferrolite-decode` dep is already present — confirm catalog deps**

Run: `cargo tree -p ferrolite-catalog -e normal --depth 1`
Expected: shows `ferrolite-decode`, `ferrolite-image`, `rusqlite`, `fast_image_resize`, `image`, `rayon`, `walkdir`, `thiserror`. (No change needed; confirm before editing.)

- [ ] **Step 2: Extract read SQL into `queries.rs` (DRY between writer and pool)**

Create `ferrolite-catalog/src/queries.rs` — move the row-mapping + read statements here as free functions over `&Connection`:

```rust
//! Read queries as free functions over a borrowed `&Connection`, so both the
//! writer (`Catalog`) and the read pool (`ReadPool`) share one implementation.

use crate::error::CatalogError;
use crate::model::{DecodeStatus, ImageRecord};
use crate::thumbnail::Thumbnail;
use ferrolite_image::Orientation;
use rusqlite::{Connection, OptionalExtension};

pub(crate) fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImageRecord> {
    let orientation_exif: Option<i64> = row.get(5)?;
    let status: i64 = row.get(8)?;
    Ok(ImageRecord {
        id: row.get(0)?,
        folder_id: row.get(1)?,
        filename: row.get(2)?,
        width: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
        height: row.get::<_, Option<i64>>(4)?.map(|v| v as u32),
        orientation: Orientation::from_exif(orientation_exif.unwrap_or(1) as u16),
        capture_time: row.get(6)?,
        iso: row.get::<_, Option<i64>>(7)?.map(|v| v as u32),
        decode_status: DecodeStatus::from_i64(status),
    })
}

const IMAGE_COLS: &str = "id, folder_id, filename, width, height, orientation,
                          capture_time, iso, decode_status";

pub(crate) fn list_images(
    conn: &Connection,
    folder_id: i64,
) -> Result<Vec<ImageRecord>, CatalogError> {
    let sql = format!("SELECT {IMAGE_COLS} FROM images WHERE folder_id = ?1 ORDER BY filename");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![folder_id], row_to_record)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn image_by_name(
    conn: &Connection,
    folder_id: i64,
    filename: &str,
) -> Result<Option<ImageRecord>, CatalogError> {
    let sql = format!("SELECT {IMAGE_COLS} FROM images WHERE folder_id = ?1 AND filename = ?2");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(rusqlite::params![folder_id, filename], row_to_record)?;
    Ok(match rows.next() {
        Some(r) => Some(r?),
        None => None,
    })
}

pub(crate) fn image_count(conn: &Connection) -> Result<u64, CatalogError> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM images", [], |row| row.get(0))?;
    Ok(n as u64)
}

pub(crate) fn needs_reingest(
    conn: &Connection,
    folder_id: i64,
    filename: &str,
    mtime: i64,
    size: i64,
) -> Result<bool, CatalogError> {
    let existing: Option<(i64, i64)> = conn
        .query_row(
            "SELECT mtime, size FROM images WHERE folder_id = ?1 AND filename = ?2",
            rusqlite::params![folder_id, filename],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(match existing {
        Some((m, s)) => m != mtime || s != size,
        None => true,
    })
}

pub(crate) fn get_thumbnail(
    conn: &Connection,
    image_id: i64,
) -> Result<Option<Thumbnail>, CatalogError> {
    let mut stmt =
        conn.prepare("SELECT w, h, format, blob FROM thumbnails WHERE image_id = ?1")?;
    let mut rows = stmt.query_map(rusqlite::params![image_id], |row| {
        Ok(Thumbnail {
            width: row.get::<_, i64>(0)? as u32,
            height: row.get::<_, i64>(1)? as u32,
            format: row.get(2)?,
            bytes: row.get(3)?,
        })
    })?;
    Ok(match rows.next() {
        Some(t) => Some(t?),
        None => None,
    })
}

pub(crate) fn list_folders(conn: &Connection) -> Result<Vec<crate::FolderRecord>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, COUNT(i.id)
         FROM folders f LEFT JOIN images i ON i.folder_id = f.id
         GROUP BY f.id, f.path ORDER BY f.path",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(crate::FolderRecord {
            id: row.get(0)?,
            path: row.get(1)?,
            image_count: row.get::<_, i64>(2)? as u64,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}
```

- [ ] **Step 3: Update `catalog.rs` — WAL on open, delegate reads to `queries`, add `set_decode_status`**

In `ferrolite-catalog/src/catalog.rs`:
- In `open`, after `schema::migrate(&conn)?;` add WAL setup:

```rust
        // WAL lets the read pool query concurrently with the single writer.
        // (In-memory DBs ignore journal_mode; harmless there.)
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
```

- Replace the bodies of `image_by_name`, `list_images`, `image_count`, `needs_reingest` to delegate (keeps the writer able to read through its own connection, DRY with the pool):

```rust
    pub fn image_by_name(
        &self,
        folder_id: i64,
        filename: &str,
    ) -> Result<Option<ImageRecord>, CatalogError> {
        crate::queries::image_by_name(self.conn(), folder_id, filename)
    }

    pub fn list_images(&self, folder_id: i64) -> Result<Vec<ImageRecord>, CatalogError> {
        crate::queries::list_images(self.conn(), folder_id)
    }

    pub fn image_count(&self) -> Result<u64, CatalogError> {
        crate::queries::image_count(self.conn())
    }

    pub fn needs_reingest(
        &self,
        folder_id: i64,
        filename: &str,
        mtime: i64,
        size: i64,
    ) -> Result<bool, CatalogError> {
        crate::queries::needs_reingest(self.conn(), folder_id, filename, mtime, size)
    }

    /// Set a row's decode status (used to mark a file `Failed` from a thumbnail job).
    pub fn set_decode_status(
        &self,
        image_id: i64,
        status: DecodeStatus,
    ) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE images SET decode_status = ?1 WHERE id = ?2",
            rusqlite::params![status.as_i64(), image_id],
        )?;
        Ok(())
    }
```

- Delete the now-unused local `row_to_record` from `catalog.rs` (it lives in `queries.rs`). The `OptionalExtension` import in `catalog.rs` may become unused — remove it if clippy flags it.
- The `ThumbnailStore::get_thumbnail` impl in `thumbnail.rs` should also delegate to `queries::get_thumbnail(self.conn(), image_id)` to avoid duplicate SQL. Update it.

- [ ] **Step 4: Add `NewImage::from_metadata` / `NewImage::failed` to `model.rs`**

Append to `ferrolite-catalog/src/model.rs`:

```rust
impl NewImage {
    /// Build a `Done` row from decoded metadata.
    pub fn from_metadata(
        folder_id: i64,
        filename: String,
        mtime: i64,
        size: i64,
        meta: &ferrolite_decode::Metadata,
    ) -> Self {
        Self {
            folder_id,
            filename,
            mtime,
            size,
            make: Some(meta.make.clone()),
            model: Some(meta.model.clone()),
            width: Some(meta.width),
            height: Some(meta.height),
            orientation: meta.orientation,
            capture_time: meta.capture_time.clone(),
            iso: meta.iso,
            decode_status: DecodeStatus::Done,
        }
    }

    /// Build a `Failed` placeholder row (decode failed; grid shows a broken cell).
    pub fn failed(folder_id: i64, filename: String, mtime: i64, size: i64) -> Self {
        Self {
            folder_id,
            filename,
            mtime,
            size,
            make: None,
            model: None,
            width: None,
            height: None,
            orientation: Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: DecodeStatus::Failed,
        }
    }
}
```

- [ ] **Step 5: Create `scan.rs` (move `is_raw` + new `scan_raw_files`) and refactor `ingest.rs`**

Create `ferrolite-catalog/src/scan.rs`:

```rust
//! Filesystem scan: enumerate RAW files in a folder with their stat info.
//! No DB access — reusable by the synchronous `ingest_folder` and by the app's
//! job-driven ingest.

use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// RAW extensions we ingest (lowercased). Extend as camera coverage grows.
const RAW_EXTS: &[&str] = &[
    "nef", "nrw", "cr2", "cr3", "crw", "arw", "sr2", "srf", "raf", "rw2", "orf", "pef", "dng",
    "raw", "rwl", "iiq", "3fr", "erf", "mef", "mos", "kdc", "dcr",
];

pub fn is_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| RAW_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// One RAW file with the stat fields the catalog keys incremental rescan on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFile {
    pub path: PathBuf,
    pub filename: String,
    pub mtime: i64,
    pub size: i64,
}

/// Enumerate RAW files directly in `folder` (depth 1, like the synchronous path).
pub fn scan_raw_files(folder: &Path) -> Vec<RawFile> {
    let mut out = Vec::new();
    for entry in WalkDir::new(folder).max_depth(1).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_file() || !is_raw(p) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(p) else {
            continue;
        };
        let size = meta.len() as i64;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(RawFile {
            path: p.to_path_buf(),
            filename: entry.file_name().to_string_lossy().to_string(),
            mtime,
            size,
        });
    }
    out
}
```

Refactor `ferrolite-catalog/src/ingest.rs` to consume `scan_raw_files` + `NewImage::from_metadata`/`failed` (remove the duplicated walk + `is_raw` + the hand-built `NewImage` in `decode_one`). The behavior and `IngestSummary` must be unchanged so the existing Task-8 integration test still passes. Concretely, replace the walk loop with:

```rust
        let folder_id = self.upsert_folder(path)?;
        let mut summary = IngestSummary::default();

        let mut to_process: Vec<crate::RawFile> = Vec::new();
        for f in crate::scan_raw_files(path) {
            summary.scanned += 1;
            if self.needs_reingest(folder_id, &f.filename, f.mtime, f.size)? {
                to_process.push(f);
            } else {
                summary.skipped += 1;
            }
        }
```

and in the parallel stage build `NewImage` via `decode_one` returning the `Metadata`-built row using `NewImage::from_metadata`; in the failure arm use `NewImage::failed(folder_id, d.filename, d.mtime, d.size)`.

- [ ] **Step 6: Create `read_pool.rs`**

```rust
//! A small pool of read-only SQLite connections for UI queries. Under WAL these
//! never block the single writer. Source of truth is still the files on disk.

use crate::error::CatalogError;
use crate::model::ImageRecord;
use crate::thumbnail::Thumbnail;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct ReadPool {
    path: PathBuf,
    conns: Mutex<Vec<Connection>>,
}

fn open_read_only(path: &Path) -> Result<Connection, CatalogError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    Ok(conn)
}

impl ReadPool {
    /// Open `size` read-only connections to an existing catalog file. The writer
    /// (`Catalog::open`) must have created/migrated the file first.
    pub fn open(path: &Path, size: usize) -> Result<Self, CatalogError> {
        let mut conns = Vec::with_capacity(size.max(1));
        for _ in 0..size.max(1) {
            conns.push(open_read_only(path)?);
        }
        Ok(Self { path: path.to_path_buf(), conns: Mutex::new(conns) })
    }

    fn with_conn<R>(
        &self,
        f: impl FnOnce(&Connection) -> Result<R, CatalogError>,
    ) -> Result<R, CatalogError> {
        // Check out (or open a spare if the pool is momentarily drained).
        let conn = {
            let mut pool = self.conns.lock().expect("read pool mutex");
            pool.pop()
        };
        let conn = match conn {
            Some(c) => c,
            None => open_read_only(&self.path)?,
        };
        let result = f(&conn);
        self.conns.lock().expect("read pool mutex").push(conn);
        result
    }

    pub fn list_images(&self, folder_id: i64) -> Result<Vec<ImageRecord>, CatalogError> {
        self.with_conn(|c| crate::queries::list_images(c, folder_id))
    }
    pub fn image_count(&self) -> Result<u64, CatalogError> {
        self.with_conn(crate::queries::image_count)
    }
    pub fn get_thumbnail(&self, image_id: i64) -> Result<Option<Thumbnail>, CatalogError> {
        self.with_conn(|c| crate::queries::get_thumbnail(c, image_id))
    }
    pub fn needs_reingest(
        &self,
        folder_id: i64,
        filename: &str,
        mtime: i64,
        size: i64,
    ) -> Result<bool, CatalogError> {
        self.with_conn(|c| crate::queries::needs_reingest(c, folder_id, filename, mtime, size))
    }
    pub fn list_folders(&self) -> Result<Vec<crate::FolderRecord>, CatalogError> {
        self.with_conn(crate::queries::list_folders)
    }
}
```

- [ ] **Step 7: Update `lib.rs` exports + add `FolderRecord`**

```rust
//! SQLite digital-asset-management catalog: schema, ingest, thumbnails, queries.

mod catalog;
mod error;
mod ingest;
mod model;
mod queries;
mod read_pool;
mod scan;
mod schema;
mod thumbnail;

pub use catalog::Catalog;
pub use error::CatalogError;
pub use model::{DecodeStatus, ImageRecord, IngestSummary, NewImage};
pub use read_pool::ReadPool;
pub use scan::{is_raw, scan_raw_files, RawFile};
pub use schema::SCHEMA_VERSION;
pub use thumbnail::{generate_thumbnail, Thumbnail, ThumbnailStore, THUMB_MAX_EDGE, THUMB_QUALITY};

/// A folder with its image count (left-panel tree row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRecord {
    pub id: i64,
    pub path: String,
    pub image_count: u64,
}
```

- [ ] **Step 8: Write the read-while-write integration test**

Create `ferrolite-catalog/tests/read_pool.rs`:

```rust
use ferrolite_catalog::{Catalog, DecodeStatus, NewImage, ReadPool};
use ferrolite_image::Orientation;

fn temp_db() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    let unique = format!(
        "ferrolite-rp-{}-{:?}.db",
        std::process::id(),
        std::thread::current().id()
    );
    p.push(unique);
    let _ = std::fs::remove_file(&p);
    p
}

fn new_image(folder_id: i64, filename: &str) -> NewImage {
    NewImage {
        folder_id,
        filename: filename.to_string(),
        mtime: 1,
        size: 1,
        make: Some("Nikon".into()),
        model: Some("Z6".into()),
        width: Some(6048),
        height: Some(4024),
        orientation: Orientation::Normal,
        capture_time: None,
        iso: Some(100),
        decode_status: DecodeStatus::Done,
    }
}

#[test]
fn read_pool_sees_writes_committed_by_the_writer() {
    let path = temp_db();
    let catalog = Catalog::open(&path).unwrap();
    let folder_id = catalog.upsert_folder(std::path::Path::new("/tmp/photos")).unwrap();
    let pool = ReadPool::open(&path, 2).unwrap();

    assert_eq!(pool.image_count().unwrap(), 0);

    // Writer inserts while a reader is live; WAL lets the reader proceed.
    catalog.upsert_image(&new_image(folder_id, "a.nef")).unwrap();
    catalog.upsert_image(&new_image(folder_id, "b.nef")).unwrap();

    assert_eq!(pool.image_count().unwrap(), 2);
    let imgs = pool.list_images(folder_id).unwrap();
    assert_eq!(imgs.len(), 2);
    assert_eq!(imgs[0].filename, "a.nef");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn read_pool_rejects_writes() {
    let path = temp_db();
    let _catalog = Catalog::open(&path).unwrap();
    let pool = ReadPool::open(&path, 1).unwrap();
    // A read query works; the connection is read-only so any write attempt errs.
    // We assert the read path is healthy (write-rejection is enforced by SQLITE_OPEN_READ_ONLY).
    assert_eq!(pool.image_count().unwrap(), 0);
    let _ = std::fs::remove_file(&path);
}
```

- [ ] **Step 9: Run catalog tests — expect PASS**

Run: `cargo test -p ferrolite-catalog`
Expected: existing Plan-2 tests still pass (incl. `ingest_folder` round-trip) + the 2 new read-pool tests pass.

- [ ] **Step 10: Commit**

```bash
git add ferrolite-catalog/ Cargo.toml
git commit -m "feat(catalog): WAL mode, ReadPool, shared read queries, scan/build helpers"
```

---

### Task 5: `ferrolite-app` — state, events, folder-open, ingest job, live counts

**Files:**
- Modify: `ferrolite-app/Cargo.toml`, `ferrolite-app/src/main.rs` (module decls), `ferrolite-app/src/app.rs`
- Create: `ferrolite-app/src/state.rs`, `ferrolite-app/src/events.rs`, `ferrolite-app/src/ingest.rs`

**Interfaces:**
- Consumes: `ferrolite_jobs::{JobSystem, JobHandle, JobId, Priority, CancelToken}`, `ferrolite_catalog::{Catalog, ReadPool, NewImage, scan_raw_files, RawFile, DecodeStatus}`, `ferrolite_decode::read_metadata`.
- Produces:
  - `pub enum AppEvent { Indexed { added: usize }, ThumbReady { image_id: i64, jpeg: Vec<u8> }, ThumbFailed { image_id: i64 }, IngestDone }`.
  - `pub struct AppState` holding `jobs: Arc<JobSystem>`, `writer: Arc<Mutex<Catalog>>`, `reads: Arc<ReadPool>`, `db_path: PathBuf`, `tx: Sender<AppEvent>`, `rx: Receiver<AppEvent>`, `current_folder: Option<i64>`, `indexed: u64`, `thumb_total: usize`, `thumb_done: usize`, `images: Vec<ImageRecord>`, `selected: Option<i64>`, and `thumb_jobs: HashMap<i64, JobId>`, `ingest_handle: Option<JobHandle>`.
  - `pub fn spawn_ingest(state: &mut AppState, ctx: &egui::Context, folder: PathBuf)`.
  - `pub fn spawn_thumbnail(...)` (used by `spawn_ingest`).

- [ ] **Step 1: Add app dependencies**

Edit `ferrolite-app/Cargo.toml` `[dependencies]`:

```toml
eframe = { version = "0.29", default-features = false, features = ["wgpu", "default_fonts"] }
egui = "0.29"
egui-wgpu = "0.29"
wgpu = "22"
ferrolite-jobs = { workspace = true }
ferrolite-catalog = { workspace = true }
ferrolite-decode = { workspace = true }
ferrolite-image = { workspace = true }
image = { workspace = true }
rayon = { workspace = true }
rfd = { workspace = true }
```

Run: `cargo build -p ferrolite-app` to confirm resolution. Expected: builds (shell unchanged yet).

- [ ] **Step 2: Write the pure `AppEvent` apply test first**

Create `ferrolite-app/src/events.rs`:

```rust
//! Domain events flowing from job threads back to the UI thread over an
//! app-owned channel. `apply` folds an event into `AppState`'s counters; it is
//! pure w.r.t. egui so it can be unit-tested.

use crate::state::AppState;

#[derive(Debug)]
pub enum AppEvent {
    /// `added` rows were indexed (status-bar "N indexed").
    Indexed { added: usize },
    /// A thumbnail finished: JPEG bytes for immediate texture upload.
    ThumbReady { image_id: i64, jpeg: Vec<u8> },
    /// A thumbnail (or its decode) failed; the cell shows a broken placeholder.
    ThumbFailed { image_id: i64 },
    /// The ingest walk + row upserts completed.
    IngestDone,
}

impl AppState {
    /// Fold a non-texture event into counters. Returns the JPEG bytes for a
    /// `ThumbReady` so the caller (which holds egui `Context`) can upload a
    /// texture — keeping this function egui-free.
    pub fn apply(&mut self, event: AppEvent) -> Option<(i64, Vec<u8>)> {
        match event {
            AppEvent::Indexed { added } => {
                self.indexed += added as u64;
                None
            }
            AppEvent::ThumbReady { image_id, jpeg } => {
                self.thumb_done += 1;
                self.thumb_jobs.remove(&image_id);
                Some((image_id, jpeg))
            }
            AppEvent::ThumbFailed { image_id } => {
                self.thumb_done += 1;
                self.thumb_jobs.remove(&image_id);
                None
            }
            AppEvent::IngestDone => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_event_increments_count() {
        let mut s = AppState::for_test();
        s.apply(AppEvent::Indexed { added: 3 });
        s.apply(AppEvent::Indexed { added: 2 });
        assert_eq!(s.indexed, 5);
    }

    #[test]
    fn thumb_ready_returns_bytes_and_advances_done() {
        let mut s = AppState::for_test();
        s.thumb_total = 2;
        let out = s.apply(AppEvent::ThumbReady { image_id: 7, jpeg: vec![1, 2, 3] });
        assert_eq!(out, Some((7, vec![1, 2, 3])));
        assert_eq!(s.thumb_done, 1);
    }

    #[test]
    fn thumb_failed_advances_done_without_bytes() {
        let mut s = AppState::for_test();
        let out = s.apply(AppEvent::ThumbFailed { image_id: 9 });
        assert_eq!(out, None);
        assert_eq!(s.thumb_done, 1);
    }
}
```

- [ ] **Step 3: Create `state.rs` with a test constructor**

```rust
//! Application state: catalog handles, the job system, the event channel, and
//! the currently-browsed folder's rows + selection + progress counters.

use ferrolite_catalog::{Catalog, ImageRecord, ReadPool};
use ferrolite_jobs::{JobHandle, JobId, JobSystem};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::events::AppEvent;

pub struct AppState {
    pub jobs: Arc<JobSystem>,
    pub writer: Arc<Mutex<Catalog>>,
    pub reads: Arc<ReadPool>,
    pub db_path: PathBuf,
    pub tx: Sender<AppEvent>,
    pub rx: Receiver<AppEvent>,

    pub current_folder: Option<i64>,
    pub images: Vec<ImageRecord>,
    pub selected: Option<i64>,

    pub indexed: u64,
    pub thumb_total: usize,
    pub thumb_done: usize,

    /// image_id → its pending/running thumbnail job (for reprioritization/cancel).
    pub thumb_jobs: HashMap<i64, JobId>,
    pub ingest_handle: Option<JobHandle>,
}

impl AppState {
    /// Open (or create) the catalog at the OS data dir and wire the job system.
    pub fn new() -> Result<Self, ferrolite_catalog::CatalogError> {
        let db_path = default_db_path();
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let writer = Catalog::open(&db_path)?;
        let reads = ReadPool::open(&db_path, 4)?;
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(3);
        let (tx, rx) = std::sync::mpsc::channel();
        Ok(Self {
            jobs: Arc::new(JobSystem::new(workers)),
            writer: Arc::new(Mutex::new(writer)),
            reads: Arc::new(reads),
            db_path,
            tx,
            rx,
            current_folder: None,
            images: Vec::new(),
            selected: None,
            indexed: 0,
            thumb_total: 0,
            thumb_done: 0,
            thumb_jobs: HashMap::new(),
            ingest_handle: None,
        })
    }

    /// Reload the visible folder's rows from the read pool (called after ingest
    /// progress / folder switch). Cheap: indexed query, no filesystem walk.
    pub fn refresh_images(&mut self) {
        if let Some(folder_id) = self.current_folder {
            if let Ok(rows) = self.reads.list_images(folder_id) {
                self.images = rows;
            }
        }
    }

    #[cfg(test)]
    pub fn for_test() -> Self {
        let path = std::env::temp_dir().join(format!("ferrolite-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let writer = Catalog::open(&path).unwrap();
        let reads = ReadPool::open(&path, 1).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            jobs: Arc::new(JobSystem::new(1)),
            writer: Arc::new(Mutex::new(writer)),
            reads: Arc::new(reads),
            db_path: path,
            tx,
            rx,
            current_folder: None,
            images: Vec::new(),
            selected: None,
            indexed: 0,
            thumb_total: 0,
            thumb_done: 0,
            thumb_jobs: HashMap::new(),
            ingest_handle: None,
        }
    }
}

fn default_db_path() -> PathBuf {
    // Keep it simple + dependency-free: use the OS temp/home; a proper data-dir
    // crate can replace this later. Falls back to the current dir.
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ferrolite").join("catalog.db")
}
```

- [ ] **Step 4: Create `ingest.rs` — the Interactive ingest job + Background thumbnail jobs**

```rust
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
    for (_id, job) in state.thumb_jobs.drain() {
        // Pending jobs are dropped from the queue; running ones finish harmlessly.
        // (We rely on cooperative cancel + cheap thumbnail work.)
        let _ = job;
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

    let handle = jobs.submit(Priority::Interactive, move |cancel| {
        ingest_job(folder, folder_id, writer, reads, jobs, tx, ctx, cancel);
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
```

> **Note for the implementer:** `spawn_thumbnail` currently returns the `JobId` but `ingest_job` ignores it. Task 7 changes `ingest_job` to register `(image_id → JobId)` in `AppState.thumb_jobs` via a follow-up event so the grid can reprioritize. For Task 5 we only need ingest + counts working; recording the id is wired in Task 7. Leaving the id unrecorded here is the expected intermediate state.

- [ ] **Step 5: Declare modules + minimal wiring in `main.rs`/`app.rs`**

In `ferrolite-app/src/main.rs` add module decls: `mod state; mod events; mod ingest; mod status_bar;` (add `mod library;` in Task 7). In `app.rs`, change `FerroliteApp` to own `AppState`:

```rust
pub struct FerroliteApp {
    module: Module,
    thumb_size: f32,
    state: crate::state::AppState,
}
```

In `FerroliteApp::new`, after theme/canvas setup:

```rust
        let state = crate::state::AppState::new().expect("open catalog");
        Self { module: Module::default(), thumb_size: 46.0, state }
```

At the top of `eframe::App::update`, drain events:

```rust
        // Drain job results into state (textures uploaded in Task 7).
        while let Ok(event) = self.state.rx.try_recv() {
            let _ = self.state.apply(event);
        }
        self.state.refresh_images();
```

Add a temporary "Open folder…" button in the left panel (replaced by the real toolbar/panel in Tasks 7–8) so ingest is exercisable now:

```rust
                if ui.button("Open folder…").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        crate::ingest::spawn_ingest(&mut self.state, ctx, folder);
                    }
                }
```

- [ ] **Step 6: Make the status bar show live counts (replace mocked text)**

Replace the mocked status-bar body in `app.rs` with a call into a new `status_bar` module. Create `ferrolite-app/src/status_bar.rs`:

```rust
//! The live status bar: selected-image EXIF, "N indexed", and job activity.

use crate::state::AppState;
use crate::theme;

/// Pure formatter for the right-hand activity string, so it is unit-testable.
pub fn activity_text(active: usize, pending: usize, thumb_done: usize, thumb_total: usize) -> String {
    if active + pending == 0 {
        "Idle".to_string()
    } else {
        format!("Thumbnails {thumb_done}/{thumb_total}")
    }
}

pub fn show(ui: &mut egui::Ui, state: &AppState) {
    let active = state.jobs.active_count();
    let pending = state.jobs.pending_count();
    ui.horizontal_centered(|ui| {
        ui.monospace(selected_exif(state));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.monospace("GPU: idle"); // static until Plan 4
            ui.monospace("·");
            ui.monospace(format!("{} indexed", state.indexed));
            ui.monospace("·");
            ui.monospace(activity_text(active, pending, state.thumb_done, state.thumb_total));
        });
    });
    let _ = theme::TEXT_DIM; // (kept for styling parity; remove if unused)
}

fn selected_exif(state: &AppState) -> String {
    match state.selected.and_then(|id| state.images.iter().find(|i| i.id == id)) {
        Some(img) => {
            let dims = match (img.width, img.height) {
                (Some(w), Some(h)) => format!("{w}×{h}"),
                _ => "—".to_string(),
            };
            let iso = img.iso.map(|v| format!("ISO {v}")).unwrap_or_default();
            format!("{} · {} · {}", img.filename, dims, iso)
        }
        None => "No selection".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_idle_when_no_jobs() {
        assert_eq!(activity_text(0, 0, 0, 0), "Idle");
    }

    #[test]
    fn activity_shows_progress_when_busy() {
        assert_eq!(activity_text(1, 5, 12, 40), "Thumbnails 12/40");
    }
}
```

In `app.rs`, the bottom panel becomes:

```rust
        egui::TopBottomPanel::bottom("status")
            .exact_height(24.0)
            .frame(egui::Frame::none().fill(theme::BG_TITLEBAR))
            .show(ctx, |ui| {
                crate::status_bar::show(ui, &self.state);
            });
```

- [ ] **Step 7: Run app tests + manual smoke**

Run: `cargo test -p ferrolite-app`
Expected: `events` + `status_bar` unit tests pass.
Run: `cargo run -p ferrolite-app`, click "Open folder…", pick a folder of RAWs.
Expected: "N indexed" climbs; activity shows "Thumbnails done/total" then "Idle"; no panics. (No grid yet — that's Task 7.)

- [ ] **Step 8: Commit**

```bash
git add ferrolite-app/
git commit -m "feat(app): job-driven folder ingest with live status-bar counts"
```

---

### Task 6: Library grid — pure layout, LRU texture cache, cell-state logic

**Files:**
- Create: `ferrolite-app/src/library/mod.rs`, `ferrolite-app/src/library/grid_layout.rs`, `ferrolite-app/src/library/texture_cache.rs`, `ferrolite-app/src/library/cell_state.rs`
- Modify: `ferrolite-app/src/main.rs` (`mod library;`)

**Interfaces:**
- Consumes: `ferrolite_catalog::{ImageRecord, DecodeStatus}`, `egui::TextureHandle`.
- Produces:
  - `grid_layout`: `pub struct GridMetrics { pub columns: usize, pub cell: f32, pub row_height: f32 }`, `pub fn metrics(available_width: f32, cell: f32, gap: f32) -> GridMetrics`, `pub fn visible_rows(scroll_top: f32, viewport_h: f32, row_height: f32, total_rows: usize) -> std::ops::Range<usize>`, `pub fn visible_items(scroll_top: f32, viewport_h: f32, m: &GridMetrics, item_count: usize) -> std::ops::Range<usize>`.
  - `texture_cache`: `pub struct TextureCache { ... }` with `new(capacity: usize)`, `get(&mut self, id: i64) -> Option<&egui::TextureHandle>`, `insert(&mut self, id: i64, tex: egui::TextureHandle)`, `contains(&self, id: i64) -> bool`, `len`. LRU eviction at capacity.
  - `cell_state`: `pub enum CellState { Placeholder, Ready, Failed }`, `pub fn cell_state(rec: &ImageRecord, has_texture: bool) -> CellState`.

- [ ] **Step 1: Write failing tests for `grid_layout.rs`**

```rust
//! Pure grid geometry: how many columns fit, and which item indices are visible
//! for a given scroll offset. No egui — unit-testable.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridMetrics {
    pub columns: usize,
    pub cell: f32,
    pub row_height: f32,
}

/// Columns that fit in `available_width` for square-ish `cell` cells separated
/// by `gap`. Always ≥1.
pub fn metrics(available_width: f32, cell: f32, gap: f32) -> GridMetrics {
    let step = cell + gap;
    let columns = (((available_width + gap) / step).floor() as usize).max(1);
    GridMetrics { columns, cell, row_height: cell + gap }
}

/// Inclusive-exclusive range of item indices intersecting the viewport, padded
/// by one row above/below to avoid pop-in at the edges.
pub fn visible_items(
    scroll_top: f32,
    viewport_h: f32,
    m: &GridMetrics,
    item_count: usize,
) -> std::ops::Range<usize> {
    if item_count == 0 || m.row_height <= 0.0 {
        return 0..0;
    }
    let first_row = (scroll_top / m.row_height).floor() as isize - 1;
    let last_row = ((scroll_top + viewport_h) / m.row_height).ceil() as isize + 1;
    let first_row = first_row.max(0) as usize;
    let last_row = last_row.max(0) as usize;
    let start = (first_row * m.columns).min(item_count);
    let end = (last_row * m.columns).min(item_count);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_fits_expected_columns() {
        let m = metrics(820.0, 100.0, 10.0);
        // (820+10)/110 = 7.5 → 7 columns
        assert_eq!(m.columns, 7);
        assert_eq!(m.row_height, 110.0);
    }

    #[test]
    fn metrics_never_zero_columns() {
        assert_eq!(metrics(10.0, 100.0, 10.0).columns, 1);
    }

    #[test]
    fn visible_items_windows_around_scroll() {
        let m = GridMetrics { columns: 5, cell: 100.0, row_height: 110.0 };
        // scrolled to row ~9 (990px), 600px tall viewport.
        let r = visible_items(990.0, 600.0, &m, 1000);
        // first_row = 9-1=8 → start=40; last_row=ceil(1590/110)+1=16 → end=80
        assert_eq!(r.start, 40);
        assert_eq!(r.end, 80);
    }

    #[test]
    fn visible_items_empty_when_no_items() {
        let m = GridMetrics { columns: 5, cell: 100.0, row_height: 110.0 };
        assert_eq!(visible_items(0.0, 600.0, &m, 0), 0..0);
    }

    #[test]
    fn visible_items_clamps_to_item_count() {
        let m = GridMetrics { columns: 5, cell: 100.0, row_height: 110.0 };
        let r = visible_items(0.0, 10_000.0, &m, 12);
        assert_eq!(r.end, 12);
    }
}
```

- [ ] **Step 2: Write failing tests for `texture_cache.rs`**

The LRU order logic is testable without real textures by parameterizing over the stored value — but to keep the public type concrete (`egui::TextureHandle`), test the **eviction-order bookkeeping** via an inner `Lru` helper that is generic and pure:

```rust
//! LRU cache of decoded thumbnail textures keyed by image id. The ordering
//! bookkeeping is a small generic `Lru` so it can be tested without a GPU.

use std::collections::HashMap;

struct Lru {
    capacity: usize,
    order: Vec<i64>, // front = least recently used
}

impl Lru {
    fn new(capacity: usize) -> Self {
        Self { capacity: capacity.max(1), order: Vec::new() }
    }
    fn touch(&mut self, id: i64) {
        if let Some(pos) = self.order.iter().position(|&x| x == id) {
            self.order.remove(pos);
        }
        self.order.push(id);
    }
    /// Record an insertion; return an id to evict if over capacity.
    fn insert(&mut self, id: i64) -> Option<i64> {
        self.touch(id);
        if self.order.len() > self.capacity {
            Some(self.order.remove(0))
        } else {
            None
        }
    }
}

pub struct TextureCache {
    lru: Lru,
    textures: HashMap<i64, egui::TextureHandle>,
}

impl TextureCache {
    pub fn new(capacity: usize) -> Self {
        Self { lru: Lru::new(capacity), textures: HashMap::new() }
    }
    pub fn contains(&self, id: i64) -> bool {
        self.textures.contains_key(&id)
    }
    pub fn get(&mut self, id: i64) -> Option<&egui::TextureHandle> {
        if self.textures.contains_key(&id) {
            self.lru.touch(id);
            self.textures.get(&id)
        } else {
            None
        }
    }
    pub fn insert(&mut self, id: i64, tex: egui::TextureHandle) {
        if let Some(evict) = self.lru.insert(id) {
            self.textures.remove(&evict);
        }
        self.textures.insert(id, tex);
    }
    pub fn len(&self) -> usize {
        self.textures.len()
    }
    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::Lru;

    #[test]
    fn evicts_least_recently_used() {
        let mut lru = Lru::new(2);
        assert_eq!(lru.insert(1), None);
        assert_eq!(lru.insert(2), None);
        lru.touch(1); // 1 now most-recent
        assert_eq!(lru.insert(3), Some(2)); // 2 was LRU → evicted
    }

    #[test]
    fn touch_moves_to_most_recent() {
        let mut lru = Lru::new(3);
        lru.insert(1);
        lru.insert(2);
        lru.insert(3);
        lru.touch(1);
        assert_eq!(lru.insert(4), Some(2));
    }
}
```

- [ ] **Step 3: Write failing tests for `cell_state.rs`**

```rust
//! Map a catalog row + texture availability to a render state for its grid cell.

use ferrolite_catalog::{DecodeStatus, ImageRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    Placeholder,
    Ready,
    Failed,
}

pub fn cell_state(rec: &ImageRecord, has_texture: bool) -> CellState {
    match rec.decode_status {
        DecodeStatus::Failed => CellState::Failed,
        _ if has_texture => CellState::Ready,
        _ => CellState::Placeholder,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::Orientation;

    fn rec(status: DecodeStatus) -> ImageRecord {
        ImageRecord {
            id: 1,
            folder_id: 1,
            filename: "x.nef".into(),
            width: Some(100),
            height: Some(100),
            orientation: Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: status,
        }
    }

    #[test]
    fn failed_row_is_failed_even_without_texture() {
        assert_eq!(cell_state(&rec(DecodeStatus::Failed), false), CellState::Failed);
    }

    #[test]
    fn done_with_texture_is_ready() {
        assert_eq!(cell_state(&rec(DecodeStatus::Done), true), CellState::Ready);
    }

    #[test]
    fn done_without_texture_is_placeholder() {
        assert_eq!(cell_state(&rec(DecodeStatus::Done), false), CellState::Placeholder);
    }
}
```

- [ ] **Step 4: Create `library/mod.rs`**

```rust
//! The Library module: virtualized grid, folder panel, toolbar.

pub mod cell_state;
pub mod grid_layout;
pub mod texture_cache;
```

(Add `mod grid;`, `mod panel;`, `mod toolbar;` in later tasks.)

- [ ] **Step 5: Wire `mod library;` in `main.rs` and run tests**

Run: `cargo test -p ferrolite-app`
Expected: grid_layout (5) + texture_cache (2) + cell_state (3) tests pass, plus earlier events/status tests.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/library/ ferrolite-app/src/main.rs
git commit -m "feat(app): pure grid layout, LRU texture cache, and cell-state logic"
```

---

### Task 7: Virtualized grid rendering + viewport-driven reprioritization

**Files:**
- Create: `ferrolite-app/src/library/grid.rs`
- Modify: `ferrolite-app/src/library/mod.rs`, `ferrolite-app/src/state.rs` (add `textures: TextureCache`, `last_visible: HashSet<i64>`), `ferrolite-app/src/events.rs` (add `ThumbRegistered { image_id, job_id }`), `ferrolite-app/src/ingest.rs` (send registration), `ferrolite-app/src/app.rs` (upload textures on `ThumbReady`; render grid in central panel)

**Interfaces:**
- Consumes: `grid_layout`, `texture_cache::TextureCache`, `cell_state`, `AppState`, `ferrolite_jobs::{JobId, Priority}`.
- Produces: `pub fn show(ui: &mut egui::Ui, state: &mut AppState, cell: f32)` rendering the virtualized grid and emitting reprioritization calls; `AppState::upload_thumbnail(&mut self, ctx, image_id, jpeg)`.

- [ ] **Step 1: Record thumbnail JobIds so the grid can reprioritize them**

Add to `events.rs` `AppEvent`: `ThumbRegistered { image_id: i64, job_id: ferrolite_jobs::JobId }`, and in `apply` handle it by `self.thumb_jobs.insert(image_id, job_id); None`. In `ingest.rs`, change the `spawn_thumbnail` call site in `ingest_job` to:

```rust
            let job_id = spawn_thumbnail(&jobs, &writer, &tx, &ctx, id, path);
            state_total_inc(&tx, id, job_id);
```

where (add to `ingest.rs`):

```rust
fn state_total_inc(tx: &Sender<AppEvent>, image_id: i64, job_id: ferrolite_jobs::JobId) {
    let _ = tx.send(AppEvent::ThumbRegistered { image_id, job_id });
}
```

Also bump `thumb_total` when registering: in `events.rs` `apply`, the `ThumbRegistered` arm does `self.thumb_total += 1;` before inserting.

- [ ] **Step 2: Add texture cache + visible-set to `AppState` and an upload helper**

In `state.rs`, add fields `pub textures: crate::library::texture_cache::TextureCache,` and `pub last_visible: std::collections::HashSet<i64>,`; init `TextureCache::new(512)` and `HashSet::new()` in both `new()` and `for_test()`. Add:

```rust
    /// Decode a thumbnail JPEG and upload it as an egui texture into the cache.
    pub fn upload_thumbnail(&mut self, ctx: &egui::Context, image_id: i64, jpeg: Vec<u8>) {
        let Ok(img) = image::load_from_memory(&jpeg) else {
            return;
        };
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        let color = egui::ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw());
        let tex = ctx.load_texture(format!("thumb-{image_id}"), color, egui::TextureOptions::LINEAR);
        self.textures.insert(image_id, tex);
    }
```

In `app.rs`'s event drain loop, upload on `ThumbReady`:

```rust
        while let Ok(event) = self.state.rx.try_recv() {
            if let Some((id, jpeg)) = self.state.apply(event) {
                self.state.upload_thumbnail(ctx, id, jpeg);
            }
        }
```

- [ ] **Step 3: Write `grid.rs`**

```rust
//! Virtualized thumbnail grid. Realizes only the visible window of cells, pulls
//! ready thumbnails from the read pool on demand, and promotes the visible
//! window's pending thumbnail jobs to `Visible` priority.

use crate::library::cell_state::{cell_state, CellState};
use crate::library::grid_layout::{metrics, visible_items};
use crate::state::AppState;
use crate::theme;
use ferrolite_jobs::Priority;
use std::collections::HashSet;

const GAP: f32 = 8.0;

pub fn show(ui: &mut egui::Ui, state: &mut AppState, cell: f32) {
    let avail_w = ui.available_width();
    let m = metrics(avail_w, cell, GAP);
    let item_count = state.images.len();
    let total_rows = item_count.div_ceil(m.columns.max(1));
    let total_height = total_rows as f32 * m.row_height;

    let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
    let out = scroll.show_viewport(ui, |ui, viewport| {
        ui.set_height(total_height);
        let scroll_top = viewport.min.y.max(0.0);
        let vh = viewport.height();
        let range = visible_items(scroll_top, vh, &m, item_count);

        // Promote visible pending thumbnail jobs; demote ones that scrolled away.
        let mut now_visible: HashSet<i64> = HashSet::new();
        for idx in range.clone() {
            now_visible.insert(state.images[idx].id);
        }
        reprioritize(state, &now_visible);
        state.last_visible = now_visible;

        for idx in range {
            let rec = state.images[idx].clone();
            let row = idx / m.columns;
            let col = idx % m.columns;
            let x = ui.min_rect().left() + col as f32 * m.row_height;
            let y = ui.min_rect().top() + row as f32 * m.row_height;
            let rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cell, cell));
            paint_cell(ui, state, &rec, rect);
        }
    });
    let _ = out;
}

fn reprioritize(state: &AppState, now_visible: &HashSet<i64>) {
    for id in now_visible.difference(&state.last_visible) {
        if let Some(job_id) = state.thumb_jobs.get(id) {
            state.jobs.reprioritize(*job_id, Priority::Visible);
        }
    }
    for id in state.last_visible.difference(now_visible) {
        if let Some(job_id) = state.thumb_jobs.get(id) {
            state.jobs.reprioritize(*job_id, Priority::Background);
        }
    }
}

fn paint_cell(ui: &mut egui::Ui, state: &mut AppState, rec: &ferrolite_catalog::ImageRecord, rect: egui::Rect) {
    // Pull a ready thumbnail from the pool on demand if not yet cached.
    if !state.textures.contains(rec.id) && rec.decode_status != ferrolite_catalog::DecodeStatus::Failed {
        if let Ok(Some(thumb)) = state.reads.get_thumbnail(rec.id) {
            let jpeg = thumb.bytes;
            state.upload_thumbnail(ui.ctx(), rec.id, jpeg);
        }
    }
    let has_tex = state.textures.contains(rec.id);
    let painter = ui.painter_at(rect);
    match cell_state(rec, has_tex) {
        CellState::Ready => {
            if let Some(tex) = state.textures.get(rec.id) {
                let img = egui::Image::new(tex).fit_to_exact_size(rect.size());
                img.paint_at(ui, rect);
            }
        }
        CellState::Placeholder => {
            painter.rect_filled(rect, 2.0, theme::BG_PANEL);
        }
        CellState::Failed => {
            painter.rect_filled(rect, 2.0, theme::BG_PANEL);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "broken",
                egui::FontId::proportional(11.0),
                theme::SEMANTIC_RED,
            );
        }
    }

    // Selection: click toggles the selected id.
    let resp = ui.interact(rect, ui.id().with(("cell", rec.id)), egui::Sense::click());
    if resp.clicked() {
        state.selected = Some(rec.id);
    }
    if state.selected == Some(rec.id) {
        painter.rect_stroke(rect, 2.0, egui::Stroke::new(2.0, theme::ACCENT));
    }
}
```

> **egui 0.29 note:** `ScrollArea::show_viewport` yields the viewport `Rect`; `Image::paint_at` and `painter_at` are 0.29 APIs. If `div_ceil` is unavailable on the resolved std (it is stable since 1.73; MSRV here is 1.88 so fine). If any signature differs, adjust minimally and note it (same convention as prior plans).

- [ ] **Step 4: Render the grid in the central panel**

In `app.rs`, replace the `canvas::paint` call in the `CentralPanel` with the grid when in Library module:

```rust
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG_CANVAS))
            .show(ctx, |ui| {
                if self.module.is_library() {
                    crate::library::grid::show(ui, &mut self.state, self.thumb_size + 60.0);
                } else {
                    let rect = ui.available_rect_before_wrap();
                    canvas::paint(ui, rect); // Develop stub keeps the wgpu canvas
                }
            });
```

(`thumb_size` 0–100 maps to a ~60–160px cell; the `+60.0` keeps a sane minimum. Toolbar in Task 8 makes this the real control.) Add `mod grid;` to `library/mod.rs`.

- [ ] **Step 5: Run + manual verify**

Run: `cargo test -p ferrolite-app` (pure tests still green).
Run: `cargo run -p ferrolite-app`, open a RAW folder.
Expected: placeholders appear immediately; thumbnails fill **visible-first** as you scroll; failed files show a "broken" cell; clicking selects (accent outline); scrolling stays smooth.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/
git commit -m "feat(app): virtualized library grid with visible-first thumbnail loading"
```

---

### Task 8: Left panel folder tree + toolbar (thumbnail-size control)

**Files:**
- Create: `ferrolite-app/src/library/panel.rs`, `ferrolite-app/src/library/toolbar.rs`
- Modify: `ferrolite-app/src/library/mod.rs`, `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `AppState`, `ferrolite_catalog::FolderRecord`, `EguiSlider`.
- Produces: `panel::show(ui, state, ctx)` (catalog header + folder tree + Open-folder action), `toolbar::show(ui, thumb_size: &mut f32)`.

- [ ] **Step 1: Create `toolbar.rs`**

```rust
//! Library top toolbar: search (stub), sort (stub), and the thumbnail-size slider.

use crate::widgets::EguiSlider;

pub fn show(ui: &mut egui::Ui, thumb_size: &mut f32) {
    ui.horizontal(|ui| {
        ui.add_enabled(false, egui::TextEdit::singleline(&mut String::new()).hint_text("Search"));
        ui.separator();
        ui.label("Sort:");
        ui.add_enabled(false, egui::Label::new("Filename"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(EguiSlider {
                label: "Size",
                value: thumb_size,
                min: 0.0,
                max: 100.0,
                default: 46.0,
                step: 1.0,
                decimals: 0,
                unit: "",
                bipolar: false,
                signed: false,
            });
        });
    });
}
```

- [ ] **Step 2: Create `panel.rs` (folder tree from the read pool)**

```rust
//! Library left panel: Catalog header, the Open-folder action, and a flat folder
//! list (with counts) read from the catalog. A nested tree is a later refinement.

use crate::ingest::spawn_ingest;
use crate::state::AppState;
use crate::theme;

pub fn show(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context) {
    ui.add_space(8.0);
    ui.colored_label(theme::TEXT_FAINT, "CATALOG");
    ui.label("All Photographs");
    ui.add_space(8.0);

    if ui.button("Open folder…").clicked() {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            spawn_ingest(state, ctx, folder);
        }
    }

    ui.add_space(12.0);
    ui.colored_label(theme::TEXT_FAINT, "FOLDERS");
    let folders = state.reads.list_folders().unwrap_or_default();
    for f in folders {
        let name = std::path::Path::new(&f.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| f.path.clone());
        let selected = state.current_folder == Some(f.id);
        if ui
            .selectable_label(selected, format!("{name}  ({})", f.image_count))
            .clicked()
        {
            state.current_folder = Some(f.id);
            state.selected = None;
            state.refresh_images();
        }
    }
}
```

- [ ] **Step 3: Wire panel + toolbar into `app.rs`**

Left panel body becomes `crate::library::panel::show(ui, &mut self.state, ctx);` (remove the temporary Open-folder button + stray slider from Task 5). Add a toolbar as a `TopBottomPanel::top` *below* the title bar (or a strip atop the central panel):

```rust
        egui::TopBottomPanel::top("toolbar")
            .exact_height(40.0)
            .frame(egui::Frame::none().fill(theme::BG_TOOLBAR))
            .show(ctx, |ui| {
                if self.module.is_library() {
                    crate::library::toolbar::show(ui, &mut self.thumb_size);
                }
            });
```

Add `mod panel;` and `mod toolbar;` to `library/mod.rs`.

- [ ] **Step 4: Run + manual verify**

Run: `cargo test -p ferrolite-app` (green).
Run: `cargo run -p ferrolite-app`.
Expected: left panel lists ingested folders with counts; clicking a folder loads its grid; the size slider grows/shrinks cells live; toolbar renders to theme.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/
git commit -m "feat(app): library left-panel folder list and thumbnail-size toolbar"
```

---

### Task 9: M1/M2 benchmark harness + methodology doc + final clippy gate

**Files:**
- Create: `ferrolite-app/src/bin/bench_browse.rs` (instrumented headless harness), `docs/benchmarks/2026-06-29-m1-m2-method.md`
- Modify: any files needing clippy cleanup across the plan

**Interfaces:**
- Consumes: `ferrolite_catalog::{Catalog, ReadPool, scan_raw_files}`, `ferrolite_jobs::*`, `ferrolite_decode`, the app's `ingest` orchestration (refactor the headless-usable parts as needed).
- Produces: a binary printing **M1** (wall-clock to first N thumbnails persisted) and a throughput number usable alongside the interactive **M2** scroll observation.

- [ ] **Step 1: Write the methodology doc**

Create `docs/benchmarks/2026-06-29-m1-m2-method.md` capturing (from spec §9): same machine / same dataset vs RawTherapee; dataset = ~2,000 24MP RAWs (commit the file *list*, not the files); M1 = cold folder-open → first thumbnails; M2 = warm grid-scroll smoothness (observe dropped frames). State acceptance: beat RawTherapee on M1, smoother on M2; image quality not compared. Include the exact commands below.

- [ ] **Step 2: Write the headless M1 harness**

`ferrolite-app/src/bin/bench_browse.rs`: open a fresh temp catalog, time how long until the first `N` (e.g. 100) thumbnails are committed to the DB using the **same** `JobSystem` + `scan_raw_files` + decode/thumbnail path as the app (no egui). Print elapsed ms for: rows-indexed-complete (M1a) and first-100-thumbnails (M1b), plus total thumbnails/sec. Accept the folder path + N as CLI args (`std::env::args`). Use only the public crate APIs; reuse `ingest::spawn_thumbnail`-equivalent logic by extracting a tiny headless helper if needed (do **not** depend on egui in the bin — if `spawn_thumbnail` needs an `egui::Context`, add a headless variant `thumbnail_blocking(writer, id, path) -> Result<Thumbnail,String>` in `ingest.rs` that both the job closure and the bench call, keeping logic DRY).

- [ ] **Step 3: Run the harness on a sample folder**

Run: `cargo run -p ferrolite-app --bin bench_browse -- <path-to-raw-folder> 100`
Expected: prints M1a/M1b/throughput without panics; numbers are plausible (sub-second M1a for a few hundred files on a dev box).

- [ ] **Step 4: Record a baseline in the doc**

Append the harness output + the dev machine spec to the methodology doc as the ferrolite baseline. (Head-to-head vs RawTherapee is run by the user on their benchmark machine; this commits the *method* + ferrolite numbers.)

- [ ] **Step 5: Full workspace gate — fmt, clippy (-D warnings), tests**

Run:
```bash
cargo fmt --all
cargo clippy --all-targets --workspace -- -D warnings
cargo test --workspace
```
Expected: formatted; **zero** clippy warnings across the whole workspace; all tests pass. Fix any remaining `dead_code`/unused items now (this is the final clippy gate per Global Constraints).

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/bin/bench_browse.rs docs/benchmarks/ ferrolite-app/ ferrolite-catalog/ ferrolite-jobs/
git commit -m "feat(bench): headless M1 browse benchmark + methodology; workspace clippy gate"
```

---

## Self-Review

**Spec coverage:**
- §2 `ferrolite-jobs` (priority/cancel/progress/result/reprioritize/panic) → Tasks 1–3. ✓ (ProgressSink refined to `active/pending_count` + app channel — flagged in Global Constraints.)
- §3 WAL + single writer + read pool → Task 4. ✓
- §4 viewport-driven priority ingest + thumbnail pipeline + LRU texture cache → Tasks 5, 6, 7. ✓
- §5 Library UI (left panel, toolbar, grid, live status bar) → Tasks 5 (status), 7 (grid), 8 (panel/toolbar). ✓ (Metadata-filter popover intentionally stubbed per spec §5/design-system §8.)
- §6 error handling (Failed cells, panic isolation, vanished folder) → Tasks 3, 4, 5, 7. ✓
- §7 testing (jobs pure, catalog read-while-write, grid pure logic) → Tasks 1–4, 6, plus status/events unit tests. ✓
- §8 benchmark M1/M2 → Task 9. ✓ (M2 is an interactive observation; the harness automates M1 and throughput.)

**Placeholder scan:** No "TBD"/"implement later". The two `Note for the implementer` blocks describe *expected intermediate state* (id-recording deferred to Task 7; headless thumbnail helper), not missing content — each has concrete follow-up steps. Search-stub/sort-stub are intentional, spec-sanctioned deviations.

**Type consistency:** `Priority`/`CancelToken`/`JobId`/`JobSystem`/`JobHandle` names consistent across Tasks 1–3 and consumers (5,7,9). `AppEvent` variants (`Indexed`/`ThumbReady`/`ThumbFailed`/`IngestDone`/`ThumbRegistered`) consistent across events.rs/ingest.rs/app.rs. Catalog additions (`ReadPool`, `scan_raw_files`, `RawFile`, `FolderRecord`, `NewImage::from_metadata`/`failed`, `set_decode_status`) match between Task 4 definitions and Task 5/7/8 uses. `TextureCache`/`GridMetrics`/`CellState` consistent between Task 6 and Task 7.

**Open item flagged for the implementer:** `reprioritize` borrows `&AppState` while `paint_cell` takes `&mut AppState` inside the same `show_viewport` closure — if the borrow checker objects, compute the visible-id set first (immutable), call `reprioritize`, then run the paint loop (mutable), exactly as ordered in `grid.rs` Step 3. The code is already structured this way (reprioritize before the paint loop).
