//! General-purpose in-app notifications (toasts) with three severity levels.
//! Pure, egui-free store: coalesces duplicate `(level, message)` bursts and
//! auto-expires non-error toasts by TTL. Rendered by `notifications::show`
//! (see the same module's `render` section) and fed both from the UI thread
//! (`AppState::notify`) and job threads (`AppEvent::Notify`).

use std::time::{Duration, Instant};

/// Longest a non-error toast stays before `prune` drops it.
// Read only via `Level::ttl`, which is itself unreached in the bin until
// `notifications::show` is wired in (Task 3) — the lib copy is `pub`-reachable
// so dead_code doesn't fire there. Remove once Task 3 lands.
#[allow(dead_code)]
const INFO_TTL: Duration = Duration::from_secs(4);
#[allow(dead_code)]
const WARNING_TTL: Duration = Duration::from_secs(6);

/// Hard cap on simultaneously-held toasts; pushing beyond drops the oldest.
pub const MAX_VISIBLE: usize = 5;

// No bin call site constructs a `Level` yet — that starts with `AppState::notify`
// callers (Task 4) and job-thread `AppEvent::Notify` sends (Task 5). Remove once
// either lands.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    /// Auto-dismiss lifetime, or `None` for `Error` (sticky until dismissed).
    // Unreached in the bin until `notifications::show` calls `prune`/`next_expiry`
    // (Task 3). Remove once Task 3 lands.
    #[allow(dead_code)]
    fn ttl(self) -> Option<Duration> {
        match self {
            Level::Info => Some(INFO_TTL),
            Level::Warning => Some(WARNING_TTL),
            Level::Error => None,
        }
    }
}

pub struct Notification {
    // Unread in the bin until `notifications::show` calls `id()` (Task 3).
    // Remove once Task 3 lands.
    #[allow(dead_code)]
    id: u64,
    level: Level,
    message: String,
    count: u32,
    born: Instant,
}

// These accessors are unreached in the bin until `notifications::show` calls
// them (Task 3) — the lib copy is `pub`-reachable so dead_code doesn't fire
// there. Remove the annotations once Task 3 lands.
impl Notification {
    #[allow(dead_code)]
    pub fn id(&self) -> u64 {
        self.id
    }
    #[allow(dead_code)]
    pub fn level(&self) -> Level {
        self.level
    }
    #[allow(dead_code)]
    pub fn message(&self) -> &str {
        &self.message
    }
    #[allow(dead_code)]
    pub fn count(&self) -> u32 {
        self.count
    }
}

#[derive(Default)]
pub struct Notifications {
    items: Vec<Notification>,
    next_id: u64,
}

impl Notifications {
    /// Add a toast. If the newest entry with the same `(level, message)` still
    /// exists, bump its count and reset its timer instead of appending a
    /// duplicate (coalescing). Enforces `MAX_VISIBLE` by dropping the oldest.
    pub fn push(&mut self, level: Level, message: impl Into<String>, now: Instant) {
        let message = message.into();
        if let Some(existing) = self
            .items
            .iter_mut()
            .find(|n| n.level == level && n.message == message)
        {
            existing.count += 1;
            existing.born = now;
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Notification {
            id,
            level,
            message,
            count: 1,
            born: now,
        });
        if self.items.len() > MAX_VISIBLE {
            self.items.remove(0);
        }
    }

    /// Drop auto-dismiss toasts whose TTL has elapsed. Errors are never pruned.
    // Unreached in the bin until `notifications::show` calls it (Task 3).
    // Remove once Task 3 lands.
    #[allow(dead_code)]
    pub fn prune(&mut self, now: Instant) {
        self.items.retain(|n| match n.level.ttl() {
            Some(ttl) => now.duration_since(n.born) < ttl,
            None => true,
        });
    }

    /// Remove the toast with `id` (manual close), if present.
    #[allow(dead_code)]
    pub fn dismiss(&mut self, id: u64) {
        self.items.retain(|n| n.id != id);
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Newest-first (render order: newest on top).
    #[allow(dead_code)]
    pub fn iter_newest_first(&self) -> impl Iterator<Item = &Notification> {
        self.items.iter().rev()
    }

    /// Shortest remaining TTL across auto-dismiss toasts, for scheduling a
    /// repaint so expiry fires on time. `None` when only errors remain.
    #[allow(dead_code)]
    pub fn next_expiry(&self, now: Instant) -> Option<Duration> {
        self.items
            .iter()
            .filter_map(|n| {
                n.level
                    .ttl()
                    .map(|ttl| ttl.saturating_sub(now.duration_since(n.born)))
            })
            .min()
    }
}

/// The `×N` badge string, shown only when a toast has coalesced (`count > 1`).
// Unreached in the bin until `notifications::show` calls it (Task 3). Remove
// once Task 3 lands.
#[allow(dead_code)]
pub fn count_badge(count: u32) -> Option<String> {
    (count > 1).then(|| format!("×{count}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn push_appends_new_notification() {
        let mut n = Notifications::default();
        n.push(Level::Info, "hello", t0());
        assert_eq!(n.iter_newest_first().count(), 1);
        let first = n.iter_newest_first().next().unwrap();
        assert_eq!(first.message(), "hello");
        assert_eq!(first.count(), 1);
        assert_eq!(first.level(), Level::Info);
    }

    #[test]
    fn duplicate_coalesces_bumping_count_and_resetting_timer() {
        let base = t0();
        let mut n = Notifications::default();
        n.push(Level::Error, "SD card removed", base);
        // A later identical push must coalesce, not append.
        n.push(
            Level::Error,
            "SD card removed",
            base + Duration::from_secs(1),
        );
        assert_eq!(n.iter_newest_first().count(), 1);
        let only = n.iter_newest_first().next().unwrap();
        assert_eq!(only.count(), 2);
    }

    #[test]
    fn different_level_does_not_coalesce() {
        let mut n = Notifications::default();
        n.push(Level::Info, "same text", t0());
        n.push(Level::Warning, "same text", t0());
        assert_eq!(n.iter_newest_first().count(), 2);
    }

    #[test]
    fn different_message_does_not_coalesce() {
        let mut n = Notifications::default();
        n.push(Level::Info, "a", t0());
        n.push(Level::Info, "b", t0());
        assert_eq!(n.iter_newest_first().count(), 2);
    }

    #[test]
    fn prune_removes_info_after_ttl_but_keeps_before() {
        let base = t0();
        let mut n = Notifications::default();
        n.push(Level::Info, "x", base);
        n.prune(base + Duration::from_secs(3)); // before 4s TTL
        assert_eq!(n.iter_newest_first().count(), 1);
        n.prune(base + Duration::from_secs(5)); // after 4s TTL
        assert!(n.is_empty());
    }

    #[test]
    fn prune_removes_warning_after_ttl() {
        let base = t0();
        let mut n = Notifications::default();
        n.push(Level::Warning, "w", base);
        n.prune(base + Duration::from_secs(5)); // before 6s
        assert_eq!(n.iter_newest_first().count(), 1);
        n.prune(base + Duration::from_secs(7)); // after 6s
        assert!(n.is_empty());
    }

    #[test]
    fn prune_never_removes_error() {
        let base = t0();
        let mut n = Notifications::default();
        n.push(Level::Error, "e", base);
        n.prune(base + Duration::from_secs(3600));
        assert_eq!(n.iter_newest_first().count(), 1);
    }

    #[test]
    fn dismiss_removes_by_id() {
        let mut n = Notifications::default();
        n.push(Level::Error, "a", t0());
        n.push(Level::Error, "b", t0());
        let id = n.iter_newest_first().last().unwrap().id(); // oldest = "a"
        n.dismiss(id);
        assert_eq!(n.iter_newest_first().count(), 1);
        assert_eq!(n.iter_newest_first().next().unwrap().message(), "b");
    }

    #[test]
    fn exceeding_max_visible_drops_oldest() {
        let mut n = Notifications::default();
        for i in 0..(MAX_VISIBLE + 2) {
            n.push(Level::Error, format!("msg {i}"), t0());
        }
        assert_eq!(n.iter_newest_first().count(), MAX_VISIBLE);
        // Newest is the last pushed; oldest ("msg 0","msg 1") were dropped.
        assert_eq!(
            n.iter_newest_first().next().unwrap().message(),
            format!("msg {}", MAX_VISIBLE + 1)
        );
    }

    #[test]
    fn count_badge_only_when_greater_than_one() {
        assert_eq!(count_badge(1), None);
        assert_eq!(count_badge(3), Some("×3".to_string()));
    }

    #[test]
    fn next_expiry_is_shortest_remaining_and_none_for_only_errors() {
        let base = t0();
        let mut n = Notifications::default();
        n.push(Level::Error, "e", base);
        assert_eq!(n.next_expiry(base), None);
        n.push(Level::Info, "i", base);
        // Info TTL 4s, elapsed 1s → 3s remaining.
        assert_eq!(
            n.next_expiry(base + Duration::from_secs(1)),
            Some(Duration::from_secs(3))
        );
    }
}
