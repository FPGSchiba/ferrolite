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
    #[allow(dead_code)]
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
