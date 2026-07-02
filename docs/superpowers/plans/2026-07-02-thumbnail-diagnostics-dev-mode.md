# Thumbnail Diagnostics / Dev-Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a permanent, env-flag-gated observability dev-mode (`FERROLITE_DIAG`) that instruments the thumbnail + background-job pipeline — job system, lazy-load, caches, per-frame loop, viewer flood, and shutdown — with a throttled ~1/sec log trace plus a toggleable egui overlay, so the remaining performance bottleneck becomes unambiguous.

**Architecture:** `ferrolite-jobs` gains its own per-instance, gated atomic counters and a `stats() -> JobStats` snapshot getter (no new dependency — the crate stays zero-dep and engine-transferable). A new `ferrolite-app::diag` module owns all app-side counters (caches, lazy-load, per-frame, shutdown), reads `JobStats` each tick, and renders both the log and the overlay from one snapshot. `ferrolite-vt` is untouched — the viewer-flood question is answered by watching `submitted[Visible]` spike, since the viewer submits tile jobs through the same `JobSystem`.

**Tech Stack:** Rust, egui/eframe 0.29.1, `std::sync::atomic` (Relaxed), `OnceLock`. No new crate dependencies.

## Global Constraints

- **Zero overhead when off:** every counter increment and all formatting/overlay/frame-timing work MUST be gated behind an `enabled()`/`diag_on` check. Unset `FERROLITE_DIAG` ⇒ one bool check, nothing else runs. (Same pattern as `ferrolite-app/src/thumb_profile.rs`.)
- **Never block or perturb the UI thread:** counters use `Ordering::Relaxed`; snapshot/format/overlay work happens only at the ~1/sec tick or gated frame-end; no new locks on the worker hot path beyond the existing queue mutex; log file writes are best-effort and never block (a failed write is dropped). The instrumentation must never itself force a repaint — it only *reports* whether one was forced.
- **Do not fix the bottleneck.** This branch is observability only.
- **Do not modify `ferrolite-vt` or `FERROLITE_PROFILE_THUMBS`.**
- **Rust style:** `cargo fmt` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; 100-col width; no `unwrap()` in non-test code.
- **Gate green before finishing:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, then HOLD for the author's hands-on visual test (CLAUDE.md).
- **Windows build note:** if `cargo test` hits `LNK1104: cannot open ...ferrolite_app-<hash>.exe`, re-run with an isolated `CARGO_TARGET_DIR` rather than killing the process.
- **Flag semantics** (verbatim): unset → off; `1`|`both` → log+overlay; `log` → log only; `overlay` → overlay only. `FERROLITE_DIAG_FILE=<path>` overrides the session-file path.

---

## File Structure

| File | Responsibility | Created / Modified |
|------|----------------|--------------------|
| `ferrolite-jobs/src/diag.rs` | `JobStats` struct, `DiagCounters` atomics, `env_on()` helper | Create |
| `ferrolite-jobs/src/system.rs` | Increment counters in submit/dispatch/complete/panic/cancel; `stats()`; per-instance `diag_on` | Modify |
| `ferrolite-jobs/src/queue.rs` | `cancel` returns `was_present`; `pending_by_priority()` | Modify |
| `ferrolite-jobs/src/lib.rs` | Re-export `JobStats` | Modify |
| `ferrolite-app/src/diag.rs` | Mode parsing, `enabled()`, session file, app-side counters + recorders, `Snapshot`, `DiagState`, `format_log`/`format_shutdown`/overlay | Create |
| `ferrolite-app/src/main.rs` | `mod diag;`, `diag::init()` | Modify |
| `ferrolite-app/src/library/texture_cache.rs` | Record tex hit/miss/evict | Modify |
| `ferrolite-app/src/library/thumb_pixel_cache.rs` | Record pix hit/miss/evict | Modify |
| `ferrolite-app/src/state.rs` | Classify `request_thumbnail`; count `retain_visible_thumbnail_jobs` cancels | Modify |
| `ferrolite-app/src/app.rs` | Frame timing, events/uploads counts, F9 toggle, per-frame tick, overlay, shutdown line | Modify |

---

## Task 1: `ferrolite-jobs` — `JobStats`, gated counters, `stats()`

**Files:**
- Create: `ferrolite-jobs/src/diag.rs`
- Modify: `ferrolite-jobs/src/system.rs`, `ferrolite-jobs/src/queue.rs`, `ferrolite-jobs/src/lib.rs`

**Interfaces:**
- Produces:
  - `ferrolite_jobs::JobStats { submitted: [u64; 3], dispatched: u64, completed: u64, cancelled_before_dispatch: u64, panicked: u64, active: usize, pending: [u64; 3], cancel_removed: u64, cancel_absent: u64 }` (indices are `Priority::index()`: Background=0, Visible=1, Interactive=2).
  - `JobSystem::stats(&self) -> JobStats`.
  - `Queue::cancel(&mut self, id) -> bool` (true iff the id was present).
  - `Queue::pending_by_priority(&self) -> [u64; 3]`.

- [ ] **Step 1: Create the diag counters module**

Create `ferrolite-jobs/src/diag.rs`:

```rust
//! Optional, per-`JobSystem` diagnostic counters. Gated by `FERROLITE_DIAG`:
//! when off, `JobSystem` never touches these (see `system.rs`), so there is no
//! runtime cost. Counters live on the per-instance `Shared` (not globals) so
//! parallel tests stay isolated. Photo-agnostic; keeps this crate zero-dep.

use crate::priority::Priority;
use std::sync::atomic::{AtomicU64, Ordering};

/// Cumulative, monotonically-increasing job-system counters. One set per
/// `JobSystem`. All reads/writes are `Relaxed` — these are diagnostics, not
/// synchronization.
#[derive(Default)]
pub(crate) struct DiagCounters {
    pub submitted: [AtomicU64; 3],
    pub dispatched: AtomicU64,
    pub completed: AtomicU64,
    pub cancelled_before_dispatch: AtomicU64,
    pub panicked: AtomicU64,
    pub cancel_removed: AtomicU64,
    pub cancel_absent: AtomicU64,
}

impl DiagCounters {
    pub fn inc_submitted(&self, p: Priority) {
        self.submitted[p.index()].fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_dispatched(&self) {
        self.dispatched.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_completed(&self) {
        self.completed.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_cancelled_before_dispatch(&self) {
        self.cancelled_before_dispatch.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_panicked(&self) {
        self.panicked.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_cancel(&self, removed: bool) {
        if removed {
            self.cancel_removed.fetch_add(1, Ordering::Relaxed);
        } else {
            self.cancel_absent.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Immutable snapshot of a `JobSystem`'s counters plus live gauges, taken by
/// [`crate::JobSystem::stats`]. Cheap to copy; safe to format off the hot path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JobStats {
    /// Per priority index (Background=0, Visible=1, Interactive=2).
    pub submitted: [u64; 3],
    pub dispatched: u64,
    pub completed: u64,
    pub cancelled_before_dispatch: u64,
    pub panicked: u64,
    /// Jobs running on a worker right now.
    pub active: usize,
    /// Queued (not-yet-started) live jobs per priority index.
    pub pending: [u64; 3],
    /// `cancel(id)` calls that actually dropped a queued job.
    pub cancel_removed: u64,
    /// `cancel(id)` calls where the id was already running/gone (no drop).
    pub cancel_absent: u64,
}

/// True iff `FERROLITE_DIAG` is set to an "on" value. Read once at
/// `JobSystem::new` (rare) and stored per instance, so no global cache is
/// needed and tests can force it on via the private `build` ctor.
pub(crate) fn env_on() -> bool {
    match std::env::var("FERROLITE_DIAG") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !(v.is_empty() || v == "0" || v == "off" || v == "false")
        }
        Err(_) => false,
    }
}
```

- [ ] **Step 2: Register the module and re-export `JobStats`**

In `ferrolite-jobs/src/lib.rs`, add the module and re-export:

```rust
mod diag;
mod priority;
mod queue;
mod system;

pub use diag::JobStats;
pub use priority::{CancelToken, JobId, Priority};
pub use system::{JobHandle, JobSystem};
```

- [ ] **Step 3: Make `Queue::cancel` report presence and add per-bucket counts**

In `ferrolite-jobs/src/queue.rs`, change `cancel` to return `bool` and add `pending_by_priority`:

```rust
    /// Drop a still-pending job (its bucket entry becomes stale). Returns true
    /// iff the job was present (i.e. actually removed); false if it had already
    /// been dequeued/run/cancelled. Jobs already running are unaffected — cancel
    /// those via their `CancelToken`.
    pub fn cancel(&mut self, id: JobId) -> bool {
        self.jobs.remove(&id).is_some()
    }

    /// Count of live (non-stale) queued jobs per priority index
    /// (Background=0, Visible=1, Interactive=2). O(pending); called only at the
    /// ~1/sec diagnostic tick.
    pub fn pending_by_priority(&self) -> [u64; 3] {
        let mut out = [0u64; 3];
        for job in self.jobs.values() {
            out[job.priority.index()] += 1;
        }
        out
    }
```

(The existing `cancel_drops_a_pending_job` test ignores the new return value, so it still passes.)

- [ ] **Step 4: Wire counters into `Shared` and the worker loop; add `stats()` and `build`**

In `ferrolite-jobs/src/system.rs`:

Add the import near the top:

```rust
use crate::diag::{DiagCounters, JobStats};
```

Extend `Shared` with the diag fields:

```rust
struct Shared {
    queue: Mutex<Queue>,
    cvar: Condvar,
    shutdown: AtomicBool,
    active: AtomicUsize,
    next_id: AtomicUsize,
    diag_on: bool,
    diag: DiagCounters,
}
```

Replace `JobSystem::new` with a thin wrapper over a private `build` (so tests can force diag on):

```rust
impl JobSystem {
    /// Spawn `workers` threads (clamp to ≥1). Diagnostics follow `FERROLITE_DIAG`.
    pub fn new(workers: usize) -> Self {
        Self::build(workers, crate::diag::env_on())
    }

    fn build(workers: usize, diag_on: bool) -> Self {
        let workers = workers.max(1);
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::new()),
            cvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            next_id: AtomicUsize::new(0),
            diag_on,
            diag: DiagCounters::default(),
        });
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let shared = Arc::clone(&shared);
            handles.push(std::thread::spawn(move || worker_loop(shared)));
        }
        Self {
            shared,
            workers: Mutex::new(handles),
        }
    }
```

In `submit`, count the submission (capture the index before `priority` is moved into the job):

```rust
    pub fn submit<F>(&self, priority: Priority, run: F) -> JobHandle
    where
        F: FnOnce(&CancelToken) + Send + 'static,
    {
        if self.shared.diag_on {
            self.shared.diag.inc_submitted(priority);
        }
        let id = JobId(self.shared.next_id.fetch_add(1, Ordering::Relaxed) as u64);
        let token = CancelToken::new();
        let job = QueuedJob {
            priority,
            token: token.clone(),
            run: Box::new(run),
        };
        self.shared.queue.lock().expect("queue mutex").push(id, job);
        self.shared.cvar.notify_one();
        JobHandle { id, token }
    }
```

Change `cancel` to record removed-vs-absent:

```rust
    /// Drop a still-pending job from the queue (no-op if already running/done).
    pub fn cancel(&self, id: JobId) {
        let removed = self.shared.queue.lock().expect("queue mutex").cancel(id);
        if self.shared.diag_on {
            self.shared.diag.inc_cancel(removed);
        }
    }
```

Add `stats()` after `pending_count`:

```rust
    /// Snapshot the diagnostic counters + live gauges. Locks the queue briefly
    /// to count per-bucket pending; call only at the ~1/sec diagnostic tick.
    pub fn stats(&self) -> JobStats {
        let d = &self.shared.diag;
        let pending = self
            .shared
            .queue
            .lock()
            .expect("queue mutex")
            .pending_by_priority();
        JobStats {
            submitted: [
                d.submitted[0].load(Ordering::Relaxed),
                d.submitted[1].load(Ordering::Relaxed),
                d.submitted[2].load(Ordering::Relaxed),
            ],
            dispatched: d.dispatched.load(Ordering::Relaxed),
            completed: d.completed.load(Ordering::Relaxed),
            cancelled_before_dispatch: d.cancelled_before_dispatch.load(Ordering::Relaxed),
            panicked: d.panicked.load(Ordering::Relaxed),
            active: self.shared.active.load(Ordering::SeqCst),
            pending,
            cancel_removed: d.cancel_removed.load(Ordering::Relaxed),
            cancel_absent: d.cancel_absent.load(Ordering::Relaxed),
        }
    }
```

In `worker_loop`, count dispatch, cancel-before-dispatch, completion, and panic:

```rust
        if let Some((_id, job)) = next {
            if job.token.is_cancelled() {
                if shared.diag_on {
                    shared.diag.inc_cancelled_before_dispatch();
                }
                continue; // cancelled between enqueue and dispatch
            }
            if shared.diag_on {
                shared.diag.inc_dispatched();
            }
            shared.active.fetch_add(1, Ordering::SeqCst);
            let token = job.token.clone();
            let run = job.run;
            let result = catch_unwind(AssertUnwindSafe(|| run(&token)));
            shared.active.fetch_sub(1, Ordering::SeqCst);
            if shared.diag_on {
                if result.is_ok() {
                    shared.diag.inc_completed();
                } else {
                    shared.diag.inc_panicked();
                }
            }
            if result.is_err() {
                eprintln!("ferrolite-jobs: job panicked; worker continues");
            }
        }
```

- [ ] **Step 5: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `ferrolite-jobs/src/system.rs`:

```rust
    /// Poll `stats().completed` until it reaches `n` or `timeout` elapses.
    fn wait_completed(sys: &JobSystem, n: u64, timeout: Duration) -> u64 {
        let start = std::time::Instant::now();
        loop {
            let c = sys.stats().completed;
            if c >= n || start.elapsed() >= timeout {
                return c;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn stats_count_submit_dispatch_complete_when_diag_on() {
        let sys = JobSystem::build(2, true);
        for _ in 0..5 {
            sys.submit(Priority::Visible, |_| {});
        }
        let completed = wait_completed(&sys, 5, Duration::from_secs(2));
        let s = sys.stats();
        assert_eq!(s.submitted[Priority::Visible.index()], 5, "5 Visible submits");
        assert_eq!(completed, 5, "all 5 completed");
        assert!(s.dispatched >= 5, "at least 5 dispatched");
        assert_eq!(s.panicked, 0, "no panics");
    }

    #[test]
    fn stats_stay_zero_when_diag_off() {
        // `build(_, false)` mirrors `new()` with FERROLITE_DIAG unset.
        let sys = JobSystem::build(2, false);
        for _ in 0..5 {
            sys.submit(Priority::Visible, |_| {});
        }
        let _ = wait_completed(&sys, 0, Duration::from_millis(50)); // let workers run
        let s = sys.stats();
        assert_eq!(s.submitted, [0, 0, 0], "no submit counting when off");
        assert_eq!(s.completed, 0, "no completion counting when off");
        assert_eq!(s.dispatched, 0, "no dispatch counting when off");
    }

    #[test]
    fn stats_count_panicked_job() {
        let sys = JobSystem::build(1, true);
        sys.submit(Priority::Background, |_| panic!("boom"));
        let (tx, rx) = mpsc::channel();
        sys.submit(Priority::Background, move |_| tx.send(()).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(()));
        // The follow-up job completing proves the panicking job already returned.
        let _ = wait_completed(&sys, 1, Duration::from_secs(2));
        assert_eq!(sys.stats().panicked, 1, "panicking job counted once");
    }

    #[test]
    fn stats_count_cancel_removed_vs_absent() {
        let sys = JobSystem::build(1, true);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        // Occupy the single worker so the next job stays queued.
        sys.submit(Priority::Background, move |_| {
            let _ = gate_rx.recv();
        });
        let handle = sys.submit(Priority::Visible, |_| {});
        sys.cancel(handle.id()); // still queued → removed
        sys.cancel(handle.id()); // already gone → absent
        let s = sys.stats();
        assert_eq!(s.cancel_removed, 1, "first cancel dropped a queued job");
        assert_eq!(s.cancel_absent, 1, "second cancel found nothing");
        let _ = gate_tx.send(());
    }
```

- [ ] **Step 6: Run the tests (expect FAIL, then PASS after Steps 1–4)**

Run: `cargo test -p ferrolite-jobs`
Expected: the four new tests pass; all pre-existing `ferrolite-jobs` tests still pass.

- [ ] **Step 7: Gate + commit**

Run: `cargo fmt -p ferrolite-jobs && cargo clippy -p ferrolite-jobs --all-targets -- -D warnings`
Expected: clean.

```bash
git add ferrolite-jobs/src/diag.rs ferrolite-jobs/src/system.rs ferrolite-jobs/src/queue.rs ferrolite-jobs/src/lib.rs
git commit -m "feat(jobs): per-instance JobStats counters + stats() snapshot"
```

---

## Task 2: `ferrolite-app::diag` — mode parsing, gate, session file, rate helper

**Files:**
- Create: `ferrolite-app/src/diag.rs`
- Modify: `ferrolite-app/src/main.rs`

**Interfaces:**
- Produces:
  - `enum DiagMode { Off, Log, Overlay, Both }`
  - `fn parse_mode(raw: Option<&str>) -> DiagMode`
  - `fn mode() -> DiagMode` (cached), `fn enabled() -> bool`, `fn log_enabled() -> bool`, `fn overlay_enabled() -> bool`
  - `fn compute_rate(delta: u64, dt_secs: f64) -> f64`
  - `fn init()` — opens the session file (when logging) and prints its path once
  - `fn write_log(block: &str)` — best-effort stderr + file append

- [ ] **Step 1: Write the failing tests**

Create `ferrolite-app/src/diag.rs` with only the tested pure helpers plus a `tests` module:

```rust
//! Env-flag-gated diagnostics dev-mode (`FERROLITE_DIAG`). Zero overhead when
//! unset: `enabled()` is a single cached bool check that short-circuits every
//! recorder, the per-frame tick, and the overlay. Sibling to `thumb_profile.rs`
//! (the narrow ingest profiler), which this does not touch.
//!
//! `FERROLITE_DIAG` = unset→off | `1`/`both`→log+overlay | `log` | `overlay`.
//! `FERROLITE_DIAG_FILE` overrides the session-log path.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagMode {
    Off,
    Log,
    Overlay,
    Both,
}

/// Parse the raw `FERROLITE_DIAG` value into a mode (case/space-insensitive).
pub fn parse_mode(raw: Option<&str>) -> DiagMode {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        None => DiagMode::Off,
        Some(v) => match v.as_str() {
            "" | "0" | "off" | "false" => DiagMode::Off,
            "log" => DiagMode::Log,
            "overlay" => DiagMode::Overlay,
            // "1", "both", "true", "on", or any other non-off value → both.
            _ => DiagMode::Both,
        },
    }
}

/// Cached mode, resolved once from the environment.
pub fn mode() -> DiagMode {
    static M: OnceLock<DiagMode> = OnceLock::new();
    *M.get_or_init(|| parse_mode(std::env::var("FERROLITE_DIAG").ok().as_deref()))
}

pub fn enabled() -> bool {
    !matches!(mode(), DiagMode::Off)
}
pub fn log_enabled() -> bool {
    matches!(mode(), DiagMode::Log | DiagMode::Both)
}
pub fn overlay_enabled() -> bool {
    matches!(mode(), DiagMode::Overlay | DiagMode::Both)
}

/// Per-second rate of a cumulative delta over `dt_secs` (guards dt→0).
pub fn compute_rate(delta: u64, dt_secs: f64) -> f64 {
    if dt_secs <= 0.0 {
        0.0
    } else {
        delta as f64 / dt_secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_maps_all_documented_values() {
        assert_eq!(parse_mode(None), DiagMode::Off);
        assert_eq!(parse_mode(Some("")), DiagMode::Off);
        assert_eq!(parse_mode(Some("0")), DiagMode::Off);
        assert_eq!(parse_mode(Some("off")), DiagMode::Off);
        assert_eq!(parse_mode(Some("log")), DiagMode::Log);
        assert_eq!(parse_mode(Some(" LOG ")), DiagMode::Log);
        assert_eq!(parse_mode(Some("overlay")), DiagMode::Overlay);
        assert_eq!(parse_mode(Some("1")), DiagMode::Both);
        assert_eq!(parse_mode(Some("both")), DiagMode::Both);
    }

    #[test]
    fn mode_flags_are_consistent() {
        assert!(!DiagMode::Off.eq(&DiagMode::Both));
        assert_eq!(parse_mode(Some("log")), DiagMode::Log);
        assert!(matches!(DiagMode::Log, DiagMode::Log));
    }

    #[test]
    fn compute_rate_handles_zero_dt() {
        assert_eq!(compute_rate(100, 0.0), 0.0);
        assert_eq!(compute_rate(100, 2.0), 50.0);
        assert_eq!(compute_rate(0, 1.0), 0.0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail to compile/link (module not registered yet)**

Run: `cargo test -p ferrolite-app diag::tests`
Expected: FAIL — `diag` is not yet a module of the crate.

- [ ] **Step 3: Register the module and add file I/O + init**

In `ferrolite-app/src/main.rs`, add `mod diag;` to the module list (keep alphabetical-ish; place after `mod develop;`):

```rust
mod app;
mod canvas;
mod chrome;
mod develop;
mod diag;
mod events;
```

And call `diag::init()` as the first line of `main`:

```rust
fn main() -> eframe::Result<()> {
    diag::init();
    let icon = egui::IconData {
```

Append the file-I/O + init functions to `ferrolite-app/src/diag.rs` (above the `#[cfg(test)]` module):

```rust
use std::io::Write;
use std::sync::Mutex;

/// Lazily-opened session log file (only when logging is enabled). `None` if
/// logging is off or the file could not be opened (writes then go to stderr
/// only). Never panics — diagnostics must not take down the app.
fn log_file() -> &'static Option<Mutex<std::fs::File>> {
    static F: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    F.get_or_init(|| {
        if !log_enabled() {
            return None;
        }
        let path = session_log_path();
        match std::fs::File::create(&path) {
            Ok(f) => {
                eprintln!("[diag] logging to {}", path.display());
                Some(Mutex::new(f))
            }
            Err(e) => {
                eprintln!("[diag] could not open log file {}: {e}", path.display());
                None
            }
        }
    })
}

/// Session-log path: `FERROLITE_DIAG_FILE` if set, else
/// `%LOCALAPPDATA%/ferrolite/diag-<pid>.log` (falling back to the temp dir).
fn session_log_path() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("FERROLITE_DIAG_FILE") {
        return std::path::PathBuf::from(p);
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(std::path::PathBuf::from)
        .map(|b| b.join("ferrolite"))
        .unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&base);
    base.join(format!("diag-{}.log", std::process::id()))
}

/// Open the log file eagerly (so its path prints at startup) when logging is
/// enabled. No-op otherwise. Cheap and idempotent.
pub fn init() {
    if enabled() {
        let _ = log_file();
    }
}

/// Best-effort write of a (multi-line) diagnostic block to stderr and, if open,
/// the session file. Never blocks meaningfully and never propagates errors.
pub fn write_log(block: &str) {
    eprintln!("{block}");
    if let Some(lock) = log_file() {
        if let Ok(mut f) = lock.lock() {
            let _ = writeln!(f, "{block}");
            let _ = f.flush();
        }
    }
}
```

- [ ] **Step 4: Run tests + gate**

Run: `cargo test -p ferrolite-app diag::tests`
Expected: the three tests pass.

Run: `cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean. (`write_log`/`init` unused for now is fine — they are `pub` and wired in later tasks; if clippy flags dead code, that resolves in Task 4. If a `dead_code` warning blocks `-D warnings` here, add `#[allow(dead_code)]` on `write_log` and remove it in Task 4.)

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/diag.rs ferrolite-app/src/main.rs
git commit -m "feat(app): diag module scaffold — mode parsing, session log, rate helper"
```

---

## Task 3: App-side counters — caches, lazy-load classification, retain

**Files:**
- Modify: `ferrolite-app/src/diag.rs`, `ferrolite-app/src/library/texture_cache.rs`, `ferrolite-app/src/library/thumb_pixel_cache.rs`, `ferrolite-app/src/state.rs`

**Interfaces:**
- Consumes: `enabled()` (Task 2).
- Produces:
  - Recorders (all gated internally): `tex_hit()`, `tex_miss()`, `tex_evict(n: usize)`, `pix_hit()`, `pix_miss()`, `pix_evict(n: usize)`, `retain_cancels(n: usize)`, `add_events(n: usize)`, `add_uploads(n: usize)`.
  - `enum ReqOutcome { NewSubmit, FastPath, DedupTextured, DedupPending, DedupMissing }` and `fn classify_request(textured: bool, pending: bool, missing: bool) -> ReqOutcome` + `record_request(outcome: ReqOutcome)`.
  - `struct AppCounters { .. }` and `fn app_counters() -> AppCounters` (snapshot of the globals).

- [ ] **Step 1: Write the failing test for request classification**

Add to `ferrolite-app/src/diag.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn classify_request_prioritises_textured_then_pending_then_missing() {
        assert_eq!(classify_request(false, false, false), ReqOutcome::NewSubmit);
        assert_eq!(classify_request(true, false, false), ReqOutcome::DedupTextured);
        assert_eq!(classify_request(false, true, false), ReqOutcome::DedupPending);
        assert_eq!(classify_request(false, false, true), ReqOutcome::DedupMissing);
        // Textured wins when multiple guards are true (matches request_thumbnail
        // guard order: textures, then pending, then missing).
        assert_eq!(classify_request(true, true, true), ReqOutcome::DedupTextured);
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p ferrolite-app diag::tests::classify_request_prioritises`
Expected: FAIL — `classify_request` / `ReqOutcome` undefined.

- [ ] **Step 3: Add the app-side counters, recorders, and classifier**

Append to `ferrolite-app/src/diag.rs` (above the `#[cfg(test)]` module). Add `use std::sync::atomic::{AtomicU64, Ordering};` to the top imports:

```rust
// ── App-side cumulative counters (process-global, like thumb_profile's statics).
// UI-thread-written in practice; Relaxed atomics keep them cheap and sound.
static TEX_HIT: AtomicU64 = AtomicU64::new(0);
static TEX_MISS: AtomicU64 = AtomicU64::new(0);
static TEX_EVICT: AtomicU64 = AtomicU64::new(0);
static PIX_HIT: AtomicU64 = AtomicU64::new(0);
static PIX_MISS: AtomicU64 = AtomicU64::new(0);
static PIX_EVICT: AtomicU64 = AtomicU64::new(0);
static REQ_CALLS: AtomicU64 = AtomicU64::new(0);
static REQ_NEW: AtomicU64 = AtomicU64::new(0);
static REQ_FAST: AtomicU64 = AtomicU64::new(0);
static REQ_DEDUP_TEX: AtomicU64 = AtomicU64::new(0);
static REQ_DEDUP_PENDING: AtomicU64 = AtomicU64::new(0);
static REQ_DEDUP_MISSING: AtomicU64 = AtomicU64::new(0);
static RETAIN_CANCELS: AtomicU64 = AtomicU64::new(0);
static EVENTS_DRAINED: AtomicU64 = AtomicU64::new(0);
static UPLOADS_APPLIED: AtomicU64 = AtomicU64::new(0);

#[inline]
fn add(c: &AtomicU64, n: u64) {
    if enabled() {
        c.fetch_add(n, Ordering::Relaxed);
    }
}

pub fn tex_hit() {
    add(&TEX_HIT, 1);
}
pub fn tex_miss() {
    add(&TEX_MISS, 1);
}
pub fn tex_evict(n: usize) {
    add(&TEX_EVICT, n as u64);
}
pub fn pix_hit() {
    add(&PIX_HIT, 1);
}
pub fn pix_miss() {
    add(&PIX_MISS, 1);
}
pub fn pix_evict(n: usize) {
    add(&PIX_EVICT, n as u64);
}
pub fn retain_cancels(n: usize) {
    add(&RETAIN_CANCELS, n as u64);
}
pub fn add_events(n: usize) {
    add(&EVENTS_DRAINED, n as u64);
}
pub fn add_uploads(n: usize) {
    add(&UPLOADS_APPLIED, n as u64);
}

/// How a `request_thumbnail` call was resolved (see `state::request_thumbnail`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReqOutcome {
    NewSubmit,
    FastPath,
    DedupTextured,
    DedupPending,
    DedupMissing,
}

/// Classify the outcome from the three dedup guards, in `request_thumbnail`'s
/// own precedence order (textured > pending > missing). `NewSubmit` is used
/// when none of the guards hit and there is no pixel-cache fast path — the
/// caller records `FastPath` explicitly for the pixel-cache branch.
pub fn classify_request(textured: bool, pending: bool, missing: bool) -> ReqOutcome {
    if textured {
        ReqOutcome::DedupTextured
    } else if pending {
        ReqOutcome::DedupPending
    } else if missing {
        ReqOutcome::DedupMissing
    } else {
        ReqOutcome::NewSubmit
    }
}

/// Record one classified `request_thumbnail` call (bumps the call total plus
/// the per-outcome counter). Gated internally.
pub fn record_request(outcome: ReqOutcome) {
    if !enabled() {
        return;
    }
    REQ_CALLS.fetch_add(1, Ordering::Relaxed);
    let c = match outcome {
        ReqOutcome::NewSubmit => &REQ_NEW,
        ReqOutcome::FastPath => &REQ_FAST,
        ReqOutcome::DedupTextured => &REQ_DEDUP_TEX,
        ReqOutcome::DedupPending => &REQ_DEDUP_PENDING,
        ReqOutcome::DedupMissing => &REQ_DEDUP_MISSING,
    };
    c.fetch_add(1, Ordering::Relaxed);
}

/// Immutable snapshot of the app-side cumulative counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppCounters {
    pub tex_hit: u64,
    pub tex_miss: u64,
    pub tex_evict: u64,
    pub pix_hit: u64,
    pub pix_miss: u64,
    pub pix_evict: u64,
    pub req_calls: u64,
    pub req_new: u64,
    pub req_fast: u64,
    pub req_dedup_tex: u64,
    pub req_dedup_pending: u64,
    pub req_dedup_missing: u64,
    pub retain_cancels: u64,
    pub events_drained: u64,
    pub uploads_applied: u64,
}

pub fn app_counters() -> AppCounters {
    let l = |c: &AtomicU64| c.load(Ordering::Relaxed);
    AppCounters {
        tex_hit: l(&TEX_HIT),
        tex_miss: l(&TEX_MISS),
        tex_evict: l(&TEX_EVICT),
        pix_hit: l(&PIX_HIT),
        pix_miss: l(&PIX_MISS),
        pix_evict: l(&PIX_EVICT),
        req_calls: l(&REQ_CALLS),
        req_new: l(&REQ_NEW),
        req_fast: l(&REQ_FAST),
        req_dedup_tex: l(&REQ_DEDUP_TEX),
        req_dedup_pending: l(&REQ_DEDUP_PENDING),
        req_dedup_missing: l(&REQ_DEDUP_MISSING),
        retain_cancels: l(&RETAIN_CANCELS),
        events_drained: l(&EVENTS_DRAINED),
        uploads_applied: l(&UPLOADS_APPLIED),
    }
}
```

- [ ] **Step 4: Run the classifier test (expect PASS)**

Run: `cargo test -p ferrolite-app diag::tests::classify_request_prioritises`
Expected: PASS.

- [ ] **Step 5: Instrument the texture cache**

In `ferrolite-app/src/library/texture_cache.rs`, record hit/miss in `get`, evict in `insert` and `clear`:

```rust
    pub fn get(&mut self, id: i64) -> Option<&egui::TextureHandle> {
        if self.textures.contains_key(&id) {
            crate::diag::tex_hit();
            self.lru.touch(id);
            self.textures.get(&id)
        } else {
            crate::diag::tex_miss();
            None
        }
    }
    pub fn insert(&mut self, id: i64, tex: egui::TextureHandle) {
        if let Some(evict) = self.lru.insert(id) {
            if let Some(old) = self.textures.remove(&evict) {
                crate::diag::tex_evict(1);
                self.retiring.push(old);
            }
        }
        if let Some(old) = self.textures.insert(id, tex) {
            self.retiring.push(old); // replacing same id: retire old handle, don't drop mid-frame
        }
    }
```

And in `clear` (count the bulk retirement as evictions):

```rust
    pub fn clear(&mut self) {
        crate::diag::tex_evict(self.textures.len());
        self.retiring.extend(self.textures.drain().map(|(_, h)| h));
        self.lru.clear();
    }
```

- [ ] **Step 6: Instrument the pixel cache**

In `ferrolite-app/src/library/thumb_pixel_cache.rs`, record hit/miss in `get`, evict in `insert`:

```rust
    pub fn get(&mut self, id: i64) -> Option<(Vec<u8>, u32, u32)> {
        if self.map.contains_key(&id) {
            crate::diag::pix_hit();
            self.touch(id);
            let e = self.map.get(&id)?;
            Some((e.rgba.clone(), e.w, e.h))
        } else {
            crate::diag::pix_miss();
            None
        }
    }

    pub fn insert(&mut self, id: i64, rgba: Vec<u8>, w: u32, h: u32) {
        self.touch(id);
        self.map.insert(id, Entry { rgba, w, h });
        while self.order.len() > self.capacity {
            let evict = self.order.remove(0);
            self.map.remove(&evict);
            crate::diag::pix_evict(1);
        }
    }
```

- [ ] **Step 7: Instrument `request_thumbnail` and `retain_visible_thumbnail_jobs`**

In `ferrolite-app/src/state.rs`, `request_thumbnail`: replace the combined early-return guard with classified recording. Change the opening of the function:

```rust
    pub fn request_thumbnail(&mut self, ctx: &egui::Context, image_id: i64) {
        let textured = self.textures.contains(image_id);
        let pending = self.thumb_pending.contains(&image_id);
        let missing = self.thumb_missing.contains(&image_id);
        if textured || pending || missing {
            crate::diag::record_request(crate::diag::classify_request(textured, pending, missing));
            return;
        }
        // Fast path: pixels already decoded this session → re-upload directly,
        // no job / DB read / JPEG decode (Bug B). Routed through the same
        // per-frame upload budget as ThumbReady via `pending_uploads`.
        if let Some((rgba, w, h)) = self.thumb_pixels.get(image_id) {
            crate::diag::record_request(crate::diag::ReqOutcome::FastPath);
            self.pending_uploads.push((image_id, rgba, w, h));
            ctx.request_repaint();
            return;
        }
        crate::diag::record_request(crate::diag::ReqOutcome::NewSubmit);
        self.thumb_pending.insert(image_id);
```

(Leave the rest of `request_thumbnail` — the job submit and `thumb_handles.insert` — unchanged.)

In `retain_visible_thumbnail_jobs`, record the number cancelled this call. Change the loop to count:

```rust
    pub fn retain_visible_thumbnail_jobs(&mut self, visible: &HashSet<i64>) {
        let offscreen: Vec<i64> = self
            .thumb_handles
            .keys()
            .copied()
            .filter(|id| !visible.contains(id))
            .collect();
        crate::diag::retain_cancels(offscreen.len());
        for id in offscreen {
            if let Some(handle) = self.thumb_handles.remove(&id) {
                self.jobs.cancel(handle.id()); // drop it from the queue if still pending
                handle.cancel(); // signal it if already running
            }
            self.thumb_pending.remove(&id);
        }
    }
```

- [ ] **Step 8: Run the full app test suite + gate**

Run: `cargo test -p ferrolite-app`
Expected: PASS (existing `request_thumbnail`/`retain_visible_thumbnail_jobs`/cache tests unaffected — the classification is additive; the pixel-cache fast path still pushes to `pending_uploads`).

Run: `cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add ferrolite-app/src/diag.rs ferrolite-app/src/library/texture_cache.rs ferrolite-app/src/library/thumb_pixel_cache.rs ferrolite-app/src/state.rs
git commit -m "feat(app): diag counters for caches, request_thumbnail classes, retain cancels"
```

---

## Task 4: Per-frame wiring + 1/sec log emit

**Files:**
- Modify: `ferrolite-app/src/diag.rs`, `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `JobStats` (Task 1), `AppCounters`/`app_counters()`/`compute_rate` (Tasks 2–3), `write_log` (Task 2).
- Produces:
  - `struct Gauges { thumb_pending: usize, thumb_missing: usize, thumb_handles: usize, pending_uploads: usize, active_ingests: usize, ingest_done: usize, ingest_total: usize, uploads_cap: usize }`
  - `struct Snapshot { .. }` + `fn build_snapshot(dt: f64, prev: &AppCounters, cur: &AppCounters, prev_frame: &AppCounters, jobs: JobStats, g: Gauges, frame_ms: f64, max_frame_ms: f64, repaint_forced: bool) -> Snapshot`
  - `fn format_log(s: &Snapshot) -> String`
  - `struct DiagState` with `fn new() -> Self` and `fn tick(&mut self, now: std::time::Instant, jobs: JobStats, g: Gauges, frame_ms: f64, repaint_forced: bool) -> Option<Snapshot>` (returns `Some` ~1×/sec)
  - `DiagState::overlay_visible: bool`, `fn toggle_overlay(&mut self)`, `fn last_snapshot(&self) -> Option<&Snapshot>`

- [ ] **Step 1: Write failing tests for snapshot + log format + tick cadence**

Add to `ferrolite-app/src/diag.rs` `#[cfg(test)] mod tests`:

```rust
    fn sample_gauges() -> Gauges {
        Gauges {
            thumb_pending: 640,
            thumb_missing: 0,
            thumb_handles: 640,
            pending_uploads: 210,
            active_ingests: 0,
            ingest_done: 3320,
            ingest_total: 3320,
            uploads_cap: 16,
        }
    }

    #[test]
    fn build_snapshot_computes_per_second_rates() {
        let prev = AppCounters { tex_hit: 100, ..Default::default() };
        let cur = AppCounters { tex_hit: 140, ..Default::default() };
        let jobs = ferrolite_jobs::JobStats::default();
        let s = build_snapshot(
            2.0, &prev, &cur, &prev, jobs, sample_gauges(), 6.2, 11.0, true,
        );
        // 40 hits over 2s = 20/s.
        assert!((s.tex_hit_per_s - 20.0).abs() < 1e-6);
    }

    #[test]
    fn format_log_contains_key_fields() {
        let cur = AppCounters {
            req_calls: 44,
            req_new: 2,
            req_fast: 1,
            req_dedup_tex: 30,
            req_dedup_pending: 11,
            ..Default::default()
        };
        let mut jobs = ferrolite_jobs::JobStats::default();
        jobs.submitted[ferrolite_jobs::Priority::Visible.index()] = 812;
        jobs.active = 6;
        jobs.pending[ferrolite_jobs::Priority::Visible.index()] = 634;
        let s = build_snapshot(
            1.0, &AppCounters::default(), &cur, &cur, jobs, sample_gauges(), 6.2, 11.0, true,
        );
        let out = format_log(&s);
        assert!(out.contains("[diag"), "has the diag prefix");
        assert!(out.contains("frame 6.2ms"), "shows frame time");
        assert!(out.contains("sub I/V/B 0/812/0"), "shows per-priority submits");
        assert!(out.contains("pending 640"), "shows lazy-load pending gauge");
        assert!(out.contains("uploads"), "shows uploads line");
    }

    #[test]
    fn tick_emits_at_most_once_per_second() {
        use std::time::{Duration, Instant};
        let mut d = DiagState::new();
        let t0 = Instant::now();
        let jobs = ferrolite_jobs::JobStats::default();
        // First tick establishes the baseline (no emit).
        assert!(d.tick(t0, jobs, sample_gauges(), 5.0, false).is_none());
        // 100ms later: below the 1s threshold → no emit.
        assert!(d
            .tick(t0 + Duration::from_millis(100), jobs, sample_gauges(), 5.0, false)
            .is_none());
        // 1.1s after baseline → emit.
        assert!(d
            .tick(t0 + Duration::from_millis(1100), jobs, sample_gauges(), 5.0, false)
            .is_some());
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p ferrolite-app diag::tests`
Expected: FAIL — `Gauges`/`Snapshot`/`build_snapshot`/`format_log`/`DiagState` undefined.

- [ ] **Step 3: Implement `Gauges`, `Snapshot`, `build_snapshot`, `format_log`, `DiagState`**

Append to `ferrolite-app/src/diag.rs` (above the `#[cfg(test)]` module). Add `use ferrolite_jobs::JobStats;` and `use std::time::Instant;` to the imports:

```rust
/// Live sizes read straight off `AppState` at tick time (not counters).
#[derive(Debug, Clone, Copy, Default)]
pub struct Gauges {
    pub thumb_pending: usize,
    pub thumb_missing: usize,
    pub thumb_handles: usize,
    pub pending_uploads: usize,
    pub active_ingests: usize,
    pub ingest_done: usize,
    pub ingest_total: usize,
    pub uploads_cap: usize,
}

/// Everything the log/overlay render, precomputed once per tick.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub dt: f64,
    pub frame_ms: f64,
    pub max_frame_ms: f64,
    pub repaint_forced: bool,
    pub jobs: JobStats,
    pub g: Gauges,
    // Per-second rates.
    pub tex_hit_per_s: f64,
    pub tex_miss_per_s: f64,
    pub tex_evict_per_s: f64,
    pub pix_hit_per_s: f64,
    pub pix_miss_per_s: f64,
    pub pix_evict_per_s: f64,
    // Per-frame counts (last frame).
    pub ev_per_frame: u64,
    pub req_per_frame: u64,
    pub uploads_per_frame: u64,
    // Last-frame request breakdown (deltas vs previous frame).
    pub req_new_f: u64,
    pub req_fast_f: u64,
    pub req_dedup_tex_f: u64,
    pub req_dedup_pending_f: u64,
    pub req_dedup_missing_f: u64,
    // Cache sizes (caps are fixed constants, shown for context).
    pub cur: AppCounters,
}

#[allow(clippy::too_many_arguments)]
pub fn build_snapshot(
    dt: f64,
    prev: &AppCounters,
    cur: &AppCounters,
    prev_frame: &AppCounters,
    jobs: JobStats,
    g: Gauges,
    frame_ms: f64,
    max_frame_ms: f64,
    repaint_forced: bool,
) -> Snapshot {
    let d = |a: u64, b: u64| a.saturating_sub(b);
    Snapshot {
        dt,
        frame_ms,
        max_frame_ms,
        repaint_forced,
        jobs,
        g,
        tex_hit_per_s: compute_rate(d(cur.tex_hit, prev.tex_hit), dt),
        tex_miss_per_s: compute_rate(d(cur.tex_miss, prev.tex_miss), dt),
        tex_evict_per_s: compute_rate(d(cur.tex_evict, prev.tex_evict), dt),
        pix_hit_per_s: compute_rate(d(cur.pix_hit, prev.pix_hit), dt),
        pix_miss_per_s: compute_rate(d(cur.pix_miss, prev.pix_miss), dt),
        pix_evict_per_s: compute_rate(d(cur.pix_evict, prev.pix_evict), dt),
        ev_per_frame: d(cur.events_drained, prev_frame.events_drained),
        req_per_frame: d(cur.req_calls, prev_frame.req_calls),
        uploads_per_frame: d(cur.uploads_applied, prev_frame.uploads_applied),
        req_new_f: d(cur.req_new, prev_frame.req_new),
        req_fast_f: d(cur.req_fast, prev_frame.req_fast),
        req_dedup_tex_f: d(cur.req_dedup_tex, prev_frame.req_dedup_tex),
        req_dedup_pending_f: d(cur.req_dedup_pending, prev_frame.req_dedup_pending),
        req_dedup_missing_f: d(cur.req_dedup_missing, prev_frame.req_dedup_missing),
        cur: *cur,
    }
}

/// Render the multi-line ~1/sec log block (also reused, compacted, by the overlay).
pub fn format_log(s: &Snapshot) -> String {
    let j = &s.jobs;
    let g = &s.g;
    let dedup = s.req_dedup_tex_f + s.req_dedup_pending_f + s.req_dedup_missing_f;
    format!(
        "[diag +{dt:.1}s] frame {fms:.1}ms(max {mx:.1}) ev/f {ev} repaint {rp}\n\
         \x20jobs  sub I/V/B {si}/{sv}/{sb}  disp {disp}  done {done}  cxl(pre){cxp}  panic {pan}\n\
         \x20      active {act}  pending I/V/B {pi}/{pv}/{pb}  cancel removed {crem}/absent {cabs}\n\
         \x20thumb req/f {req} = new {rn} + fast {rf} + dedup {dd} (tex {rt}/pend {rpd}/miss {rms})\n\
         \x20      pending {tp}  handles {th}  missing {tm}  retain cxl {rc}\n\
         \x20cache tex h/s {thh:.0} m/s {thm:.0} ev/s {the:.0} | pix h/s {pxh:.0} m/s {pxm:.0} ev/s {pxe:.0}\n\
         \x20uploads {up}/{cap} cap  backlog {bk}\n\
         \x20ingest active {ai}  done {idn}/{itot}",
        dt = s.dt,
        fms = s.frame_ms,
        mx = s.max_frame_ms,
        ev = s.ev_per_frame,
        rp = if s.repaint_forced { "forced" } else { "no" },
        si = j.submitted[ferrolite_jobs::Priority::Interactive.index()],
        sv = j.submitted[ferrolite_jobs::Priority::Visible.index()],
        sb = j.submitted[ferrolite_jobs::Priority::Background.index()],
        disp = j.dispatched,
        done = j.completed,
        cxp = j.cancelled_before_dispatch,
        pan = j.panicked,
        act = j.active,
        pi = j.pending[ferrolite_jobs::Priority::Interactive.index()],
        pv = j.pending[ferrolite_jobs::Priority::Visible.index()],
        pb = j.pending[ferrolite_jobs::Priority::Background.index()],
        crem = j.cancel_removed,
        cabs = j.cancel_absent,
        req = s.req_per_frame,
        rn = s.req_new_f,
        rf = s.req_fast_f,
        dd = dedup,
        rt = s.req_dedup_tex_f,
        rpd = s.req_dedup_pending_f,
        rms = s.req_dedup_missing_f,
        tp = g.thumb_pending,
        th = g.thumb_handles,
        tm = g.thumb_missing,
        rc = s.cur.retain_cancels,
        thh = s.tex_hit_per_s,
        thm = s.tex_miss_per_s,
        the = s.tex_evict_per_s,
        pxh = s.pix_hit_per_s,
        pxm = s.pix_miss_per_s,
        pxe = s.pix_evict_per_s,
        up = s.uploads_per_frame,
        cap = g.uploads_cap,
        bk = g.pending_uploads,
        ai = g.active_ingests,
        idn = g.ingest_done,
        itot = g.ingest_total,
    )
}

/// Per-frame diagnostic state held on `FerroliteApp`. Drives the ~1/sec tick,
/// per-frame deltas, and the overlay toggle. Cheap to hold when diag is off
/// (it is simply never ticked).
pub struct DiagState {
    last_tick: Option<Instant>,
    prev_tick: AppCounters,
    prev_frame: AppCounters,
    max_frame_ms: f64,
    pub overlay_visible: bool,
    last_snapshot: Option<Snapshot>,
}

impl DiagState {
    pub fn new() -> Self {
        Self {
            last_tick: None,
            prev_tick: AppCounters::default(),
            prev_frame: AppCounters::default(),
            max_frame_ms: 0.0,
            overlay_visible: overlay_enabled(),
            last_snapshot: None,
        }
    }

    pub fn toggle_overlay(&mut self) {
        self.overlay_visible = !self.overlay_visible;
    }

    pub fn last_snapshot(&self) -> Option<&Snapshot> {
        self.last_snapshot.as_ref()
    }

    /// Call once at the end of every `update()`. Tracks per-frame deltas and the
    /// running max frame time; emits (and caches) a `Snapshot` at most ~1×/sec.
    pub fn tick(
        &mut self,
        now: Instant,
        jobs: JobStats,
        g: Gauges,
        frame_ms: f64,
        repaint_forced: bool,
    ) -> Option<Snapshot> {
        if frame_ms > self.max_frame_ms {
            self.max_frame_ms = frame_ms;
        }
        let cur = app_counters();
        let out = match self.last_tick {
            None => {
                // Establish baselines; no emit on the very first frame.
                self.last_tick = Some(now);
                None
            }
            Some(last) => {
                let dt = now.duration_since(last).as_secs_f64();
                if dt < 1.0 {
                    None
                } else {
                    let snap = build_snapshot(
                        dt,
                        &self.prev_tick,
                        &cur,
                        &self.prev_frame,
                        jobs,
                        g,
                        frame_ms,
                        self.max_frame_ms,
                        repaint_forced,
                    );
                    self.last_tick = Some(now);
                    self.prev_tick = cur;
                    self.max_frame_ms = 0.0;
                    self.last_snapshot = Some(snap.clone());
                    Some(snap)
                }
            }
        };
        // Per-frame delta baseline advances every frame.
        self.prev_frame = cur;
        out
    }
}

impl Default for DiagState {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Run the diag tests (expect PASS)**

Run: `cargo test -p ferrolite-app diag::tests`
Expected: all diag tests pass.

- [ ] **Step 5: Hold `DiagState` on the app and wire the per-frame tick**

In `ferrolite-app/src/app.rs`:

Add a field to the `FerroliteApp` struct (defined near the top of the file, in the block starting around line 24) — place it with the other state fields:

```rust
    diag: crate::diag::DiagState,
```

Initialise it in `FerroliteApp::new` (the constructor returning `Self { .. }`):

```rust
            diag: crate::diag::DiagState::new(),
```

At the very top of `update` (right after `self.state.textures.begin_frame();`, ~line 1174), capture the frame start when enabled:

```rust
        let diag_t0 = crate::diag::enabled().then(std::time::Instant::now);
```

Count events drained: in the `while let Ok(event) = self.state.rx.try_recv()` loop, increment a local counter. Add before the loop:

```rust
        let mut events_this_frame = 0usize;
```

and as the first line inside the loop body (right after `while let Ok(event) = ...) {`):

```rust
            events_this_frame += 1;
```

Record events + uploads after the drain loop finishes. Immediately after the existing block that ends with `if !self.state.pending_uploads.is_empty() { ctx.request_repaint(); }` (~line 1313–1315), add:

```rust
        let repaint_forced = !self.state.pending_uploads.is_empty();
        crate::diag::add_events(events_this_frame);
        crate::diag::add_uploads(uploads_this_frame);
```

At the **end** of `update` — immediately after `window_resize(ctx);` (line 2004), before the method's closing brace — add the tick + log emit:

```rust
        if let Some(t0) = diag_t0 {
            let frame_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let gauges = crate::diag::Gauges {
                thumb_pending: self.state.thumb_pending.len(),
                thumb_missing: self.state.thumb_missing.len(),
                thumb_handles: self.state.thumb_handles.len(),
                pending_uploads: self.state.pending_uploads.len(),
                active_ingests: self.state.active_ingests,
                ingest_done: self.state.ingest_done,
                ingest_total: self.state.ingest_total,
                uploads_cap: MAX_THUMB_UPLOADS_PER_FRAME,
            };
            let stats = self.state.jobs.stats();
            if let Some(snap) =
                self.diag
                    .tick(std::time::Instant::now(), stats, gauges, frame_ms, repaint_forced)
            {
                if crate::diag::log_enabled() {
                    crate::diag::write_log(&crate::diag::format_log(&snap));
                }
            }
        }
```

> Note: `repaint_forced` and `uploads_this_frame` are computed earlier in `update`; both are in scope at the end of the method. `MAX_THUMB_UPLOADS_PER_FRAME` is the existing const at app.rs:836.

- [ ] **Step 6: Verify the workspace builds and tests pass**

Run: `cargo test -p ferrolite-app`
Expected: PASS. Also confirm it compiles: `cargo build -p ferrolite-app`.

- [ ] **Step 7: Gate + commit**

Run: `cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean. (If `write_log` had a temporary `#[allow(dead_code)]` from Task 2, remove it now — it is used here.)

```bash
git add ferrolite-app/src/diag.rs ferrolite-app/src/app.rs
git commit -m "feat(app): per-frame diag tick + throttled 1/sec log trace"
```

---

## Task 5: Live egui overlay + F9 toggle

**Files:**
- Modify: `ferrolite-app/src/diag.rs`, `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `Snapshot`, `DiagState` (Task 4).
- Produces: `fn format_overlay(s: &Snapshot) -> String`; `fn draw_overlay(ctx: &egui::Context, s: &Snapshot)`.

- [ ] **Step 1: Write the failing test for the compact overlay text**

Add to `ferrolite-app/src/diag.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn format_overlay_contains_core_gauges() {
        let mut jobs = ferrolite_jobs::JobStats::default();
        jobs.active = 6;
        jobs.pending[ferrolite_jobs::Priority::Visible.index()] = 634;
        let s = build_snapshot(
            1.0,
            &AppCounters::default(),
            &AppCounters::default(),
            &AppCounters::default(),
            jobs,
            sample_gauges(),
            6.2,
            11.0,
            false,
        );
        let out = format_overlay(&s);
        assert!(out.contains("frame"), "overlay shows frame time");
        assert!(out.contains("active 6"), "overlay shows active jobs");
        assert!(out.contains("pending"), "overlay shows a pending gauge");
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p ferrolite-app diag::tests::format_overlay_contains_core_gauges`
Expected: FAIL — `format_overlay` undefined.

- [ ] **Step 3: Implement `format_overlay` and `draw_overlay`**

Append to `ferrolite-app/src/diag.rs` (above the `#[cfg(test)]` module):

```rust
/// Compact multi-line text for the on-screen overlay (a denser view of the same
/// snapshot the log formats).
pub fn format_overlay(s: &Snapshot) -> String {
    let j = &s.jobs;
    let g = &s.g;
    format!(
        "frame {fms:.1}ms (max {mx:.1})  ev/f {ev}\n\
         jobs sub I/V/B {si}/{sv}/{sb}  active {act}\n\
         pending I/V/B {pi}/{pv}/{pb}\n\
         cancel removed {crem}/absent {cabs}  cxl(pre) {cxp}  panic {pan}\n\
         req/f {req} new {rn} fast {rf} dedup {dd}\n\
         thumb pending {tp} handles {th} missing {tm}\n\
         tex h/s {thh:.0} ev/s {the:.0} | pix h/s {pxh:.0} ev/s {pxe:.0}\n\
         uploads/f {up}/{cap}  backlog {bk}\n\
         ingest {idn}/{itot} active {ai}",
        fms = s.frame_ms,
        mx = s.max_frame_ms,
        ev = s.ev_per_frame,
        si = j.submitted[ferrolite_jobs::Priority::Interactive.index()],
        sv = j.submitted[ferrolite_jobs::Priority::Visible.index()],
        sb = j.submitted[ferrolite_jobs::Priority::Background.index()],
        act = j.active,
        pi = j.pending[ferrolite_jobs::Priority::Interactive.index()],
        pv = j.pending[ferrolite_jobs::Priority::Visible.index()],
        pb = j.pending[ferrolite_jobs::Priority::Background.index()],
        crem = j.cancel_removed,
        cabs = j.cancel_absent,
        cxp = j.cancelled_before_dispatch,
        pan = j.panicked,
        req = s.req_per_frame,
        rn = s.req_new_f,
        rf = s.req_fast_f,
        dd = s.req_dedup_tex_f + s.req_dedup_pending_f + s.req_dedup_missing_f,
        tp = g.thumb_pending,
        th = g.thumb_handles,
        tm = g.thumb_missing,
        thh = s.tex_hit_per_s,
        the = s.tex_evict_per_s,
        pxh = s.pix_hit_per_s,
        pxe = s.pix_evict_per_s,
        up = s.uploads_per_frame,
        cap = g.uploads_cap,
        bk = g.pending_uploads,
        idn = g.ingest_done,
        itot = g.ingest_total,
        ai = g.active_ingests,
    )
}

/// Paint the diagnostics overlay: a non-interactive, top-right, monospace panel
/// drawn on the tooltip layer so it floats above the app. Call at the end of
/// `update()` only when the overlay is enabled AND visible.
pub fn draw_overlay(ctx: &egui::Context, s: &Snapshot) {
    egui::Area::new(egui::Id::new("ferrolite-diag-overlay"))
        .order(egui::Order::Tooltip)
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_black_alpha(200))
                .inner_margin(egui::Margin::same(8.0))
                .rounding(egui::Rounding::same(4.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format_overlay(s))
                            .monospace()
                            .size(11.0)
                            .color(egui::Color32::from_rgb(120, 255, 120)),
                    );
                });
        });
}
```

- [ ] **Step 4: Run the overlay test (expect PASS)**

Run: `cargo test -p ferrolite-app diag::tests::format_overlay_contains_core_gauges`
Expected: PASS.

- [ ] **Step 5: Wire F9 toggle + overlay draw in `update`**

In `ferrolite-app/src/app.rs`:

Add the F9 toggle right after the `diag_t0` capture near the top of `update` (~line 1175):

```rust
        if crate::diag::enabled() && ctx.input(|i| i.key_pressed(egui::Key::F9)) {
            self.diag.toggle_overlay();
        }
```

Extend the end-of-`update` diag block (added in Task 4) so the overlay is drawn every frame from the cached snapshot when visible. Replace the inner `if let Some(snap) = self.diag.tick(..) { .. }` block with:

```rust
            if let Some(snap) =
                self.diag
                    .tick(std::time::Instant::now(), stats, gauges, frame_ms, repaint_forced)
            {
                if crate::diag::log_enabled() {
                    crate::diag::write_log(&crate::diag::format_log(&snap));
                }
            }
            if crate::diag::overlay_enabled() && self.diag.overlay_visible {
                if let Some(snap) = self.diag.last_snapshot() {
                    crate::diag::draw_overlay(ctx, snap);
                    ctx.request_repaint(); // keep the live view refreshing
                }
            }
```

> The overlay's `request_repaint` is intentional and applies ONLY when the overlay is enabled+visible (dev-mode); it never runs when diag is off, preserving the idle-frame guarantee for normal builds.

- [ ] **Step 6: Build + gate**

Run: `cargo build -p ferrolite-app && cargo test -p ferrolite-app`
Expected: PASS.

Run: `cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/diag.rs ferrolite-app/src/app.rs
git commit -m "feat(app): live diag overlay with F9 toggle"
```

---

## Task 6: Shutdown line

**Files:**
- Modify: `ferrolite-app/src/diag.rs`, `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `JobStats` (Task 1), `write_log` (Task 2).
- Produces: `fn format_shutdown(before: JobStats, joined: bool, timeout_ms: u64, on_exit_ms: f64) -> String`.

- [ ] **Step 1: Write the failing test**

Add to `ferrolite-app/src/diag.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn format_shutdown_reports_join_result_and_counts() {
        let mut before = ferrolite_jobs::JobStats::default();
        before.active = 6;
        before.pending[ferrolite_jobs::Priority::Visible.index()] = 640;
        let detached = format_shutdown(before, false, 75, 78.0);
        assert!(detached.contains("[diag close]"));
        assert!(detached.contains("active 6"));
        assert!(detached.contains("pending 640"));
        assert!(detached.contains("joined=false"));
        assert!(detached.contains("detach@75ms"));
        assert!(detached.contains("on_exit 78"));

        let joined = format_shutdown(ferrolite_jobs::JobStats::default(), true, 75, 3.0);
        assert!(joined.contains("joined=true"));
        assert!(!joined.contains("detach@"), "no detach note when joined");
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p ferrolite-app diag::tests::format_shutdown_reports_join_result_and_counts`
Expected: FAIL — `format_shutdown` undefined.

- [ ] **Step 3: Implement `format_shutdown`**

Append to `ferrolite-app/src/diag.rs` (above the `#[cfg(test)]` module):

```rust
/// One-line close-path report (emitted from `on_exit`, where the overlay is
/// already gone). Shows work in flight at close and the bounded-join outcome.
pub fn format_shutdown(before: JobStats, joined: bool, timeout_ms: u64, on_exit_ms: f64) -> String {
    let pending: u64 = before.pending.iter().sum();
    let detach = if joined {
        String::new()
    } else {
        format!("(detach@{timeout_ms}ms)")
    };
    format!(
        "[diag close] active {act} pending {pend}  joined={joined}{detach}  on_exit {ms:.0}ms",
        act = before.active,
        pend = pending,
        ms = on_exit_ms,
    )
}
```

- [ ] **Step 4: Run the test (expect PASS)**

Run: `cargo test -p ferrolite-app diag::tests::format_shutdown_reports_join_result_and_counts`
Expected: PASS.

- [ ] **Step 5: Emit the shutdown line from `on_exit`**

In `ferrolite-app/src/app.rs`, replace the body of `on_exit` (lines 2028–2040) with a diag-instrumented version that measures duration and reports before/after:

```rust
    fn on_exit(&mut self) {
        let t0 = crate::diag::enabled().then(std::time::Instant::now);
        let before = crate::diag::enabled().then(|| self.state.jobs.stats());

        self.state.cancel_pending_jobs();
        self.state.jobs.request_shutdown();
        let timeout_ms = 75u64;
        let joined = self
            .state
            .jobs
            .join_with_timeout(std::time::Duration::from_millis(timeout_ms));
        if !joined {
            eprintln!(
                "ferrolite: worker(s) still running at close after {timeout_ms}ms; detaching so the app can exit"
            );
        }

        if let (Some(t0), Some(before)) = (t0, before) {
            let on_exit_ms = t0.elapsed().as_secs_f64() * 1000.0;
            crate::diag::write_log(&crate::diag::format_shutdown(
                before, joined, timeout_ms, on_exit_ms,
            ));
        }
    }
```

> This preserves the existing 75 ms bounded-join behaviour exactly (same timeout, same detach message) and only adds the gated diagnostic emit. `write_log` flushes the file, so the close line survives window teardown.

- [ ] **Step 6: Build + gate**

Run: `cargo build -p ferrolite-app && cargo test -p ferrolite-app`
Expected: PASS.

Run: `cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/diag.rs ferrolite-app/src/app.rs
git commit -m "feat(app): diag shutdown line (in-flight jobs + join outcome)"
```

---

## Task 7: Workspace gate + author hand-off

**Files:** none (verification only).

- [ ] **Step 1: Full workspace gate**

Run:
```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all clean/green. (If `cargo test` hits `LNK1104` on Windows, re-run `cargo test --workspace` with an isolated `CARGO_TARGET_DIR`, e.g. `CARGO_TARGET_DIR=target-diag cargo test --workspace`.)

- [ ] **Step 2: Smoke-check the flag off (zero overhead)**

Run the app WITHOUT the flag and confirm no `[diag]` output and no overlay:
```bash
cargo run -p ferrolite-app
```
Expected: no `[diag] logging to ...` line at startup, no overlay, behaviour identical to `main`.

- [ ] **Step 3: HOLD for the author's hands-on visual test (CLAUDE.md)**

Do NOT merge/finish. Hand the author these repro commands and wait for feedback:

```bash
# log + overlay
FERROLITE_DIAG=1 cargo run -p ferrolite-app
# then: scroll Library grid fully down, fully back up; toggle overlay with F9;
# then close the app. Read the diag log file (path printed at startup) —
# especially the final [diag close] line for the shutdown-hang numbers.
```

On Windows PowerShell:
```powershell
$env:FERROLITE_DIAG = "1"; cargo run -p ferrolite-app
```

Address any issues the author finds, then use superpowers:finishing-a-development-branch.

---

## Self-Review

**1. Spec coverage:**
- Job system per-priority submit/dispatch/complete/cancel/panic, queue depth per bucket, active, cancel removed-vs-absent → Task 1 (`JobStats`, `stats()`, worker-loop + `cancel` counting). ✓
- Lazy-load: `request_thumbnail` calls/frame classified (new/fast/dedup×3), sizes of pending/missing/handles, retain cancels/frame → Task 3 (classify + record) + Task 4 (gauges + per-frame deltas). ✓
- Caches: tex + pixel size/hit/miss/evict per second → Task 3 (recorders) + Task 4 (rates). Sizes: caps are fixed constants; live counts shown via hit/miss/evict rates and the pending/handles gauges. ✓
- Per-frame: pending_uploads backlog, uploads vs cap, events drained/frame, repaint forced, frame time → Task 4. ✓
- Ingest: active_ingests, done/total → Task 4 (`Gauges`). ✓
- Viewer flood: derived from `submitted[Visible]` (Task 1) — no VT change, per design. Develop→Library `textures.clear()` re-uploads → captured by `tex_evict` on `clear()` (Task 3) + subsequent `tex_miss`. ✓
- Shutdown: in-flight/queued at close, join result/timeout, on_exit duration → Task 6. ✓
- Env flag `FERROLITE_DIAG` mode-valued + `FERROLITE_DIAG_FILE`; zero overhead off → Task 2 (parse/gate/file) + gating throughout. ✓
- Both log + overlay; F9 toggle → Tasks 4 & 5. ✓

**2. Placeholder scan:** No TBD/TODO; every code step contains complete, compilable code and exact commands. ✓

**3. Type consistency:** `JobStats` field names/types identical across Tasks 1, 4, 5, 6. `AppCounters`, `Gauges`, `Snapshot`, `ReqOutcome`, `DiagMode` defined once (Tasks 2–4) and referenced consistently. `classify_request`/`record_request`/`build_snapshot`/`format_log`/`format_overlay`/`format_shutdown` signatures match their call sites. `Queue::cancel -> bool` matches its use in `JobSystem::cancel`. `MAX_THUMB_UPLOADS_PER_FRAME` (app.rs:836) reused, not redefined. ✓

> Note on unit-testing global counters: the app-side counters are process-global atomics (matching `thumb_profile`), so exact-count assertions across parallel tests would be flaky. The plan therefore unit-tests the **pure logic** — `classify_request`, `compute_rate`, `build_snapshot` rate math, `format_log`/`format_overlay`/`format_shutdown` content, and `DiagState::tick` cadence — while the `ferrolite-jobs` counters get real counting tests via per-instance `Shared` (isolated). This covers the meaningful logic without global-state test flakiness.
