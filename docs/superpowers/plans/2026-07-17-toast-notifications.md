# Toast Notification System + Live Version Display Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a general-purpose 3-level (Info/Warning/Error) non-blocking toast notification system, unify the existing single status-bar `warning` slot into it, and show the live crate version in the title bar.

**Architecture:** A pure, egui-free `Notifications` store (`ferrolite-app/src/notifications.rs`) holds toasts with coalescing and TTL logic. Job threads raise toasts via a new `AppEvent::Notify` over the existing app event channel; UI-thread code calls `AppState::notify`. A render function draws the stack as an egui `Area` anchored top-right. All existing `warning` writers migrate to the new path.

**Tech Stack:** Rust, egui/eframe 0.29, egui-phosphor 0.7 (icons), std `Instant`/`Duration`.

## Global Constraints

- **Crate scope:** all changes are in `ferrolite-app`. Scoped gate only: `cargo fmt -p ferrolite-app -- --check`, `cargo clippy -p ferrolite-app --all-targets -- -D warnings`, `cargo test -p ferrolite-app`.
- **Icons (load-bearing):** every glyph comes from `icons.rs` (Phosphor aliases), rendered in the icon font family via `icons::font(size)`. No raw emoji in Plex text; no hand-drawn `Painter` icons. Verified-available Phosphor `regular` names usable here: `INFO`, `WARNING`, `WARNING_OCTAGON`, `X`.
- **Threading (load-bearing):** never block the UI thread. Off-thread failures deliver over the event channel (`Sender<AppEvent>`), then the job calls `ctx.request_repaint()`.
- **Immutability / style:** run `cargo fmt` before every commit; clippy is `-D warnings`.
- **Commit prefix convention:** `feat:` / `refactor:` / `fix:` per repo git-workflow.

---

### Task 1: `Notifications` core store (pure logic)

**Files:**
- Create: `ferrolite-app/src/notifications.rs`
- Modify: `ferrolite-app/src/lib.rs:19` (add `pub mod notifications;` in alphabetical order — between `module` and `monitor_profile`, i.e. after line 14)

**Interfaces:**
- Produces:
  - `pub enum Level { Info, Warning, Error }` (derives `Clone, Copy, PartialEq, Eq, Debug`)
  - `pub struct Notification` with private fields; public accessors `id() -> u64`, `level() -> Level`, `message() -> &str`, `count() -> u32`
  - `pub struct Notifications` (derives `Default`) with:
    - `push(&mut self, level: Level, message: impl Into<String>, now: Instant)`
    - `prune(&mut self, now: Instant)`
    - `dismiss(&mut self, id: u64)`
    - `is_empty(&self) -> bool`
    - `iter_newest_first(&self) -> impl Iterator<Item = &Notification>`
    - `next_expiry(&self, now: Instant) -> Option<Duration>`
  - `pub const MAX_VISIBLE: usize = 5;`
  - `pub fn count_badge(count: u32) -> Option<String>`

- [ ] **Step 1: Write the failing tests**

Create `ferrolite-app/src/notifications.rs` with the test module only first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
        n.push(Level::Error, "SD card removed", base + Duration::from_secs(1));
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-app --lib notifications`
Expected: FAIL to compile (`Notifications`, `Level`, etc. not defined).

- [ ] **Step 3: Write the implementation**

Prepend above the test module in `ferrolite-app/src/notifications.rs`:

```rust
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
            .filter_map(|n| n.level.ttl().map(|ttl| ttl.saturating_sub(now.duration_since(n.born))))
            .min()
    }
}

/// The `×N` badge string, shown only when a toast has coalesced (`count > 1`).
pub fn count_badge(count: u32) -> Option<String> {
    (count > 1).then(|| format!("×{count}"))
}
```

Then add the module declaration in `ferrolite-app/src/lib.rs` after line 14 (`pub mod module;`):

```rust
pub mod notifications;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-app --lib notifications`
Expected: PASS (11 tests).

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
git add ferrolite-app/src/notifications.rs ferrolite-app/src/lib.rs
git commit -m "feat: add Notifications core store (coalescing + TTL toasts)"
```

---

### Task 2: `AppEvent::Notify` + `AppState.notifications` + `notify` helper

**Files:**
- Modify: `ferrolite-app/src/events.rs` (add `Notify` variant ~line 168, its `Debug` arm ~line 298, and an `apply` fold arm ~line 425)
- Modify: `ferrolite-app/src/state.rs` (add `notifications` field near the existing `warning` field ~line 173; init in the two constructors at ~line 308 and ~line 898; add `notify` method)

**Interfaces:**
- Consumes: `Notifications`, `Level` from Task 1.
- Produces:
  - `AppEvent::Notify { level: crate::notifications::Level, message: String }`
  - `AppState.notifications: crate::notifications::Notifications` (public)
  - `AppState::notify(&mut self, level: crate::notifications::Level, message: impl Into<String>)`

Note: `AppState.warning` stays in place this task (removed in Task 4) so the tree keeps compiling.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `ferrolite-app/src/events.rs`:

```rust
    #[test]
    fn notify_event_pushes_into_store() {
        use crate::notifications::Level;
        let mut s = AppState::for_test();
        s.apply(AppEvent::Notify {
            level: Level::Error,
            message: "SD card removed".into(),
        });
        assert_eq!(s.notifications.iter_newest_first().count(), 1);
        let n = s.notifications.iter_newest_first().next().unwrap();
        assert_eq!(n.level(), Level::Error);
        assert_eq!(n.message(), "SD card removed");
    }

    #[test]
    fn notify_helper_pushes_into_store() {
        use crate::notifications::Level;
        let mut s = AppState::for_test();
        s.notify(Level::Info, "12 photos indexed");
        assert_eq!(s.notifications.iter_newest_first().count(), 1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-app --lib events::tests::notify`
Expected: FAIL to compile (`AppEvent::Notify`, `s.notifications`, `s.notify` unknown).

- [ ] **Step 3: Implement**

In `ferrolite-app/src/events.rs`, add the variant at the end of the `AppEvent` enum (after the `MetaLoaded { .. }` variant, before the closing `}`):

```rust
    /// A general-purpose user notification (toast). Raised from job threads over
    /// the event channel; folded by `apply` into `AppState.notifications`.
    Notify {
        level: crate::notifications::Level,
        message: String,
    },
```

Add its `Debug` arm inside `impl std::fmt::Debug for AppEvent` (after the `MetaLoaded` arm):

```rust
            AppEvent::Notify { level, message } => f
                .debug_struct("Notify")
                .field("level", level)
                .field("message", message)
                .finish(),
```

Add the fold arm in `AppState::apply`, after the `AppEvent::MetaLoaded { .. } => None,` arm:

```rust
            AppEvent::Notify { level, message } => {
                self.notifications.push(level, message, std::time::Instant::now());
                None
            }
```

In `ferrolite-app/src/state.rs`, add the field right after `pub warning: Option<String>,`:

```rust
    /// General-purpose in-app notifications (toasts). See `notifications` module.
    pub notifications: crate::notifications::Notifications,
```

Initialize it in BOTH constructors (the real one ~line 308 and `for_test` ~line 898) wherever `warning: None,` appears — add alongside:

```rust
            notifications: crate::notifications::Notifications::default(),
```

Add the helper method inside `impl AppState` (near the other small helpers):

```rust
    /// Push a toast from UI-thread code. Job threads instead send
    /// `AppEvent::Notify` over the event channel.
    pub fn notify(
        &mut self,
        level: crate::notifications::Level,
        message: impl Into<String>,
    ) {
        self.notifications
            .push(level, message, std::time::Instant::now());
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-app --lib events`
Expected: PASS (including the two new tests; existing `warning` tests still pass — `warning` is untouched this task).

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
git add ferrolite-app/src/events.rs ferrolite-app/src/state.rs
git commit -m "feat: add AppEvent::Notify, notifications field, and notify() helper"
```

---

### Task 3: Rendering — theme colors, icons, `notifications::show`, wire into frame

**Files:**
- Modify: `ferrolite-app/src/theme.rs` (add two color constants after `SEMANTIC_GREEN` ~line 20)
- Modify: `ferrolite-app/src/icons.rs` (add `NOTIFY_ERROR` and `CLOSE` aliases; `INFO`/`WARNING` already exist at lines 41–42)
- Modify: `ferrolite-app/src/notifications.rs` (add `level_color`, `level_icon`, and `show`; add tests for the pure mappers)
- Modify: `ferrolite-app/src/app.rs:3373` (call `show` after the status bar panel)

**Interfaces:**
- Consumes: `Notifications`, `Level`, `count_badge`, `MAX_VISIBLE` (Task 1); `AppState.notifications` (Task 2).
- Produces:
  - `pub fn level_color(level: Level) -> egui::Color32`
  - `pub fn level_icon(level: Level) -> &'static str`
  - `pub fn show(ctx: &egui::Context, n: &mut Notifications)`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `ferrolite-app/src/notifications.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-app --lib notifications::tests::level_`
Expected: FAIL to compile (`level_color`, `level_icon`, `SEMANTIC_BLUE`, `SEMANTIC_AMBER`, `NOTIFY_ERROR` undefined).

- [ ] **Step 3: Implement the constants and mappers**

In `ferrolite-app/src/theme.rs`, after `pub const SEMANTIC_GREEN: ...` (line 20) add:

```rust
pub const SEMANTIC_AMBER: Color32 = Color32::from_rgb(0xd6, 0xa8, 0x4c); // warning toasts
pub const SEMANTIC_BLUE: Color32 = Color32::from_rgb(0x5a, 0x9d, 0xd6); // info toasts
```

In `ferrolite-app/src/icons.rs`, after the existing `pub const INFO: &str = p::INFO;` (line 42) add:

```rust
pub const NOTIFY_ERROR: &str = p::WARNING_OCTAGON; // error toast glyph
pub const CLOSE: &str = p::X; // toast dismiss button
```

In `ferrolite-app/src/notifications.rs`, add after `count_badge` (still above the test module):

```rust
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
```

- [ ] **Step 4: Run mapper tests to verify they pass**

Run: `cargo test -p ferrolite-app --lib notifications`
Expected: PASS.

- [ ] **Step 5: Implement `show` (rendering)**

Add to `ferrolite-app/src/notifications.rs` after `level_icon` (rendering has no unit test — it is covered by the author's visual test):

```rust
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
                                egui::Label::new(
                                    egui::RichText::new(toast.message()).size(12.0),
                                )
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
```

- [ ] **Step 6: Wire into the frame**

In `ferrolite-app/src/app.rs`, immediately after the status bar panel block that ends at line 3373 (the `});` closing `.show(ctx, |ui| { crate::status_bar::show(ui, &self.state); });`), add:

```rust
        crate::notifications::show(ctx, &mut self.state.notifications);
```

- [ ] **Step 7: Build + run scoped gate**

Run: `cargo build -p ferrolite-app` then `cargo test -p ferrolite-app`
Expected: builds clean; all tests PASS.

- [ ] **Step 8: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
git add ferrolite-app/src/theme.rs ferrolite-app/src/icons.rs ferrolite-app/src/notifications.rs ferrolite-app/src/app.rs
git commit -m "feat: render toast stack top-right and wire into the frame"
```

---

### Task 4: Migrate the `warning` slot into toasts; remove the field

**Files:**
- Modify: `ferrolite-app/src/events.rs` (fold arms `MetadataResult` ~line 371, `OpsSaved` ~line 384; rewrite the `warning`-based tests ~lines 582–643)
- Modify: `ferrolite-app/src/status_bar.rs:97-104` (remove the warning render block)
- Modify: `ferrolite-app/src/state.rs` (remove `pub warning` field + its two initializers; migrate lines 739, 760)
- Modify: `ferrolite-app/src/app.rs` (migrate lines 574, 580, 605, 636, 693, 704, 718, 3031, 3194, 3210, 3596)
- Modify: `ferrolite-app/src/library/panel.rs:265,386`
- Modify: `ferrolite-app/src/library/image_context_menu.rs:161,164,173`
- Modify: `ferrolite-app/src/metadata.rs:60,67` and `ferrolite-app/src/develop/ops_persist.rs:32,39` (these build the `warning` string carried by `MetadataResult`/`OpsSaved` — leave those strings as-is; the fold arm decides the level)

**Interfaces:**
- Consumes: `AppState::notify`, `Level` (Task 2). Note `AppEvent::MetadataResult { ok, warning }` and `AppEvent::OpsSaved { ok, warning }` keep their existing signatures — only the fold arms change.

**Level assignment (apply exactly):**

| Site | New call | Level |
|------|----------|-------|
| `state.rs:739` "Could not load export queue." | `self.notify` | Warning |
| `state.rs:760` "Export queue not saved (kept for this session)." | `self.notify` | Warning |
| `app.rs:574/580` "Image still loading; cannot export yet." | `self.state.notify` | Warning |
| `app.rs:605/704` "No GPU render state; cannot export." | `self.state.notify` | Warning |
| `app.rs:636` "Choose a destination folder first." | `self.state.notify` | Warning |
| `app.rs:693/718` export-with-skips summary (the `if skipped > 0 {..}` string) | `self.state.notify` | Info |
| `app.rs:3031` `message.clone()` (ExportFinished) | `self.state.notify` | Info if `ok` else Error |
| `app.rs:3194` "Added to export queue." | `self.state.notify` | Info |
| `app.rs:3210` "Preview cache purged." | `self.state.notify` | Info |
| `app.rs:3596` queued/unqueued toggle string | `self.state.notify` | Info |
| `library/panel.rs:265` "Added N image(s)…" | `state.notify` | Info |
| `library/panel.rs:386` "Tagged N image(s)…" | `state.notify` | Info |
| `library/image_context_menu.rs:161/164/173` export-queue strings | `state.notify` | Info |
| `MetadataResult` fold (metadata write/sidecar failure) | fold arm | Error |
| `OpsSaved` fold (edit save failure) | fold arm | Error |

- [ ] **Step 1: Rewrite the `events.rs` fold arms**

Replace the `AppEvent::MetadataResult { ok, warning }` arm body (currently sets `self.warning`) with:

```rust
            AppEvent::MetadataResult { ok, warning } => {
                if !ok {
                    self.dirty = true;
                }
                if let Some(w) = warning {
                    self.notify(crate::notifications::Level::Error, w);
                }
                None
            }
```

Replace the `AppEvent::OpsSaved { ok, warning }` arm body with:

```rust
            AppEvent::OpsSaved { ok, warning } => {
                self.ops_save_inflight = self.ops_save_inflight.saturating_sub(1);
                self.ops_save_failed = !ok;
                if let Some(w) = warning {
                    self.notify(crate::notifications::Level::Error, w);
                }
                None
            }
```

- [ ] **Step 2: Update the `events.rs` tests**

Replace the three `warning`-based tests (`metadata_result_clears_warning_on_clean_success`, `metadata_result_preserves_warning_on_failure`, `metadata_result_sets_warning_when_provided`) and the `ops_saved_ok_decrements_inflight_and_clears_failed` warning assertion with notification-based equivalents:

```rust
    #[test]
    fn metadata_result_failure_pushes_error_toast() {
        use crate::notifications::Level;
        let mut s = AppState::for_test();
        s.apply(AppEvent::MetadataResult {
            ok: false,
            warning: Some("catalog write failed".into()),
        });
        assert!(s.dirty);
        let n = s.notifications.iter_newest_first().next().unwrap();
        assert_eq!(n.level(), Level::Error);
        assert_eq!(n.message(), "catalog write failed");
    }

    #[test]
    fn metadata_result_clean_success_pushes_nothing() {
        let mut s = AppState::for_test();
        s.apply(AppEvent::MetadataResult {
            ok: true,
            warning: None,
        });
        assert!(s.notifications.is_empty());
    }

    #[test]
    fn ops_saved_ok_decrements_inflight_and_clears_failed() {
        let mut s = AppState::for_test();
        s.ops_save_inflight = 1;
        s.ops_save_failed = true;
        s.apply(AppEvent::OpsSaved {
            ok: true,
            warning: None,
        });
        assert_eq!(s.ops_save_inflight, 0);
        assert!(!s.ops_save_failed);
        assert!(s.notifications.is_empty());
    }

    #[test]
    fn ops_saved_failure_pushes_error_toast() {
        use crate::notifications::Level;
        let mut s = AppState::for_test();
        s.ops_save_inflight = 1;
        s.apply(AppEvent::OpsSaved {
            ok: false,
            warning: Some("sidecar write failed".into()),
        });
        assert!(s.ops_save_failed);
        assert_eq!(
            s.notifications.iter_newest_first().next().unwrap().level(),
            Level::Error
        );
    }
```

Keep `ops_saved_ok_saturates_at_zero_when_already_zero` as-is (it does not touch `warning`).

- [ ] **Step 3: Remove the status-bar warning block**

In `ferrolite-app/src/status_bar.rs`, delete lines 97–104 (the `if let Some(w) = &state.warning { ... }` block). Leave everything else in `show` intact.

- [ ] **Step 4: Migrate the UI-thread call sites**

Apply the table above. The mechanical transform is `X.warning = Some(<expr>);` → `X.notify(<Level>, <expr>);` where `X` is `self.state`, `self`, or `state` as at the site. Two non-mechanical sites:

`app.rs:693` / `app.rs:718` (the `if skipped > 0` form) — convert
```rust
self.state.warning = if skipped > 0 { A } else { B };
```
to
```rust
self.state.notify(crate::notifications::Level::Info, if skipped > 0 { A } else { B });
```
(keep the exact `A`/`B` string expressions already there).

`app.rs:3031` — the ExportFinished handler has an `ok` in scope; convert
```rust
self.state.warning = Some(message.clone());
```
to
```rust
self.state.notify(
    if ok { crate::notifications::Level::Info } else { crate::notifications::Level::Error },
    message.clone(),
);
```

For `library/panel.rs`, `library/image_context_menu.rs`, and `state.rs`, use the same transform with the level from the table. Import path is always the fully-qualified `crate::notifications::Level::_` (no `use` needed).

- [ ] **Step 5: Remove the `warning` field**

In `ferrolite-app/src/state.rs`, delete `pub warning: Option<String>,` (~line 173) and its two `warning: None,` initializers (~lines 308, 898). `metadata.rs` and `develop/ops_persist.rs` are untouched (they set a local `warning` variable sent in the event, not `AppState.warning`).

- [ ] **Step 6: Build to catch any missed site**

Run: `cargo build -p ferrolite-app`
Expected: builds clean. If the compiler flags a remaining `.warning` reference, migrate it per the table (or Warning if not listed).

- [ ] **Step 7: Run tests**

Run: `cargo test -p ferrolite-app`
Expected: PASS (rewritten events tests included).

- [ ] **Step 8: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
git add -A ferrolite-app/src
git commit -m "refactor: unify status-bar warning slot into the toast system"
```

---

### Task 5: Route log-only I/O / decode failures to Error toasts

**Files:**
- Modify: `ferrolite-app/src/viewer/load.rs` (the two decode-failure sites that currently only `log::error!` and send `FullFailed` — around lines 64/133 area; the failure branches)
- Modify: `ferrolite-app/src/state.rs` (`remove_folder` failure ~line where `remove_folder failed` is logged)
- Modify: `ferrolite-app/src/ingest.rs` (ingest walk / catalog failure paths that currently only log)

**Interfaces:**
- Consumes: `AppEvent::Notify`, `Level` (Task 2). Job threads already hold `tx: &Sender<AppEvent>`.

Route only **user-facing** failures (the SD-card class). Leave purely-diagnostic logs (e.g. "display LUT bake failed", "preview cache purge failed") log-only.

- [ ] **Step 1: Add a toast at each user-facing failure branch**

At a job-thread failure that has `tx` in scope and currently does `log::error!(...)`, add a `Notify` send alongside the existing log (do not remove the log). Example — the full-decode failure path in `viewer/load.rs`:

```rust
                log::error!("ferrolite: full decode failed for #{image_id}: {e}");
                let _ = tx.send(crate::events::AppEvent::Notify {
                    level: crate::notifications::Level::Error,
                    message: format!("Could not load image (check the card/drive is connected): {e}"),
                });
                let _ = tx.send(AppEvent::FullFailed { image_id });
```

Apply the same pattern (keep the existing `log::error!`, add a `Notify` send with a user-readable message) to:
- `viewer/load.rs` preview-decode failure branch — message: `format!("Could not load preview: {e}")`.
- `ingest.rs` directory-walk / catalog failure branch(es) that currently only log — message: `format!("Import failed (check the card/drive is connected): {e}")`.

For a UI-thread failure with no `tx` (e.g. `state.rs` `remove_folder` failure ~the `remove_folder failed` log), add `self.notify(crate::notifications::Level::Error, format!("Could not remove folder: {e}"));` next to the existing log.

- [ ] **Step 2: Build + test**

Run: `cargo build -p ferrolite-app && cargo test -p ferrolite-app`
Expected: builds clean; tests PASS.

- [ ] **Step 3: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
git add -A ferrolite-app/src
git commit -m "feat: surface I/O and decode failures as error toasts"
```

---

### Task 6: Live version in the title bar

**Files:**
- Modify: `ferrolite-app/src/app.rs:3174`

**Interfaces:**
- Consumes: nothing new. `title_bar` already takes `version: &str`.

- [ ] **Step 1: Replace the hardcoded version**

In `ferrolite-app/src/app.rs`, change the `title_bar` argument at line 3174 from:

```rust
                    "v0.0.1",
```

to:

```rust
                    concat!("v", env!("CARGO_PKG_VERSION")),
```

`CARGO_PKG_VERSION` is set by Cargo from `ferrolite-app/Cargo.toml` at compile time (currently `0.1.1`), so the title bar reads `v0.1.1` and tracks future bumps automatically.

- [ ] **Step 2: Build**

Run: `cargo build -p ferrolite-app`
Expected: builds clean.

- [ ] **Step 3: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
git add ferrolite-app/src/app.rs
git commit -m "fix: show live crate version in the title bar"
```

---

## Post-implementation (coordinator)

After all tasks: run the **repo gate** (`rustup update stable` first), then hand the author the visual test plan from the spec (`docs/superpowers/specs/2026-07-17-toast-notifications-design.md`, "Visual test plan" section) and hold for hands-on results before finishing the branch.

## Self-Review

- **Spec coverage:** 3 levels (Task 1) ✓; stacking toasts top-right (Task 3) ✓; unify `warning` (Task 4) ✓; coalescing (Task 1) ✓; sticky errors (Task 1) ✓; TTLs 4s/6s (Task 1) ✓; `AppEvent::Notify` threading (Task 2) ✓; icons via `icons.rs` (Task 3) ✓; I/O errors surfaced (Task 5) ✓; live version (Task 6) ✓; unit tests (Tasks 1–4) ✓; no dismiss keybind (respected — click-only close in Task 3) ✓.
- **Type consistency:** `Notifications`/`Level`/`Notification` accessors, `push(level, msg, now)`, `notify(level, msg)`, `AppEvent::Notify { level, message }`, `level_color`/`level_icon`/`count_badge`/`next_expiry` used identically across tasks ✓.
- **Placeholder scan:** none — every step has concrete code/paths.
