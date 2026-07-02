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
        self.cancelled_before_dispatch
            .fetch_add(1, Ordering::Relaxed);
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
