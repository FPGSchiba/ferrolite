# Develop Module Switch & Filmstrip Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make opening an image switch to the Develop module with a module-driven central panel, and give Develop a top-bar filmstrip of the current image set navigable by click and arrow keys.

**Architecture:** The Library/Develop module tab drives the central panel (Library→grid, Develop→viewer). Opening an image (grid double-click, Enter, filmstrip click, or arrow key) routes through one shared `FerroliteApp::open_record` that sets `module = Develop` and reuses the existing two-tier viewer load. The Develop top bar renders a new `library::filmstrip` widget instead of the Library filters; image-to-image movement uses a pure `viewer::nav::neighbor_index` helper (non-cyclic).

**Tech Stack:** Rust 2021, egui/eframe 0.29, the existing `ferrolite-app` viewer (rung-4 sparse VT), `ferrolite-catalog` thumbnails.

## Global Constraints

- Rust edition 2021, `rust-version = "1.88"`. Stay on branch `feat/viewer-and-vt-ladder`; do NOT create a new branch.
- `cargo fmt --all` before every commit. Keep `cargo clippy --workspace --all-targets -- -D warnings` exit 0 and `cargo test --workspace` green.
- Conventional commits, no attribution footer.
- Arrow-key mapping is **standard**: Left = previous image, Right = next; **non-cyclic** (nothing happens at the ends).
- Filmstrip: Develop top bar height ~72px (Library stays 40px); ~96×64 (3:2) thumbnails reusing the catalog thumbnail cache + the grid's lazy-load path (`texture_cache.contains` → `reads.get_thumbnail` → `upload_thumbnail`); current image outlined in `theme::ACCENT`; auto-scroll to keep current visible.
- Left panel (236px) shown only in Library; hidden in Develop.
- Esc closes the viewer (cancelling its loads, as today) AND sets `module = Library`.
- Reuse existing APIs; do not change the viewer's two-tier load or the VT.

## File Structure

- `ferrolite-app/src/viewer/nav.rs` (create) — `Step` enum + pure `neighbor_index`.
- `ferrolite-app/src/viewer/mod.rs` (modify) — add `pub mod nav;`.
- `ferrolite-app/src/library/filmstrip.rs` (create) — the Develop top-bar filmstrip widget.
- `ferrolite-app/src/library/mod.rs` (modify) — add `pub mod filmstrip;`.
- `ferrolite-app/src/library/grid.rs` (modify) — `show` returns the double-clicked image id instead of opening directly.
- `ferrolite-app/src/app.rs` (modify) — `open_record` helper; module-driven central panel; per-module top bar (height + filmstrip vs filters); left panel gated on Library; Enter + arrow handling; Esc → close + Library.

---

## Task 1: Pure neighbor-index navigation helper

**Files:**
- Create: `ferrolite-app/src/viewer/nav.rs`
- Modify: `ferrolite-app/src/viewer/mod.rs`

**Interfaces:**
- Produces: `pub enum Step { Prev, Next }` and
  `pub fn neighbor_index(current: usize, len: usize, dir: Step) -> Option<usize>`.

- [ ] **Step 1: Write the failing test**

Create `ferrolite-app/src/viewer/nav.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_and_prev_in_the_middle() {
        assert_eq!(neighbor_index(2, 5, Step::Next), Some(3));
        assert_eq!(neighbor_index(2, 5, Step::Prev), Some(1));
    }

    #[test]
    fn clamps_non_cyclic_at_both_ends() {
        assert_eq!(neighbor_index(4, 5, Step::Next), None); // last → no next
        assert_eq!(neighbor_index(0, 5, Step::Prev), None); // first → no prev
    }

    #[test]
    fn empty_and_single_yield_none() {
        assert_eq!(neighbor_index(0, 0, Step::Next), None);
        assert_eq!(neighbor_index(0, 0, Step::Prev), None);
        assert_eq!(neighbor_index(0, 1, Step::Next), None);
        assert_eq!(neighbor_index(0, 1, Step::Prev), None);
    }

    #[test]
    fn out_of_range_current_is_none() {
        assert_eq!(neighbor_index(9, 5, Step::Next), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app nav::`
Expected: FAIL — `Step` / `neighbor_index` not defined.

- [ ] **Step 3: Write minimal implementation**

Prepend to `ferrolite-app/src/viewer/nav.rs`:
```rust
//! Pure, non-cyclic neighbour selection for image-to-image navigation in the
//! viewer. Left arrow = `Prev`, Right arrow = `Next`; no wraparound.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Prev,
    Next,
}

/// The index of the neighbour of `current` within a list of `len` items, or
/// `None` at the ends / when `current` is out of range / the list is empty.
pub fn neighbor_index(current: usize, len: usize, dir: Step) -> Option<usize> {
    if len == 0 || current >= len {
        return None;
    }
    match dir {
        Step::Prev => current.checked_sub(1),
        Step::Next => {
            let next = current + 1;
            (next < len).then_some(next)
        }
    }
}
```

Add to `ferrolite-app/src/viewer/mod.rs` (with the other `pub mod` lines near the top):
```rust
pub mod nav;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app nav::`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add ferrolite-app/src/viewer/nav.rs ferrolite-app/src/viewer/mod.rs
git commit -m "feat(app): pure non-cyclic neighbor_index for viewer navigation"
```

---

## Task 2: Develop filmstrip widget

**Files:**
- Create: `ferrolite-app/src/library/filmstrip.rs`
- Modify: `ferrolite-app/src/library/mod.rs`

**Interfaces:**
- Consumes: `crate::state::AppState` (fields `images: Vec<ImageRecord>`, `textures`, `reads`, plus `upload_thumbnail`), `crate::theme`.
- Produces: `pub fn show(ui: &mut egui::Ui, state: &mut AppState, current_id: Option<i64>) -> Option<i64>` — renders the horizontal thumbnail strip for `state.images`; returns the image id the user clicked this frame, if any. Outlines `current_id` in accent and auto-scrolls to it.

**Note:** there is no pure logic to unit-test here (it is egui rendering); the gate is `cargo build` + `clippy` + the existing app tests staying green, plus manual smoke. Mirror the thumbnail lazy-load + paint pattern from `library/grid.rs::paint_cell` (lines ~68-110).

- [ ] **Step 1: Write the widget**

Create `ferrolite-app/src/library/filmstrip.rs`:
```rust
//! Develop top-bar filmstrip: a horizontally-scrolling row of the current
//! folder's image thumbnails (same order as the grid), with the open image
//! outlined in the accent colour. Clicking a thumbnail returns its id so the
//! app can switch the viewer to it. Reuses the catalog thumbnail cache and the
//! grid's lazy-load path.

use crate::state::AppState;
use crate::theme;

/// Thumbnail cell size (3:2) and gap, in points.
const THUMB_W: f32 = 96.0;
const THUMB_H: f32 = 64.0;
const GAP: f32 = 6.0;

/// Render the strip; return the image id clicked this frame, if any.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, current_id: Option<i64>) -> Option<i64> {
    let mut clicked: Option<i64> = None;
    // Snapshot the ids/decode-status up front so we don't hold an immutable
    // borrow of `state.images` while mutably borrowing `state` for thumbnails.
    let ids: Vec<(i64, bool)> = state
        .images
        .iter()
        .map(|r| {
            (
                r.id,
                r.decode_status != ferrolite_catalog::DecodeStatus::Failed,
            )
        })
        .collect();

    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for (id, decodable) in ids {
                    // Lazy-load the thumbnail (same path as the grid).
                    if !state.textures.contains(id) && decodable {
                        if let Ok(Some(thumb)) = state.reads.get_thumbnail(id) {
                            state.upload_thumbnail(ui.ctx(), id, thumb.bytes);
                        }
                    }
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(THUMB_W, THUMB_H),
                        egui::Sense::click(),
                    );
                    if let Some(tex) = state.textures.get(id) {
                        egui::Image::new(tex)
                            .fit_to_exact_size(rect.size())
                            .paint_at(ui, rect);
                    } else {
                        ui.painter().rect_filled(rect, 2.0, theme::BG_PANEL);
                    }
                    if Some(id) == current_id {
                        ui.painter().rect_stroke(
                            rect,
                            2.0,
                            egui::Stroke::new(2.0, theme::ACCENT),
                        );
                        // Keep the open image in view as navigation moves it.
                        ui.scroll_to_rect(rect, None);
                    }
                    if resp.clicked() {
                        clicked = Some(id);
                    }
                }
            });
        });
    clicked
}
```

Add to `ferrolite-app/src/library/mod.rs`:
```rust
pub mod filmstrip;
```

- [ ] **Step 2: Verify it compiles + clippy clean**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: exit 0 (the function is unused until Task 3 wires it; if clippy flags `dead_code`, that is expected and Task 3 removes it by calling `show` — to keep this commit clean, add a temporary `#[allow(dead_code)]` on `show` with a comment "// wired in Task 3" and REMOVE it in Task 3).

- [ ] **Step 3: Commit**

```bash
cargo fmt --all
git add ferrolite-app/src/library/filmstrip.rs ferrolite-app/src/library/mod.rs
git commit -m "feat(app): Develop top-bar filmstrip widget (thumbnails + accent-outlined current)"
```

---

## Task 3: Wire module switch, central panel, top bar, left panel & navigation

**Files:**
- Modify: `ferrolite-app/src/library/grid.rs` (return the opened id instead of opening directly)
- Modify: `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `viewer::nav::{neighbor_index, Step}`, `library::filmstrip::show`, `crate::module::Module`, existing `AppState::open_image_in_viewer`, `FerroliteApp::cancel_viewer_tiles`.
- Produces: `FerroliteApp::open_record(&mut self, frame: &mut eframe::Frame, rec: &ferrolite_catalog::ImageRecord)` — the single open path (cancel old viewer tiles → `state.open_image_in_viewer(rec)` → `self.module = Module::Develop`).

**Context — current behavior (to change):**
- `grid::show(ui, &mut state, cell)` calls `state.open_image_in_viewer(rec)` directly on double-click (it cannot set `self.module`, which lives on `FerroliteApp`).
- `app.rs` central panel: `if viewer.is_some() { drive_viewer } else if library { grid } else { stub }` (viewer overrides the module).
- Top panel "toolbar" (exact_height 40) shows `library::toolbar::show` only when `module.is_library()`.
- Left `SidePanel` "left" is always shown.
- Enter-to-open and Esc-close already exist in `app.rs update()`.

- [ ] **Step 1: Make `grid::show` return the double-clicked id**

In `ferrolite-app/src/library/grid.rs`: change `show` and `paint_cell` to bubble up the double-clicked image id instead of opening the viewer. `paint_cell` currently ends with the interaction block; replace the `resp.double_clicked()` arm so it RETURNS the id rather than calling `open_image_in_viewer`.

Change `paint_cell`'s signature to return `Option<i64>`:
```rust
fn paint_cell(
    ui: &mut egui::Ui,
    state: &mut AppState,
    rec: &ferrolite_catalog::ImageRecord,
    rect: egui::Rect,
) -> Option<i64> {
    // ... unchanged thumbnail paint ...
    let resp = ui.interact(rect, ui.id().with(("cell", rec.id)), egui::Sense::click());
    if resp.clicked() {
        state.selected = Some(rec.id);
    }
    let mut opened = None;
    if resp.double_clicked() {
        opened = Some(rec.id);
    }
    if state.selected == Some(rec.id) {
        ui.painter_at(rect)
            .rect_stroke(rect, 2.0, egui::Stroke::new(2.0, theme::ACCENT));
    }
    opened
}
```
Change `show` to return `Option<i64>` — fold each cell's result, returning the last `Some` seen this frame (only one cell can be double-clicked per frame):
```rust
pub fn show(ui: &mut egui::Ui, state: &mut AppState, cell: f32) -> Option<i64> {
    // ... existing virtualized layout ...
    // wherever paint_cell is currently called, capture its return:
    //   if let Some(id) = paint_cell(ui, state, rec, rect) { opened = Some(id); }
    // declare `let mut opened = None;` before the cell loop and `opened` at the end.
}
```
(Keep the existing reprioritization / visibility logic. Only the open call changes to a return value.)

- [ ] **Step 2: Add `open_record` + rewire opens, central panel, top bar, left panel, arrows in `app.rs`**

In `ferrolite-app/src/app.rs`:

(a) Add the shared open helper as a method on `FerroliteApp` (near `cancel_viewer_tiles`):
```rust
/// The single image-open path: cancel the previously-open viewer's in-flight
/// tile jobs, open the new image's two-tier load, and switch to Develop.
fn open_record(&mut self, frame: &mut eframe::Frame, rec: &ferrolite_catalog::ImageRecord) {
    if let Some(old) = self.state.viewer.as_ref() {
        let old_id = old.image_id;
        old.cancel_loads();
        self.cancel_viewer_tiles(frame, old_id);
    }
    self.state.open_image_in_viewer(rec);
    self.module = crate::module::Module::Develop;
}
```

(b) Central panel — make it module-driven (replace the current `if viewer.is_some() … else if library … else …` block):
```rust
egui::CentralPanel::default()
    .frame(egui::Frame::none().fill(theme::BG_CANVAS))
    .show(ctx, |ui| {
        if self.module.is_library() {
            // Grid; capture a double-clicked id to open after the panel closes.
            opened = crate::library::grid::show(ui, &mut self.state, self.thumb_size + 60.0);
        } else if self.state.viewer.is_some() {
            self.drive_viewer(ui, frame);
        } else {
            let rect = ui.available_rect_before_wrap();
            canvas::paint(ui, rect); // Develop with no image open: stub canvas
        }
    });
if let Some(id) = opened {
    if let Some(rec) = self.state.images.iter().find(|r| r.id == id).cloned() {
        self.open_record(frame, &rec);
    }
}
```
Declare `let mut opened: Option<i64> = None;` just before the `CentralPanel`. (egui's `show` borrows `self` in the closure, so capture the id and call `open_record` AFTER the closure to avoid a double mutable borrow.)

(c) Top bar — per-module height + content. Replace the fixed `exact_height(40.0)` toolbar panel with a height chosen by module, and render the filmstrip in Develop:
```rust
let top_h = if self.module.is_library() { 40.0 } else { 72.0 };
let mut film_clicked: Option<i64> = None;
egui::TopBottomPanel::top("toolbar")
    .exact_height(top_h)
    .frame(
        egui::Frame::none()
            .fill(theme::BG_TOOLBAR)
            .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
    )
    .show(ctx, |ui| {
        if self.module.is_library() {
            let changed = crate::library::toolbar::show(
                ui,
                &mut self.thumb_size,
                &mut self.state.include_subfolders,
            );
            if changed {
                self.state.dirty = true;
            }
        } else {
            let current = self.state.viewer.as_ref().map(|v| v.image_id);
            film_clicked = crate::library::filmstrip::show(ui, &mut self.state, current);
        }
    });
if let Some(id) = film_clicked {
    if let Some(rec) = self.state.images.iter().find(|r| r.id == id).cloned() {
        self.open_record(frame, &rec);
    }
}
```

(d) Left panel — gate on Library. Wrap the existing `SidePanel::left("left")…show(…)` block in `if self.module.is_library() { … }`.

(e) Enter-to-open — route through `open_record` (it currently calls `self.state.open_image_in_viewer(&rec)` directly). Replace that call:
```rust
// inside the existing Enter handler, replacing `self.state.open_image_in_viewer(&rec);`
self.open_record(frame, &rec);
```

(f) Arrow-key navigation — add after the Enter handler:
```rust
// Left/Right move between images while viewing (Develop), non-cyclic.
if self.module == crate::module::Module::Develop
    && self.state.viewer.is_some()
    && !ctx.wants_keyboard_input()
{
    let dir = ctx.input(|i| {
        if i.key_pressed(egui::Key::ArrowRight) {
            Some(crate::viewer::nav::Step::Next)
        } else if i.key_pressed(egui::Key::ArrowLeft) {
            Some(crate::viewer::nav::Step::Prev)
        } else {
            None
        }
    });
    if let Some(dir) = dir {
        let cur_id = self.state.viewer.as_ref().map(|v| v.image_id);
        if let Some(cur_id) = cur_id {
            if let Some(pos) = self.state.images.iter().position(|r| r.id == cur_id) {
                if let Some(n) = crate::viewer::nav::neighbor_index(pos, self.state.images.len(), dir) {
                    let rec = self.state.images[n].clone();
                    self.open_record(frame, &rec);
                }
            }
        }
    }
}
```

(g) Esc — after closing the viewer, also return to Library. In the existing Esc handler, after the `self.state.viewer.take()` block, add:
```rust
self.module = crate::module::Module::Library;
```
(Only switch to Library when an Esc actually closed a viewer — keep it inside the `if let Some(v) = …take()` block so a stray Esc in Library is a no-op.)

(h) Remove the temporary `#[allow(dead_code)]` added to `filmstrip::show` in Task 2 (it is now called).

- [ ] **Step 3: Verify build + tests + lints**

Run:
```bash
cargo build -p ferrolite-app
cargo test -p ferrolite-app
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: build OK; app tests pass (including `nav::` and the existing viewer math tests); clippy exit 0. (The `Module` enum already derives `PartialEq` — `self.module == Module::Develop` compiles; if not, add `PartialEq, Eq` to its derive.)

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/app.rs ferrolite-app/src/library/grid.rs ferrolite-app/src/library/filmstrip.rs
git commit -m "feat(app): module-driven central panel, Develop filmstrip + arrow nav, hide left panel in Develop"
```

- [ ] **Step 5: Manual smoke (user)**

Open the app, ingest/select a folder, double-click an image → confirm it switches to Develop, the left panel hides, and the top bar shows the filmstrip with the open image outlined. Press Left/Right → previous/next image loads (preview→full), nothing at the ends. Click another filmstrip thumb → switches. Click the Library tab → grid returns; double-click another image → back to Develop. Esc → returns to the Library grid.

---

## Self-Review

**Spec coverage:**
- §3.1 module-driven central panel + open switches to Develop + tabs preserve viewer + Esc→Library → Task 3 (b),(a),(e),(g). ✓
- §3.2 left panel hidden in Develop → Task 3 (d). ✓
- §3.3 top bar filters vs filmstrip + ~72px height → Task 3 (c) + Task 2. ✓
- §3.4 navigation (neighbor_index, arrows, filmstrip clicks, shared open path) → Task 1 + Task 3 (a),(c),(f). ✓
- §3.5 files → all created/modified as listed. ✓
- §4 testing (neighbor_index units; glue via build/clippy/smoke) → Task 1 tests; Tasks 2-3 build/clippy. ✓

**Placeholder scan:** No TBD/TODO. The only deferred item is the explicit `#[allow(dead_code)]` on `filmstrip::show` introduced in Task 2 and REMOVED in Task 3 step (h) — called out, not a silent gap.

**Type consistency:** `open_record(&mut self, frame, &ImageRecord)`, `grid::show(...) -> Option<i64>`, `filmstrip::show(ui, &mut AppState, Option<i64>) -> Option<i64>`, `neighbor_index(usize, usize, Step) -> Option<usize>`, `Module::Develop`/`is_library()` used consistently across tasks. The `opened`/`film_clicked` captures + post-closure `open_record` calls avoid double mutable borrows of `self`.
