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
