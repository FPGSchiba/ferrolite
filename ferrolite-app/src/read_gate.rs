//! Pure gradient/AIMD concurrency controller for adaptive ingest reads.
//!
//! Ports the Netflix/Vector "gradient" adaptive-concurrency algorithm: track the
//! minimum observed read latency (`rtt_min_us`, the no-load baseline) alongside a
//! short exponentially-weighted recent latency (`rtt_recent_us`). The ratio of the
//! two (`gradient`, clamped to `(0, 1]`) measures how far current latency has
//! drifted from baseline. `recompute` grows the concurrency limit toward `max`
//! when latency stays near the floor (gradient ~= 1) and shrinks it when latency
//! rises under contention (gradient < 1).
//!
//! `rtt_min_us` is windowed rather than all-time: it tracks the minimum latency
//! observed within the current [`WINDOW`]-sized batch of observations, and every
//! `WINDOW` observations that window rolls over and starts fresh. This mirrors
//! the periodic min-RTT reset in Vegas-style congestion control and matters
//! because an all-time running minimum can never recover from a single
//! transient fast outlier: once poisoned, `gradient = rtt_min / rtt_recent`
//! would stay pinned near zero forever even after latency normalizes. Rolling
//! the window lets a stale outlier age out after at most `2 * WINDOW`
//! observations.
//!
//! This module is pure logic: no threads, no synchronization, no I/O, no timing
//! source. Callers record latencies they measured elsewhere via [`observe`], then
//! call [`recompute`] to obtain the new limit. The synchronization wrapper that
//! turns this into an actual permit gate lives in a later task.
//!
//! [`observe`]: ConcurrencyController::observe
//! [`recompute`]: ConcurrencyController::recompute

use std::sync::{Condvar, Mutex};
use std::time::Instant;

/// Environment variable that, when set to an integer `N >= 1`, pins the ingest
/// read concurrency to exactly `N` and disables the adaptive controller. Any
/// other value (`0`, unset, or unparseable) leaves the gate adaptive.
const READ_CONCURRENCY_ENV: &str = "FERROLITE_INGEST_READ_CONCURRENCY";

/// Smoothing factor for the recent-latency EWMA. Chosen so the controller
/// reacts within roughly 10-20 samples of a latency regime change (verified in
/// `rising_latency_shrinks_limit`) without being so twitchy that a single
/// outlier sample swings the limit.
const RECENT_EWMA_ALPHA: f64 = 0.3;

/// Constant added to the gradient-scaled limit on each `recompute`, giving the
/// controller a small amount of headroom to probe for additional capacity even
/// once the gradient has settled at 1.0 (otherwise `limit * 1.0` is a fixed
/// point and the limit would never grow).
const QUEUE_ALLOWANCE: f64 = 1.0;

/// Number of observations in one `rtt_min` measurement window (Vegas-style
/// periodic reset). Every `WINDOW` observations, the current window's minimum
/// becomes the active baseline and a fresh window starts, so a single
/// transient fast reading ages out after at most `2 * WINDOW` observations
/// instead of poisoning `rtt_min` for the lifetime of the controller. Chosen
/// large enough that the existing tests (which use <= 50 observations, well
/// under one window) never roll and keep seeing the same running-min-within-
/// the-window behavior they were written against.
const WINDOW: usize = 100;

/// Point-in-time view of a [`ConcurrencyController`] for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControllerSnapshot {
    pub limit: usize,
    pub rtt_min_us: u64,
    pub rtt_recent_us: u64,
    pub gradient: f64,
}

/// Gradient-based adaptive concurrency limit.
///
/// Feed observed read latencies in with [`observe`](Self::observe), then call
/// [`recompute`](Self::recompute) to get the updated, clamped limit. The
/// controller never allocates and never blocks; it is plain arithmetic over a
/// handful of `u64`/`f64` fields.
#[derive(Debug, Clone)]
pub struct ConcurrencyController {
    min_limit: usize,
    max_limit: usize,
    limit: usize,
    /// Active `rtt_min` baseline: the minimum observed within the current
    /// measurement window so far. Reset to the next observation every
    /// `WINDOW` samples (see [`WINDOW`]) so a stale outlier ages out instead
    /// of poisoning the baseline forever.
    rtt_min_us: Option<u64>,
    /// Count of observations folded into the current window's `rtt_min_us`.
    window_count: usize,
    rtt_recent_us: Option<f64>,
    gradient: f64,
}

impl ConcurrencyController {
    /// Create a new controller. `start` is clamped into `[min_limit, max_limit]`
    /// on construction so callers cannot seed an out-of-bounds limit.
    pub fn new(min_limit: usize, max_limit: usize, start: usize) -> Self {
        let (min_limit, max_limit) = if min_limit <= max_limit {
            (min_limit, max_limit)
        } else {
            (max_limit, min_limit)
        };
        let start = start.clamp(min_limit, max_limit);
        Self {
            min_limit,
            max_limit,
            limit: start,
            rtt_min_us: None,
            window_count: 0,
            rtt_recent_us: None,
            gradient: 1.0,
        }
    }

    /// Record one observed read latency, in microseconds.
    ///
    /// Folds the latency into the current window's minimum (the no-load
    /// baseline) and the recent-latency EWMA. Every [`WINDOW`] observations,
    /// the window rolls over: the next observation starts a fresh window
    /// minimum rather than continuing to accumulate all-time, so an old
    /// outlier cannot permanently pin the baseline (see module docs). Does
    /// not itself change `limit` — call [`recompute`](Self::recompute) to
    /// fold the new observation into the concurrency limit.
    pub fn observe(&mut self, latency_us: u64) {
        if self.window_count >= WINDOW {
            // Window closed: start a fresh baseline from this observation
            // rather than folding it into the (now stale) previous window.
            self.rtt_min_us = Some(latency_us);
            self.window_count = 1;
        } else {
            self.rtt_min_us = Some(match self.rtt_min_us {
                Some(current_min) => current_min.min(latency_us),
                None => latency_us,
            });
            self.window_count += 1;
        }
        let latency = latency_us as f64;
        self.rtt_recent_us = Some(match self.rtt_recent_us {
            Some(recent) => RECENT_EWMA_ALPHA * latency + (1.0 - RECENT_EWMA_ALPHA) * recent,
            None => latency,
        });
    }

    /// Recompute the concurrency limit from the observations recorded so far,
    /// returning the new (already clamped) limit.
    ///
    /// If no observations have been made yet, this is a no-op that returns the
    /// current limit unchanged (there is nothing to divide by, and no signal to
    /// act on).
    pub fn recompute(&mut self) -> usize {
        let (rtt_min, rtt_recent) = match (self.rtt_min_us, self.rtt_recent_us) {
            (Some(min), Some(recent)) if recent > 0.0 => (min, recent),
            _ => return self.limit,
        };

        self.gradient = (rtt_min as f64 / rtt_recent).clamp(f64::MIN_POSITIVE, 1.0);

        let scaled = self.limit as f64 * self.gradient + QUEUE_ALLOWANCE;
        let new_limit = scaled.round();
        // `scaled` is finite and non-negative (limit, gradient, and the
        // allowance are all non-negative), so this cast is lossless within
        // usize range; clamp still guards against any residual edge case.
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let new_limit = new_limit as usize;
        self.limit = new_limit.clamp(self.min_limit, self.max_limit);
        self.limit
    }

    /// The current concurrency limit.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// A snapshot of the controller's internal state, for diagnostics.
    pub fn snapshot(&self) -> ControllerSnapshot {
        ControllerSnapshot {
            limit: self.limit,
            rtt_min_us: self.rtt_min_us.unwrap_or(0),
            rtt_recent_us: self.rtt_recent_us.unwrap_or(0.0).round() as u64,
            gradient: self.gradient,
        }
    }
}

/// Mutable state guarded by the gate's `Mutex`.
///
/// Holds the pure [`ConcurrencyController`] plus the number of permits handed
/// out and not yet dropped (`in_flight`). Both fields are only ever touched
/// while the gate's mutex is held.
struct State {
    controller: ConcurrencyController,
    in_flight: usize,
}

/// A dynamically-resizable read-permit gate for ingest reads.
///
/// Wraps the pure [`ConcurrencyController`] with the synchronization it needs
/// to act as an admission gate: a `Mutex<State>` protects the controller and
/// the in-flight count, and a `Condvar` lets [`acquire`](Self::acquire) block
/// until a slot opens up (either because a permit dropped or because the
/// controller grew the limit).
///
/// # Modes
///
/// - **Adaptive** (default): every dropped [`ReadPermit`] feeds its measured
///   latency into the controller and recomputes the limit, so the number of
///   concurrent reads tracks observed I/O contention.
/// - **Pinned**: when [`FERROLITE_INGEST_READ_CONCURRENCY`](READ_CONCURRENCY_ENV)
///   is set to `N >= 1` (or via [`with_pinned_limit`](Self::with_pinned_limit)),
///   the limit is fixed at `N` and `observe`/`recompute` are skipped entirely.
///   Permit accounting still enforces the fixed limit.
///
/// # Synchronization design
///
/// - `acquire` holds the mutex, then waits on the condvar *while*
///   `in_flight >= limit` in a `while` loop (never an `if`), so a spurious or
///   stale wakeup simply re-checks the predicate. `Condvar::wait` atomically
///   releases the mutex while parked and reacquires it on wake, so the
///   predicate is always evaluated under the lock.
/// - `ReadPermit::drop` takes the lock, updates the controller (adaptive mode
///   only), decrements `in_flight`, releases the lock, then `notify_all`. Both
///   events that can unblock a waiter — a freed slot and a grown limit —
///   happen together under that one lock/notify, so no waiter that should now
///   proceed can miss the wakeup.
pub struct AdaptiveReadGate {
    state: Mutex<State>,
    slot_freed: Condvar,
    /// Whether adaptation is disabled (fixed limit). Immutable after
    /// construction, so it can be read without taking the lock.
    pinned: bool,
}

impl AdaptiveReadGate {
    /// Create a gate sized for `max_limit` concurrent reads (typically the
    /// ingest worker count).
    ///
    /// Reads the [`FERROLITE_INGEST_READ_CONCURRENCY`](READ_CONCURRENCY_ENV)
    /// environment variable: if it parses to an integer `N >= 1`, the limit is
    /// pinned to `N` and adaptation is disabled. Otherwise (`0`, unset, or
    /// unparseable) the gate is adaptive, starting at
    /// `(max_limit / 2).max(2).min(max_limit)` and free to move within
    /// `[1, max_limit]`.
    pub fn new(max_limit: usize) -> Self {
        if let Some(n) = std::env::var(READ_CONCURRENCY_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n >= 1)
        {
            return Self::with_pinned_limit(n);
        }
        let start = (max_limit / 2).max(2).min(max_limit.max(1));
        let controller = ConcurrencyController::new(1, max_limit.max(1), start);
        Self {
            state: Mutex::new(State {
                controller,
                in_flight: 0,
            }),
            slot_freed: Condvar::new(),
            pinned: false,
        }
    }

    /// Create a gate whose limit is pinned to exactly `n` (adaptation
    /// disabled). Does **not** read any environment variable — this is the
    /// ctor used by tests and internally by [`new`](Self::new) for the pinned
    /// path. `n` is floored to `1` so the gate can always make progress.
    pub fn with_pinned_limit(n: usize) -> Self {
        let n = n.max(1);
        // A controller whose min == max == start can never change its limit,
        // so even if `recompute` were called it would be a no-op. We still gate
        // adaptation behind `pinned` to skip the timing/observe work entirely.
        let controller = ConcurrencyController::new(n, n, n);
        Self {
            state: Mutex::new(State {
                controller,
                in_flight: 0,
            }),
            slot_freed: Condvar::new(),
            pinned: true,
        }
    }

    /// Acquire a read permit, blocking until `in_flight < limit`.
    ///
    /// Increments the in-flight count and captures a start [`Instant`] used to
    /// measure this read's latency when the returned [`ReadPermit`] drops.
    pub fn acquire(&self) -> ReadPermit<'_> {
        let mut state = self.state.lock().expect("read gate mutex poisoned");
        // Re-check the predicate in a `while` loop: after any wakeup the limit
        // and in_flight may have changed, and spurious wakeups are permitted by
        // the platform. `wait` atomically releases the lock while parked and
        // reacquires it before returning, so the predicate below is always
        // evaluated while holding the mutex.
        while state.in_flight >= state.controller.limit() {
            state = self
                .slot_freed
                .wait(state)
                .expect("read gate mutex poisoned");
        }
        state.in_flight += 1;
        // Drop the guard before returning; the permit does its own locking on
        // drop.
        drop(state);
        ReadPermit {
            gate: self,
            start: Instant::now(),
        }
    }

    /// A snapshot of the underlying controller's state, for diagnostics.
    pub fn snapshot(&self) -> ControllerSnapshot {
        self.state
            .lock()
            .expect("read gate mutex poisoned")
            .controller
            .snapshot()
    }

    /// Whether the gate is running with a fixed (pinned) limit and adaptation
    /// disabled.
    pub fn is_pinned(&self) -> bool {
        self.pinned
    }
}

/// RAII read permit handed out by [`AdaptiveReadGate::acquire`].
///
/// While alive it counts toward the gate's `in_flight` total. On drop it
/// measures elapsed time since acquisition and (in adaptive mode) feeds that
/// latency into the controller before releasing its slot and waking any waiter.
pub struct ReadPermit<'a> {
    gate: &'a AdaptiveReadGate,
    start: Instant,
}

impl Drop for ReadPermit<'_> {
    fn drop(&mut self) {
        // Measure latency before taking the lock so contention on the mutex is
        // not counted as read latency.
        let elapsed_us = self.start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        {
            let mut state = self.gate.state.lock().expect("read gate mutex poisoned");
            if !self.gate.pinned {
                // Adaptive: fold this read's latency into the controller and
                // recompute the limit. The limit may grow here, which — together
                // with the freed slot below — is exactly what the notify covers.
                state.controller.observe(elapsed_us);
                state.controller.recompute();
            }
            debug_assert!(state.in_flight > 0, "permit drop without matching acquire");
            state.in_flight -= 1;
        } // release the lock before notifying
          // Wake every waiter: a slot just freed and (in adaptive mode) the limit
          // may have grown, so more than one parked thread might now proceed.
          // Each re-checks its predicate under the lock, so over-notifying is
          // safe and under-notifying is impossible.
        self.gate.slot_freed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::{AdaptiveReadGate, ConcurrencyController};

    #[test]
    fn gate_blocks_beyond_limit_then_releases() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};
        const THREADS: usize = 8;
        let gate = Arc::new(AdaptiveReadGate::with_pinned_limit(2)); // test ctor pinning limit=2
        let peak = Arc::new(AtomicUsize::new(0));
        let cur = Arc::new(AtomicUsize::new(0));
        // Barrier sized to THREADS: every thread finishes setup and calls
        // `wait()` before any of them calls `acquire()`, so all THREADS threads
        // race to acquire simultaneously. This makes contention deterministic
        // (guaranteed, not scheduling-dependent) — without it, threads could be
        // spawned/scheduled far enough apart that some finish their permit and
        // release before others even start, and the observed peak could come
        // out at 1, silently passing on a broken gate.
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut hs = vec![];
        for _ in 0..THREADS {
            let (g, p, c, b) = (gate.clone(), peak.clone(), cur.clone(), barrier.clone());
            hs.push(std::thread::spawn(move || {
                b.wait();
                let _permit = g.acquire();
                let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(20));
                c.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
        // With all 8 threads guaranteed to race for the gate at once and each
        // holding its permit for 20ms, the observed peak concurrency must hit
        // the pinned limit exactly (not just "never exceed it"): the barrier
        // rules out the under-contention case where peak comes in below 2.
        assert_eq!(
            peak.load(Ordering::SeqCst),
            2,
            "guaranteed contention must reach the pinned limit of 2 concurrent permits"
        );
    }

    #[test]
    fn env_override_pins_limit() {
        // with_pinned_limit models the FERROLITE_INGEST_READ_CONCURRENCY=N path.
        let gate = AdaptiveReadGate::with_pinned_limit(3);
        assert!(gate.is_pinned());
        assert_eq!(gate.snapshot().limit, 3);
    }

    #[test]
    fn flat_fast_latency_grows_limit_toward_max() {
        let mut c = ConcurrencyController::new(1, 12, 4);
        for _ in 0..50 {
            c.observe(1000);
            c.recompute();
        } // steady, at the floor
        assert!(
            c.limit() >= 8,
            "limit should climb when latency stays near rtt_min"
        );
    }

    #[test]
    fn rising_latency_shrinks_limit() {
        let mut c = ConcurrencyController::new(1, 12, 10);
        c.observe(1000);
        c.recompute(); // establishes rtt_min ~1ms
        for _ in 0..20 {
            c.observe(8000);
            c.recompute();
        } // 8x worse -> contention
        assert!(c.limit() < 10, "limit should shrink under rising latency");
    }

    #[test]
    fn limit_clamped_to_bounds() {
        let mut c = ConcurrencyController::new(2, 6, 6);
        for _ in 0..50 {
            c.observe(1_000_000);
            c.recompute();
        }
        assert!(c.limit() >= 2, "never below min");
        let mut c2 = ConcurrencyController::new(2, 6, 2);
        for _ in 0..50 {
            c2.observe(500);
            c2.recompute();
        }
        assert!(c2.limit() <= 6, "never above max");
    }

    /// Regression test for the rtt_min all-time-minimum poisoning bug: a
    /// single transient fast reading must not permanently pin the limit at
    /// `min_limit`. With `WINDOW = 100`, the 1us outlier is folded into the
    /// first window; once that window rolls over (after 100 observations)
    /// the baseline is reseeded from the steady 1000us stream and the
    /// gradient recovers to ~1, letting the limit climb again. We feed well
    /// over `2 * WINDOW` steady observations so the outlier has aged out of
    /// both the closing window and the fresh one, and assert the limit is
    /// strictly above `min_limit` (not knife-edged to a specific value,
    /// since the exact climb depends on `QUEUE_ALLOWANCE` rounding).
    #[test]
    fn transient_low_latency_outlier_does_not_permanently_poison_limit() {
        let mut c = ConcurrencyController::new(1, 12, 6);
        c.observe(1); // transient outlier: poisons rtt_min under the old all-time-min logic
        c.recompute();
        for _ in 0..250 {
            c.observe(1000);
            c.recompute();
        } // long steady stream, well past 2 * WINDOW, so the outlier ages out
        assert!(
            c.limit() >= 8,
            "limit should recover well above min_limit once the outlier ages out of the window, got {}",
            c.limit()
        );
        assert!(
            c.limit() > 1,
            "limit must not stay permanently pinned at min_limit"
        );
    }
}
