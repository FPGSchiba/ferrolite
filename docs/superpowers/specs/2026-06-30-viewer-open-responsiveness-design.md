# ferrolite — Viewer open responsiveness (design)

> **Status:** Design — approved by user; pending writing-plans.
> **Date:** 2026-06-30
> **Branch:** `feat/viewer-and-vt-ladder` (UX fix before finishing the branch).

---

## 1. Problem

Double-clicking an image to open the Develop viewer "does not directly switch — it is stuck for a
moment," especially on larger images.

**Root cause.** `FerroliteApp::open_record` switches `module = Develop` and sets the viewer at the
**end** of the update frame (after the central-panel closure has already drawn the grid), but it does
**not** request a repaint. egui therefore finishes that frame still showing the grid and goes idle;
the viewer only appears when the next input event happens to wake egui — perceived as a stall. A
secondary gap: once switched, the canvas is plain black with no feedback while the (larger) embedded
preview JPEG decodes on the job thread.

## 2. Goal

Opening an image switches to the Develop viewer **immediately**, and the brief decode wait reads as
the app **working** (a loading indicator) rather than frozen.

## 3. Design

### 3.1 Immediate repaint on open
`FerroliteApp::open_record` takes the `egui::Context` (it already takes `&mut eframe::Frame`) and
calls `ctx.request_repaint()` after switching to Develop. This guarantees the next frame runs —
which draws the viewer and starts `drive_viewer`'s existing "while loading" repaint loop and submits
the preview decode — without waiting for stray input. All four open call sites (grid double-click,
filmstrip click, Enter, arrow navigation) already have `ctx` in scope and pass it through.

No other repaint wiring changes: `spawn_preview`/`spawn_full` already `ctx.request_repaint()` on
completion, and `drive_viewer` already repaints while `loading_preview || crossfading || tiles_loading`.

### 3.2 Loading indicator
`viewer::paint`, while the viewer has not yet received its first pixel (`!state.loaded`), draws a
centered `egui::Spinner` with a faint "Loading…" label (`theme::TEXT_DIM`) over the black canvas,
instead of bare black. It vanishes the instant the preview texture is uploaded (`state.loaded`
becomes true); the full-resolution crossfade then proceeds unchanged. The spinner path keeps
returning `true` (as today) so `drive_viewer` keeps the frame animating while it spins.

This covers RAW (embedded preview → later crossfade to full) and Standard images (preview is
full-res) identically — both show the spinner only until the first pixel.

## 4. Scope & files
- `ferrolite-app/src/app.rs` — `open_record` gains a `ctx: &egui::Context` parameter and calls
  `ctx.request_repaint()`; update the four call sites.
- `ferrolite-app/src/viewer/mod.rs` — `paint` draws the centered spinner + "Loading…" while
  `!state.loaded`.

## 5. Testing
Pure-logic-free (egui wiring + a widget). Gate: `cargo build` + `cargo clippy --workspace
--all-targets -- -D warnings` + `cargo test --workspace` green, plus the user's manual smoke
(double-click a large RAW → switches instantly, spinner shows, then the image appears and sharpens).

## 6. Out of scope (YAGNI)
Progress percentages, decode-time estimates, skeleton/blur placeholders, or a separate background
thread for the texture upload. Just an immediate switch + a spinner.

## 7. Decision recorded (2026-06-30)
Loading indicator = **spinner + faint "Loading…" label** (over spinner-only), per the user's intent
that the wait should clearly feel like work in progress.
