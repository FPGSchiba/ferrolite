# Toast Notification System + Live Version Display — Design

**Date:** 2026-07-17
**Status:** Approved (brainstorm)
**Crates touched:** `ferrolite-app` only

## Problem

1. **Errors are invisible to the user.** I/O failures (e.g. the SD card being
   removed mid-ingest), decode failures, and catalog/export I/O errors are only
   `log::error!`-logged. The user sees nothing. There is no general-purpose way to
   communicate a problem.
2. **Only one transient message slot exists.** Today `AppState.warning:
   Option<String>` is the single user-message channel, rendered in red at the far
   right of the status bar. It has no severity levels and every writer overwrites
   the same slot.
3. **The version in the title bar is hardcoded.** `app.rs:3174` passes the literal
   `"v0.0.1"` into the title bar; it does not reflect `ferrolite-app/Cargo.toml`
   (currently `0.1.1`).

## Goals

- A general-purpose notification system with **three levels: Info, Warning,
  Error** that can be raised from both the UI thread and job threads.
- Present notifications as **non-blocking stacking toasts** anchored **top-right,
  below the title bar**.
- **Unify** the existing status-bar `warning` message path into this system (one
  notification path, not two).
- **Coalesce** bursts of the same notification (the SD-card scenario fans out
  across many in-flight jobs) into a single toast with a `×N` counter.
- Display the **live crate version** from `Cargo.toml` in the title bar.

## Non-Goals (YAGNI)

- No dismiss keybind. A rebindable `Action` would trigger the load-bearing
  keymap/Help/Settings discoverability requirements for no real benefit — dismiss
  is a mouse click on the toast's close button.
- No notification history / log panel.
- No per-toast action buttons (retry, undo, etc.).
- Ambient live status in the status bar (indexed/scanned counts, ingest and export
  progress bars, export activity indicator) is **not** a notification and is left
  untouched.

## Presentation (decided during brainstorm)

- **Style:** stacking toasts (non-blocking), not modal dialogs.
- **Placement:** top-right, below the custom title bar. Rationale: the bottom of
  the window carries busier controls/info, and top-down stacking reads naturally.
  Toasts render above panels (including the Develop right-side tool palette); they
  are transient, and errors are dismissible, so they never permanently block a
  control.
- **Levels & lifetime:**
  - `Info` — auto-dismiss after ~4s. (confirmations: "Preview cache purged")
  - `Warning` — auto-dismiss after ~6s. (soft failures: "Export queue not saved")
  - `Error` — **sticky**: never auto-expires, only manual dismiss. (I/O failures)
- **Coalescing:** a new `push` whose newest matching entry has the same
  `(level, message)` bumps that entry's `count` and resets its lifetime timer,
  instead of appending a duplicate. Renders as a `×N` badge.
- **Cap:** at most `MAX_VISIBLE` (5) toasts; pushing beyond the cap drops the
  oldest.

## Architecture

### New module: `ferrolite-app/src/notifications.rs` (pure, egui-free)

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level { Info, Warning, Error }

pub struct Notification {
    id: u64,
    level: Level,
    message: String,
    count: u32,       // coalesced duplicates → "×N"
    born: Instant,    // reset on coalesce; drives TTL pruning
}

pub struct Notifications {
    items: Vec<Notification>,
    next_id: u64,
}
```

Methods (all unit-testable, no egui dependency):

- `push(&mut self, level: Level, message: impl Into<String>)` — coalesce against
  the newest matching `(level, message)`: if found, `count += 1` and reset `born`;
  else append a new `Notification` with a fresh `id`. Enforce `MAX_VISIBLE` by
  dropping the oldest.
- `prune(&mut self, now: Instant)` — remove non-`Error` items whose age exceeds
  their level TTL. `Error` items are never pruned here.
- `dismiss(&mut self, id: u64)` — remove by id (manual close).
- Accessors for rendering (`iter()` newest-first, `is_empty()`).

Constants: `INFO_TTL = 4s`, `WARNING_TTL = 6s`, `MAX_VISIBLE = 5`.

### Rendering: `notifications::show(ctx: &egui::Context, n: &mut Notifications)`

- First `prune(Instant::now())`, then render remaining items in an egui `Area`
  anchored top-right below the title bar, stacked newest-on-top.
- Each toast card: level color (reuse `theme::SEMANTIC_RED` and siblings) + a
  **Phosphor icon from the `icons` module** + message text + `×N` badge (only when
  `count > 1`) + a close button. Clicking close calls `dismiss(id)`.
- While any non-error (TTL-bearing) toast is visible, call
  `ctx.request_repaint_after(remaining_ttl)` so auto-dismiss fires on time without
  a per-frame busy repaint.

### Icons (load-bearing rule)

Add semantic aliases to `ferrolite-app/src/icons.rs`, sourced from the Phosphor
catalog — e.g. `NOTIFY_INFO`, `NOTIFY_WARNING`, `NOTIFY_ERROR`, and a close glyph
(reuse an existing `×`/close alias if one already exists). **No raw emoji in Plex
text; no hand-drawn `Painter` icons.** Rendered in the icon font family.

### Threading: new `AppEvent::Notify { level, message }`

- Add the variant to `events.rs` (with a matching hand-written `Debug` arm).
- `AppState::apply` folds it by calling `self.notifications.push(level, message)`.
- Job threads raise toasts by sending `AppEvent::Notify` over the existing app
  event channel, then `ctx.request_repaint()` — honoring the "never block the UI
  thread; deliver over the event channel" rule.
- UI-thread code calls a new convenience method `AppState::notify(&mut self,
  level: Level, msg: impl Into<String>)` that pushes directly.

## Migration (unify the existing `warning` path)

- Replace `AppState.warning: Option<String>` with `AppState.notifications:
  Notifications`.
- Convert all ~25 `self.warning = Some(x)` sites (in `app.rs`, `state.rs`,
  `metadata.rs`, `develop/ops_persist.rs`, `library/panel.rs`,
  `library/image_context_menu.rs`, and the `events.rs` fold arms) to
  `self.notify(Level::_, x)`:
  - **Info** for confirmations ("Preview cache purged", "Added N to export queue").
  - **Warning** for soft/recoverable failures ("Export queue not saved (kept for
    this session)", "Could not load export queue").
- `MetadataResult` / `OpsSaved` fold arms: on failure push a toast; **drop** the
  old set/clear-on-success `warning` bookkeeping (toasts self-expire, so there is
  no stale message to clear). Keep the unrelated `dirty` / `ops_save_inflight` /
  `ops_save_failed` bookkeeping intact.
- Remove the status-bar `warning` render block (`status_bar.rs:97–104`). Leave the
  rest of the status bar unchanged.
- Route the user-facing **log-only I/O / decode failures** to `Level::Error`
  toasts (keeping the existing `log::error!` alongside): full-decode failed,
  preview decode failed, ingest walk / catalog read-write failures, and other
  SD-card-class errors. Internal/diagnostic-only logs (e.g. "display LUT bake
  failed") stay log-only unless they are user-actionable.

## Version fix

Replace the hardcoded `"v0.0.1"` argument at `app.rs:3174` with
`concat!("v", env!("CARGO_PKG_VERSION"))`. `CARGO_PKG_VERSION` resolves from
`ferrolite-app/Cargo.toml` at compile time, so the title bar shows the real
version (`v0.1.1`) and stays correct across future version bumps.

## Testing

Unit tests on `Notifications` (pure logic, target 80%+):

- `push` of a new `(level, message)` appends; a duplicate coalesces (count bumps,
  timer resets) instead of appending.
- Different level OR different message does **not** coalesce.
- `prune` removes `Info` after `INFO_TTL` and `Warning` after `WARNING_TTL`.
- `prune` never removes `Error` (sticky).
- `dismiss(id)` removes exactly that item.
- Exceeding `MAX_VISIBLE` drops the oldest.

Existing `events.rs` tests that assert on `warning` are updated to assert on the
`notifications` store instead (e.g. a failed `MetadataResult` produces an error
toast).

## Visual test plan (for the author, post-gate)

This feature is UI-reachable, so hands-on testing is required:

1. **Version** — launch the app; the title-bar version at top-right reads
   `v0.1.1` (matches `ferrolite-app/Cargo.toml`), not `v0.0.1`.
2. **Info toast** — trigger a confirmation (e.g. purge preview cache). A blue-ish
   info toast appears top-right and auto-dismisses after ~4s.
3. **Warning toast** — trigger a soft failure path (e.g. an export-queue persist
   failure) and confirm a yellow warning toast auto-dismisses after ~6s.
4. **Error toast (sticky + coalesce)** — start an ingest from an SD card / external
   volume, then pull/eject it mid-ingest. A red error toast appears and **stays**
   until closed; repeated identical failures collapse into one toast with a `×N`
   counter rather than flooding the stack.
5. **Dismiss** — the close button on any toast removes it; the stack re-flows.
6. **Placement** — in Develop, confirm toasts render above the right tool palette
   and do not permanently obscure a control (transient / dismissible).
```
