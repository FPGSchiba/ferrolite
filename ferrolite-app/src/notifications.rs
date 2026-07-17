//! General-purpose in-app notifications (toasts) with three severity levels.
//! Pure, egui-free store: coalesces duplicate `(level, message)` bursts and
//! auto-expires non-error toasts by TTL. Rendered by `notifications::show`
//! (see the same module's `render` section) and fed both from the UI thread
//! (`AppState::notify`) and job threads (`AppEvent::Notify`).

use std::time::{Duration, Instant};

/// Longest a non-error toast stays before `prune` drops it.
const INFO_TTL: Duration = Duration::from_secs(4);
const WARNING_TTL: Duration = Duration::from_secs(6);

/// Hard cap on simultaneously-held toasts; pushing beyond drops the oldest.
pub const MAX_VISIBLE: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    /// Auto-dismiss lifetime, or `None` for `Error` (sticky until dismissed).
    fn ttl(self) -> Option<Duration> {
        match self {
            Level::Info => Some(INFO_TTL),
            Level::Warning => Some(WARNING_TTL),
            Level::Error => None,
        }
    }
}

pub struct Notification {
    id: u64,
    level: Level,
    message: String,
    count: u32,
    born: Instant,
}

impl Notification {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn level(&self) -> Level {
        self.level
    }
    pub fn message(&self) -> &str {
        &self.message
    }
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
    pub fn prune(&mut self, now: Instant) {
        self.items.retain(|n| match n.level.ttl() {
            Some(ttl) => now.duration_since(n.born) < ttl,
            None => true,
        });
    }

    /// Remove the toast with `id` (manual close), if present.
    pub fn dismiss(&mut self, id: u64) {
        self.items.retain(|n| n.id != id);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Newest-first (render order: newest on top).
    pub fn iter_newest_first(&self) -> impl Iterator<Item = &Notification> {
        self.items.iter().rev()
    }

    /// Shortest remaining TTL across auto-dismiss toasts, for scheduling a
    /// repaint so expiry fires on time. `None` when only errors remain.
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
pub fn count_badge(count: u32) -> Option<String> {
    (count > 1).then(|| format!("×{count}"))
}

/// Accent color for a level's toast (border/icon). Reuses the theme palette.
pub fn level_color(level: Level) -> egui::Color32 {
    match level {
        Level::Info => crate::theme::SEMANTIC_BLUE,
        Level::Warning => crate::theme::SEMANTIC_AMBER,
        Level::Error => crate::theme::SEMANTIC_RED,
    }
}

/// Phosphor glyph (from `icons`) for a level.
pub fn level_icon(level: Level) -> &'static str {
    match level {
        Level::Info => crate::icons::INFO,
        Level::Warning => crate::icons::WARNING,
        Level::Error => crate::icons::NOTIFY_ERROR,
    }
}

/// Draw the toast stack top-right, below the title bar. Prunes expired toasts,
/// renders newest-on-top, and schedules a repaint so auto-dismiss fires on time.
/// A close button dismisses a toast; the click is applied after the layout pass.
pub fn show(ctx: &egui::Context, n: &mut Notifications) {
    let now = std::time::Instant::now();
    n.prune(now);
    if n.is_empty() {
        return;
    }

    let mut to_dismiss: Option<u64> = None;
    egui::Area::new(egui::Id::new("toast_stack"))
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 40.0))
        .interactable(true)
        .show(ctx, |ui| {
            ui.set_max_width(300.0);
            for toast in n.iter_newest_first() {
                let color = level_color(toast.level());
                egui::Frame::none()
                    .fill(crate::theme::BG_TOOLBAR)
                    .stroke(egui::Stroke::new(1.0, color))
                    .rounding(6.0)
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .outer_margin(egui::Margin {
                        bottom: 8.0,
                        ..Default::default()
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(level_icon(toast.level()))
                                    .color(color)
                                    .font(crate::icons::font(16.0)),
                            );
                            ui.add(
                                egui::Label::new(egui::RichText::new(toast.message()).size(12.0))
                                    .wrap(),
                            );
                            if let Some(badge) = count_badge(toast.count()) {
                                ui.label(
                                    egui::RichText::new(badge)
                                        .color(crate::theme::TEXT_DIM)
                                        .size(11.0),
                                );
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let close = ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(crate::icons::CLOSE)
                                                .color(crate::theme::TEXT_DIM)
                                                .font(crate::icons::font(13.0)),
                                        )
                                        .sense(egui::Sense::click()),
                                    );
                                    if close.on_hover_text("Dismiss").clicked() {
                                        to_dismiss = Some(toast.id());
                                    }
                                },
                            );
                        });
                    });
            }
        });

    if let Some(id) = to_dismiss {
        n.dismiss(id);
    }

    // Keep the frame clock ticking so TTL expiry fires without user input.
    if let Some(remaining) = n.next_expiry(std::time::Instant::now()) {
        ctx.request_repaint_after(remaining);
    }
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
    fn level_color_maps_each_level() {
        assert_eq!(level_color(Level::Info), crate::theme::SEMANTIC_BLUE);
        assert_eq!(level_color(Level::Warning), crate::theme::SEMANTIC_AMBER);
        assert_eq!(level_color(Level::Error), crate::theme::SEMANTIC_RED);
    }

    #[test]
    fn level_icon_maps_each_level() {
        assert_eq!(level_icon(Level::Info), crate::icons::INFO);
        assert_eq!(level_icon(Level::Warning), crate::icons::WARNING);
        assert_eq!(level_icon(Level::Error), crate::icons::NOTIFY_ERROR);
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
