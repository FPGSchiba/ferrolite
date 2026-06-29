//! Fixed-size worker pool driving the priority [`Queue`]. We use our own threads
//! (not rayon) so queued work can be reprioritized before it starts; rayon does
//! not expose priorities. Panics in jobs are caught so one bad job never downs
//! the pool.

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
        Self {
            shared,
            workers: handles,
        }
    }

    pub fn submit<F>(&self, priority: Priority, run: F) -> JobHandle
    where
        F: FnOnce(&CancelToken) + Send + 'static,
    {
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

    /// Drop a still-pending job from the queue (no-op if already running/done).
    pub fn cancel(&self, id: JobId) {
        self.shared.queue.lock().expect("queue mutex").cancel(id);
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
