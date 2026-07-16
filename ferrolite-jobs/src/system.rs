//! Fixed-size worker pool driving the priority [`Queue`]. We use our own threads
//! (not rayon) so queued work can be reprioritized before it starts; rayon does
//! not expose priorities. Panics in jobs are caught so one bad job never downs
//! the pool.

use crate::diag::{DiagCounters, JobStats};
use crate::priority::{CancelToken, JobId, Priority};
use crate::queue::{Queue, QueuedJob};
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
    diag_on: bool,
    diag: DiagCounters,
}

pub struct JobSystem {
    shared: Arc<Shared>,
    workers: Mutex<Vec<JoinHandle<()>>>,
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

    pub fn reprioritize(&self, id: JobId, priority: Priority) {
        self.shared
            .queue
            .lock()
            .expect("queue mutex")
            .reprioritize(id, priority);
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

    /// Drop a still-pending job from the queue (no-op if already running/done).
    pub fn cancel(&self, id: JobId) {
        let removed = self.shared.queue.lock().expect("queue mutex").cancel(id);
        if self.shared.diag_on {
            self.shared.diag.inc_cancel(removed);
        }
    }

    /// Signal all workers to stop pulling new jobs. Idempotent. In-flight jobs
    /// keep running until they return (or observe cancellation cooperatively).
    pub fn request_shutdown(&self) {
        // Flip the flag and notify while holding the queue mutex. A worker
        // checks `shutdown` and parks in `cvar.wait` under this same lock, so
        // taking it here closes the lost-wakeup window: without the lock, a
        // `notify_all` landing between a worker's `!shutdown` check and its
        // `wait` call is missed, the worker parks forever, and `Drop`'s join
        // deadlocks (a rare, timing-dependent hang). Holding the lock forces us
        // to serialize with the worker: it either hasn't parked yet (and will
        // see the flag on its next check) or is already parked (and the
        // notify reaches it).
        let _guard = self.shared.queue.lock().expect("queue mutex");
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
}

impl Drop for JobSystem {
    fn drop(&mut self) {
        self.request_shutdown();
        // If join_with_timeout already drained the handles (normal exit path),
        // this is empty and returns immediately. Otherwise join here.
        let handles: Vec<JoinHandle<()>> = self
            .workers
            .get_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        for h in handles {
            let _ = h.join();
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
        assert_eq!(
            s.submitted[Priority::Visible.index()],
            5,
            "5 Visible submits"
        );
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

    /// Regression: shutdown must never lose a wakeup. Creating and immediately
    /// dropping many pools races `request_shutdown` (via `Drop`) against workers
    /// parking in `cvar.wait` right after startup — the exact window a
    /// lost-wakeup would strand a worker, hanging `Drop`'s join forever. The
    /// whole loop runs on a side thread joined with a timeout, so a regression
    /// FAILS the test at the deadline instead of hanging the suite.
    #[test]
    fn shutdown_never_loses_wakeup_under_stress() {
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            for _ in 0..1000 {
                // `Drop` runs `request_shutdown` + joins all workers.
                drop(JobSystem::new(4));
            }
            let _ = tx.send(());
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(30)),
            Ok(()),
            "JobSystem shutdown deadlocked (lost wakeup in request_shutdown)"
        );
        worker.join().expect("stress thread joins cleanly");
    }
}
