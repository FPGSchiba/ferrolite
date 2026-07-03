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
    rtt_min_us: Option<u64>,
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
            rtt_recent_us: None,
            gradient: 1.0,
        }
    }

    /// Record one observed read latency, in microseconds.
    ///
    /// Updates the running minimum (the no-load baseline) and the recent-latency
    /// EWMA. Does not itself change `limit` — call [`recompute`](Self::recompute)
    /// to fold the new observation into the concurrency limit.
    pub fn observe(&mut self, latency_us: u64) {
        self.rtt_min_us = Some(match self.rtt_min_us {
            Some(current_min) => current_min.min(latency_us),
            None => latency_us,
        });
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
}
