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

#![allow(dead_code)] // Wired into the ingest permit gate in a follow-up task.

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

#[cfg(test)]
mod tests {
    use super::ConcurrencyController;

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
