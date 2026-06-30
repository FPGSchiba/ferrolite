# Develop metadata & filters (Spec 1.6) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the Library's rating/flag/tag/collection organizing model and filtering into the Develop module for a fast cull-while-viewing workflow — without rebuilding any plumbing.

**Architecture:** Reuse Spec 1.5's `AppState::apply_metadata_edit`, the `icons` draw helpers, the `FilterState` widgets, and the grid context menu. Add a single-image edit entry point, a pure navigation helper for filtered sets, a slim Develop filter strip, filmstrip rating/flag overlays, a bottom current-image metadata bar, Develop keyboard shortcuts, and a shared right-click menu. All edits are current-image-only and persist exactly as in Spec 1.5 (optimistic + off-thread job).

**Tech Stack:** Rust 2021, egui/eframe 0.29.1 (pinned), the existing `ferrolite-app` crate (dual roots: `lib.rs` for tests + `main.rs` for the binary; `app`/`canvas`/`module` are bin-only; everything under `library/` is declared in `library/mod.rs` and visible from both roots).

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-06-30-develop-metadata-and-filters-design.md` — read it before starting.
- **Branch:** `feat/tags-and-filters` (continue on it; do not branch).
- **Responsiveness (CLAUDE.md §1):** nothing slow on the UI thread. Metadata writes go through the existing off-thread `spawn_metadata_write`; the filmstrip stays virtualized (visible cells only).
- **Icons are DRAWN shapes** via `crate::library::icons` (`star`, `rating_stars`, `flag`, `caret`) — NEVER font glyphs (IBM Plex lacks ★⚑▾). This was the Spec-1.5 visual-fix rule.
- **Edits are current-image only** in Develop (no filmstrip multi-select).
- **Persistence unchanged:** rating → `xmp:Rating` sidecar + SQLite mirror; flag/tags/collections → SQLite. Don't touch the model.
- **`.lock().expect("writer")`** is the established writer-mutex pattern (acceptable in production here). No other `unwrap()`/`expect()` outside tests.
- **Gate:** `cargo fmt --all --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` all green. Then HOLD for the author's visual test (CLAUDE.md rule) before finishing — do not merge.
- Commit after each task (conventional commits). Do NOT spawn your own code-review subagent (a separate reviewer runs per task).

---

## Task 1: Pure navigation for a filtered set (`neighbor_in_set`)

**Files:**
- Modify: `ferrolite-app/src/viewer/nav.rs` (add function + tests)
- Modify: `ferrolite-app/src/app.rs` (Develop ←/→ handler, lines ~536-562, to use it)

**Interfaces:**
- Consumes: existing `Step { Prev, Next }` in `viewer/nav.rs`.
- Produces: `pub fn neighbor_in_set(ids: &[i64], current: i64, dir: Step) -> Option<i64>` — the prev/next image id within `ids`; when `current` is absent from `ids`, returns the first (Next) or last (Prev) id; `None` only when `ids` is empty or already at the end.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-app/src/viewer/nav.rs — add inside `mod tests`
    #[test]
    fn neighbor_in_set_walks_and_clamps() {
        let ids = vec![10, 20, 30];
        assert_eq!(neighbor_in_set(&ids, 20, Step::Next), Some(30));
        assert_eq!(neighbor_in_set(&ids, 20, Step::Prev), Some(10));
        assert_eq!(neighbor_in_set(&ids, 30, Step::Next), None); // at end
        assert_eq!(neighbor_in_set(&ids, 10, Step::Prev), None); // at start
    }

    #[test]
    fn neighbor_in_set_falls_back_when_current_absent() {
        let ids = vec![10, 20, 30];
        // 99 is not in the set: Next → first, Prev → last.
        assert_eq!(neighbor_in_set(&ids, 99, Step::Next), Some(10));
        assert_eq!(neighbor_in_set(&ids, 99, Step::Prev), Some(30));
    }

    #[test]
    fn neighbor_in_set_empty_and_single() {
        assert_eq!(neighbor_in_set(&[], 1, Step::Next), None);
        assert_eq!(neighbor_in_set(&[10], 10, Step::Next), None);
        assert_eq!(neighbor_in_set(&[10], 10, Step::Prev), None);
        // single element, current absent → fallback to that element.
        assert_eq!(neighbor_in_set(&[10], 99, Step::Next), Some(10));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app nav::tests::neighbor_in_set`
Expected: FAIL — `neighbor_in_set` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-app/src/viewer/nav.rs — add above `mod tests`
/// Prev/next image id within `ids`. If `current` is not in `ids` (e.g. it was
/// filtered out), Next falls back to the first id and Prev to the last, so
/// arrow-keys still move into the filtered set. Non-cyclic at the ends.
pub fn neighbor_in_set(ids: &[i64], current: i64, dir: Step) -> Option<i64> {
    if ids.is_empty() {
        return None;
    }
    match ids.iter().position(|id| *id == current) {
        Some(pos) => neighbor_index(pos, ids.len(), dir).map(|n| ids[n]),
        None => match dir {
            Step::Next => ids.first().copied(),
            Step::Prev => ids.last().copied(),
        },
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app nav::tests`
Expected: PASS (all nav tests, including the existing `neighbor_index` ones).

- [ ] **Step 5: Wire it into the Develop arrow handler**

```rust
// ferrolite-app/src/app.rs — replace the body inside `if let Some(dir) = dir { ... }`
//   (the Develop Left/Right block, ~lines 549-561) with:
            if let Some(dir) = dir {
                let cur_id = self.state.viewer.as_ref().map(|v| v.image_id);
                if let Some(cur_id) = cur_id {
                    let ids: Vec<i64> = self.state.images.iter().map(|r| r.id).collect();
                    if let Some(next_id) = crate::viewer::nav::neighbor_in_set(&ids, cur_id, dir) {
                        if let Some(rec) =
                            self.state.images.iter().find(|r| r.id == next_id).cloned()
                        {
                            self.open_record(ctx, frame, &rec);
                        }
                    }
                }
            }
```

- [ ] **Step 6: Build + commit**

Run: `cargo build -p ferrolite-app && cargo test -p ferrolite-app nav:: && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
Expected: all clean.

```bash
git add ferrolite-app/src/viewer/nav.rs ferrolite-app/src/app.rs
git commit -m "feat(app): neighbor_in_set — arrow-nav within the filtered set, current-absent fallback"
```

---

## Task 2: Single-image edit entry point (state refactor)

**Files:**
- Modify: `ferrolite-app/src/state.rs` (`apply_metadata_edit`, `add_selection_to_collection`; add core + single-image methods)
- Test: `ferrolite-app/src/state.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `crate::metadata::{MetaEdit, apply_edit_in_memory, spawn_metadata_write}`, `self.images`, `self.visible_tags`, `self.reads`, `self.writer`, `self.source`.
- Produces:
  - `AppState::apply_metadata_edit_to_ids(&mut self, ctx: &egui::Context, ids: &[i64], edit: MetaEdit)` — the shared core (collect paths, optimistic update each, one `spawn_metadata_write`).
  - `AppState::apply_metadata_edit_to_image(&mut self, ctx: &egui::Context, image_id: i64, edit: MetaEdit)` — convenience for one image.
  - `AppState::add_images_to_collection(&mut self, ids: &[i64], coll_id: i64)` core + `AppState::add_image_to_collection_now(&mut self, image_id: i64, coll_id: i64)`.
  - `apply_metadata_edit` and `add_selection_to_collection` keep their signatures and now delegate to the cores (so multi-select still issues ONE job — batching preserved).

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-app/src/state.rs — add inside the existing `#[cfg(test)] mod tests`
    #[test]
    fn apply_metadata_edit_to_image_targets_only_that_image() {
        use ferrolite_catalog::{DecodeStatus, FileKind};
        use ferrolite_image::{Flag, Orientation, Rating};
        let mut s = AppState::for_test();
        let ctx = egui::Context::default();
        let mk = |id: i64| ferrolite_catalog::ImageRecord {
            id,
            folder_id: 99,
            filename: format!("img{id}.nef"),
            width: None,
            height: None,
            orientation: Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: DecodeStatus::Done,
            kind: FileKind::Raw,
            rating: Rating::default(),
            flag: Flag::None,
        };
        s.images = vec![mk(1), mk(2)];
        // Selection is image 2, but we edit image 1 explicitly.
        s.selection = [2].into_iter().collect();
        s.selected = Some(2);

        s.apply_metadata_edit_to_image(&ctx, 1, crate::metadata::MetaEdit::SetRating(Rating::new(4)));

        let r1 = s.images.iter().find(|r| r.id == 1).unwrap().rating;
        let r2 = s.images.iter().find(|r| r.id == 2).unwrap().rating;
        assert_eq!(r1, Rating::new(4), "explicit target updated");
        assert_eq!(r2, Rating::default(), "selection NOT touched");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app state::tests::apply_metadata_edit_to_image_targets_only_that_image`
Expected: FAIL — method not defined.

- [ ] **Step 3: Refactor `apply_metadata_edit` to delegate to a shared core; add the new methods**

```rust
// ferrolite-app/src/state.rs — REPLACE the body of `apply_metadata_edit` (it currently
// resolves targets then inlines path-collection/optimistic-update/spawn) with a thin resolver:
    pub fn apply_metadata_edit(&mut self, ctx: &egui::Context, edit: MetaEdit) {
        let mut targets: Vec<i64> = self.selection.iter().copied().collect();
        if targets.is_empty() {
            if let Some(id) = self.selected {
                targets.push(id);
            }
        }
        self.apply_metadata_edit_to_ids(ctx, &targets, edit);
    }

    /// Shared core: optimistically update each id's in-memory row + tag cache,
    /// then persist all of them in ONE off-thread job (DB + xmp:Rating).
    pub fn apply_metadata_edit_to_ids(&mut self, ctx: &egui::Context, ids: &[i64], edit: MetaEdit) {
        if ids.is_empty() {
            return;
        }
        let mut image_paths: Vec<(i64, std::path::PathBuf)> = Vec::new();
        for id in ids {
            if let Some(rec) = self.images.iter().find(|r| r.id == *id).cloned() {
                if let Ok(Some(fp)) = self.reads.folder_path(rec.folder_id) {
                    image_paths.push((*id, std::path::PathBuf::from(fp).join(&rec.filename)));
                }
            }
        }
        for id in ids {
            let mut tags = self.visible_tags.get(id).cloned().unwrap_or_default();
            if let Some(rec) = self.images.iter_mut().find(|r| r.id == *id) {
                crate::metadata::apply_edit_in_memory(rec, &mut tags, edit);
            }
            self.visible_tags.insert(*id, tags);
        }
        crate::metadata::spawn_metadata_write(&self.jobs, &self.writer, &self.tx, ctx, edit, image_paths);
    }

    /// Apply an edit to a single explicit image (used by Develop: the open viewer image).
    pub fn apply_metadata_edit_to_image(&mut self, ctx: &egui::Context, image_id: i64, edit: MetaEdit) {
        self.apply_metadata_edit_to_ids(ctx, &[image_id], edit);
    }
```

```rust
// ferrolite-app/src/state.rs — REPLACE `add_selection_to_collection` body to delegate,
// and add the core + single-image helper:
    pub fn add_selection_to_collection(&mut self, coll_id: i64) {
        let mut targets: Vec<i64> = self.selection.iter().copied().collect();
        if targets.is_empty() {
            if let Some(id) = self.selected {
                targets.push(id);
            }
        }
        self.add_images_to_collection(&targets, coll_id);
    }

    pub fn add_images_to_collection(&mut self, ids: &[i64], coll_id: i64) {
        if ids.is_empty() {
            return;
        }
        {
            let w = self.writer.lock().expect("writer");
            for id in ids {
                let _ = w.add_image_to_collection(coll_id, *id);
            }
        }
        if matches!(self.source, ViewSource::Collection(id) if id == coll_id) {
            self.dirty = true;
        }
    }

    pub fn add_image_to_collection_now(&mut self, image_id: i64, coll_id: i64) {
        self.add_images_to_collection(&[image_id], coll_id);
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ferrolite-app state:: && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS — the new test plus the existing `apply_metadata_edit_toggle_tag_updates_visible_tags` and `add_selection_to_collection_adds_images_and_sets_dirty_when_viewing` (now exercising the delegation) all pass.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/state.rs
git commit -m "refactor(app): single-image metadata + collection edit entry points (shared core)"
```

---

## Task 3: Extract shared filter widgets (DRY the toolbar)

**Files:**
- Create: `ferrolite-app/src/library/filter_widgets.rs`
- Modify: `ferrolite-app/src/library/mod.rs` (`pub mod filter_widgets;`)
- Modify: `ferrolite-app/src/library/toolbar.rs` (call the shared widgets)
- Test: `ferrolite-app/src/library/filter_widgets.rs` (pure `clickable_stars` math)

**Interfaces:**
- Consumes: `crate::library::icons`, `ferrolite_catalog::{SortKey, TagMode, TagRecord}`, `ferrolite_image::{Flag, TagId}`.
- Produces (all return `true` when they changed the bound state):
  - `pub fn clickable_stars(ui, current: u8, max: u8) -> Option<u8>` — draws `max` stars (first `current` filled), returns the new value on click (clicking the active value yields 0). The hit-math (`star_value_clicked`) is a pure tested helper.
  - `pub fn rating_threshold(ui, min_rating: &mut u8) -> bool`
  - `pub fn flag_filters(ui, flags: &mut Vec<Flag>) -> bool`
  - `pub fn tag_filter_dropdown(ui, tag_ids: &mut Vec<TagId>, mode: &mut TagMode, tags: &[TagRecord]) -> bool`
  - `pub fn sort_controls(ui, key: &mut SortKey, desc: &mut bool) -> bool`

- [ ] **Step 1: Write the failing test (pure hit-math)**

```rust
// ferrolite-app/src/library/filter_widgets.rs
//! Reusable Library/Develop filter widgets, drawn with the `icons` helpers and
//! bound to `FilterState` fields. Shared by the Library toolbar and the Develop
//! filter strip so the two never duplicate widget logic.

use crate::library::icons;
use ferrolite_catalog::{SortKey, TagMode, TagRecord};
use ferrolite_image::{Flag, TagId};

/// Given the current value and the star index clicked (1-based), return the new
/// value: clicking the already-active value clears to 0, else sets to the clicked index.
pub fn star_value_clicked(current: u8, clicked: u8) -> u8 {
    if current == clicked {
        0
    } else {
        clicked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_click_sets_or_clears() {
        assert_eq!(star_value_clicked(0, 3), 3); // set
        assert_eq!(star_value_clicked(3, 3), 0); // clicking active clears
        assert_eq!(star_value_clicked(2, 5), 5); // change
        assert_eq!(star_value_clicked(5, 1), 1); // lower
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app filter_widgets::`
Expected: FAIL — module not declared / `star_value_clicked` not found.

- [ ] **Step 3: Implement the widgets (port the existing toolbar code) + declare the module**

```rust
// ferrolite-app/src/library/mod.rs — add
pub mod filter_widgets;
```

```rust
// ferrolite-app/src/library/filter_widgets.rs — add above the tests module.
// These bodies are ported verbatim from the current toolbar.rs so behavior is identical.

const STAR_R: f32 = 5.0;
const STAR_GAP: f32 = 3.0;

/// Draw `max` clickable stars (first `current` filled, ACCENT; rest outlined, TEXT_FAINT).
/// Returns the new value if a star was clicked (active→0), else None.
pub fn clickable_stars(ui: &mut egui::Ui, current: u8, max: u8) -> Option<u8> {
    let width = icons::advance_width(STAR_R, STAR_GAP, max);
    let (rect, _resp) =
        ui.allocate_exact_size(egui::vec2(width, STAR_R * 2.0 + 4.0), egui::Sense::hover());
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let clicked_now = ui.input(|i| i.pointer.primary_clicked());
    let cell = STAR_R * 2.0 + STAR_GAP;
    let mut result = None;
    for n in 1..=max {
        let cx = rect.left() + STAR_R + (n as f32 - 1.0) * cell;
        let center = egui::pos2(cx, rect.center().y);
        let filled = n <= current;
        let color = if filled {
            crate::theme::ACCENT
        } else {
            crate::theme::TEXT_FAINT
        };
        icons::star(ui.painter(), center, STAR_R, filled, color);
        let hit = egui::Rect::from_center_size(center, egui::vec2(cell, rect.height()));
        if clicked_now && pointer.map(|p| hit.contains(p)).unwrap_or(false) {
            result = Some(star_value_clicked(current, n));
        }
    }
    result
}

/// Rating-threshold control bound to `FilterState.min_rating`.
pub fn rating_threshold(ui: &mut egui::Ui, min_rating: &mut u8) -> bool {
    if let Some(v) = clickable_stars(ui, *min_rating, 5) {
        *min_rating = v;
        true
    } else {
        false
    }
}

/// Flag-filter toggles (Pick green, Reject red); filled when active.
pub fn flag_filters(ui: &mut egui::Ui, flags: &mut Vec<Flag>) -> bool {
    let mut changed = false;
    for (f, color) in [
        (Flag::Pick, crate::theme::SEMANTIC_GREEN),
        (Flag::Reject, crate::theme::SEMANTIC_RED),
    ] {
        let active = flags.contains(&f);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
        if active {
            ui.painter()
                .rect_filled(rect, 2.0, crate::theme::ACCENT_BG_SEL);
        }
        icons::flag(ui.painter(), rect.center() + egui::vec2(0.0, 4.0), 11.0, active, color);
        if resp.clicked() {
            if let Some(p) = flags.iter().position(|x| *x == f) {
                flags.remove(p);
            } else {
                flags.push(f);
            }
            changed = true;
        }
    }
    changed
}

/// Tag multi-select dropdown with Any/All mode.
pub fn tag_filter_dropdown(
    ui: &mut egui::Ui,
    tag_ids: &mut Vec<TagId>,
    mode: &mut TagMode,
    tags: &[TagRecord],
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt("tagfilter")
        .selected_text(format!("Tags ({})", tag_ids.len()))
        .show_ui(ui, |ui| {
            let all = matches!(mode, TagMode::All);
            if ui.selectable_label(!all, "Any").clicked() {
                *mode = TagMode::Any;
                changed = true;
            }
            if ui.selectable_label(all, "All").clicked() {
                *mode = TagMode::All;
                changed = true;
            }
            ui.separator();
            for t in tags {
                let mut on = tag_ids.contains(&t.id);
                if ui.checkbox(&mut on, &t.name).changed() {
                    if let Some(p) = tag_ids.iter().position(|x| *x == t.id) {
                        tag_ids.remove(p);
                    } else {
                        tag_ids.push(t.id);
                    }
                    changed = true;
                }
            }
        });
    changed
}

/// Sort-key combo + ascending/descending caret.
pub fn sort_controls(ui: &mut egui::Ui, key: &mut SortKey, desc: &mut bool) -> bool {
    let mut changed = false;
    let label = |k: SortKey| match k {
        SortKey::CaptureTime => "Capture Time",
        SortKey::Filename => "Filename",
        SortKey::Rating => "Rating",
        SortKey::AddedAt => "Date Added",
    };
    egui::ComboBox::from_id_salt("sort")
        .selected_text(label(*key))
        .show_ui(ui, |ui| {
            for k in [SortKey::CaptureTime, SortKey::Filename, SortKey::Rating, SortKey::AddedAt] {
                if ui.selectable_value(key, k, label(k)).clicked() {
                    changed = true;
                }
            }
        });
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    icons::caret(ui.painter(), rect.center(), 4.0, crate::theme::TEXT_PRIMARY, *desc);
    if resp.clicked() {
        *desc = !*desc;
        changed = true;
    }
    changed
}
```

- [ ] **Step 4: Refactor `toolbar.rs` to call the shared widgets**

Replace the inlined rating-stars loop, flag-toggle loop, tag `ComboBox`, and sort combo+caret in `toolbar.rs::show` with calls to the new functions, accumulating `changed`. Keep search, the size slider, Subfolders, and the Metadata popover exactly as they are. Example (the rating/flag/tag/sort region):

```rust
// ferrolite-app/src/library/toolbar.rs — inside show(), replacing the four inlined blocks:
        use crate::library::filter_widgets as fw;
        if fw::sort_controls(ui, &mut state.filter.sort_key, &mut state.filter.sort_desc) {
            changed = true;
        }
        if fw::rating_threshold(ui, &mut state.filter.min_rating) {
            changed = true;
        }
        if fw::flag_filters(ui, &mut state.filter.flags) {
            changed = true;
        }
        if fw::tag_filter_dropdown(
            ui,
            &mut state.filter.tag_ids,
            &mut state.filter.tag_mode,
            &state.tags,
        ) {
            changed = true;
        }
```

Delete the now-unused local `toggle_flag`/`toggle_tag`/`sort_label` helpers from toolbar.rs if they are no longer referenced (clippy will flag them as dead).

- [ ] **Step 5: Build, test, verify behavior unchanged**

Run: `cargo build -p ferrolite-app && cargo test -p ferrolite-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: clean. (Visual: the Library toolbar looks/behaves exactly as before.)

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/library/filter_widgets.rs ferrolite-app/src/library/mod.rs ferrolite-app/src/library/toolbar.rs
git commit -m "refactor(app): extract shared filter widgets; toolbar reuses them"
```

---

## Task 4: Develop filter strip + two-row top panel

**Files:**
- Create: `ferrolite-app/src/library/develop_filter_bar.rs`
- Modify: `ferrolite-app/src/library/mod.rs` (`pub mod develop_filter_bar;`)
- Modify: `ferrolite-app/src/app.rs` (Develop top panel: height + render filter row above filmstrip)
- Test: none new (the `FilterState`→query mapping is already covered; this is rendering).

**Interfaces:**
- Consumes: `crate::library::filter_widgets`, `AppState`.
- Produces: `pub fn show(ui: &mut egui::Ui, state: &mut AppState) -> bool` — renders the compact filter row (rating threshold, flag toggles, tag dropdown, sort); returns `changed`.

- [ ] **Step 1: Implement the strip**

```rust
// ferrolite-app/src/library/develop_filter_bar.rs
//! Compact Develop filter strip: re-filter/sort the navigated set without leaving
//! Develop. Reuses `filter_widgets`; omits search + the metadata-range popover
//! (those stay in the Library toolbar).

use crate::library::filter_widgets as fw;
use crate::state::AppState;

/// Returns true if a filter/sort field changed (caller sets `state.dirty`).
pub fn show(ui: &mut egui::Ui, state: &mut AppState) -> bool {
    let mut changed = false;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        changed |= fw::sort_controls(ui, &mut state.filter.sort_key, &mut state.filter.sort_desc);
        changed |= fw::rating_threshold(ui, &mut state.filter.min_rating);
        changed |= fw::flag_filters(ui, &mut state.filter.flags);
        changed |= fw::tag_filter_dropdown(
            ui,
            &mut state.filter.tag_ids,
            &mut state.filter.tag_mode,
            &state.tags,
        );
    });
    changed
}
```

```rust
// ferrolite-app/src/library/mod.rs — add
pub mod develop_filter_bar;
```

- [ ] **Step 2: Grow the Develop top panel to two rows and render the strip above the filmstrip**

```rust
// ferrolite-app/src/app.rs — bump the Develop top height (the `top_h` line ~418):
        let top_h = if self.module.is_library() { 40.0 } else { 108.0 };
```

```rust
// ferrolite-app/src/app.rs — replace the `else` branch inside the toolbar panel `.show`
//   (currently just renders the filmstrip, ~lines 434-437) with a stacked layout:
                } else {
                    if crate::library::develop_filter_bar::show(ui, &mut self.state) {
                        self.state.dirty = true;
                    }
                    ui.separator();
                    let current = self.state.viewer.as_ref().map(|v| v.image_id);
                    film_clicked = crate::library::filmstrip::show(ui, &mut self.state, current);
                }
```

- [ ] **Step 3: Build + visual check**

Run: `cargo build -p ferrolite-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: clean. `/run`: in Develop, a filter row sits above the filmstrip; changing rating/flag/tag/sort re-filters the strip.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/library/develop_filter_bar.rs ferrolite-app/src/library/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(app): compact Develop filter strip above the filmstrip"
```

---

## Task 5: Filmstrip rating + flag overlays

**Files:**
- Modify: `ferrolite-app/src/library/filmstrip.rs`
- Test: none new (rendering).

**Interfaces:**
- Consumes: `crate::library::icons`, `ImageRecord.rating`/`.flag`.
- Note: `filmstrip::show` currently snapshots only `(id, decodable)`. Extend the snapshot to also carry `rating: u8` and `flag: Flag` so overlays draw without re-borrowing `state.images` during the mutable thumbnail loop.

- [ ] **Step 1: Carry rating/flag in the snapshot and draw overlays**

```rust
// ferrolite-app/src/library/filmstrip.rs — change the snapshot tuple to include rating+flag:
    let cells: Vec<(i64, bool, u8, ferrolite_image::Flag)> = state
        .images
        .iter()
        .map(|r| {
            (
                r.id,
                r.decode_status != ferrolite_catalog::DecodeStatus::Failed,
                r.rating.get(),
                r.flag,
            )
        })
        .collect();
```

Update the loop header to destructure the new tuple, and after painting the thumbnail (and the current-image outline), draw the overlays on visible cells:

```rust
// ferrolite-app/src/library/filmstrip.rs — inside `if ui.is_rect_visible(rect) { ... }`,
// after the existing thumbnail + current-outline painting:
                        if rating > 0 {
                            crate::library::icons::rating_stars(
                                ui.painter(),
                                rect.left_bottom() + egui::vec2(3.0, -6.0),
                                3.0,
                                1.5,
                                rating,
                                rating,
                                theme::ACCENT,
                            );
                        }
                        let flag_color = match flag {
                            ferrolite_image::Flag::Pick => Some(theme::SEMANTIC_GREEN),
                            ferrolite_image::Flag::Reject => Some(theme::SEMANTIC_RED),
                            ferrolite_image::Flag::None => None,
                        };
                        if let Some(c) = flag_color {
                            crate::library::icons::flag(
                                ui.painter(),
                                rect.left_top() + egui::vec2(7.0, 4.0),
                                10.0,
                                true,
                                c,
                            );
                        }
```

(The loop variable pattern becomes `for (id, decodable, rating, flag) in cells {`.)

- [ ] **Step 2: Build + visual check**

Run: `cargo build -p ferrolite-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: clean. `/run`: filmstrip thumbs show stars/flag; current image still outlined; scrolling stays smooth (still visible-only).

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/library/filmstrip.rs
git commit -m "feat(app): rating + flag overlays on filmstrip thumbnails"
```

---

## Task 6: Bottom current-image metadata bar

**Files:**
- Create: `ferrolite-app/src/library/develop_metadata_bar.rs`
- Modify: `ferrolite-app/src/library/mod.rs` (`pub mod develop_metadata_bar;`)
- Modify: `ferrolite-app/src/app.rs` (render a bottom panel in Develop with a viewer open)
- Test: none new (rendering; edit logic covered by Task 2).

**Interfaces:**
- Consumes: `crate::library::{filter_widgets, icons}`, `AppState`, `crate::metadata::MetaEdit`, `ferrolite_image::{Rating, Flag, TagId}`. Operates on a given `image_id` (the open viewer image).
- Produces: `pub fn show(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context, image_id: i64)`.

- [ ] **Step 1: Implement the bar**

```rust
// ferrolite-app/src/library/develop_metadata_bar.rs
//! Bottom Develop bar: rate / flag / tag / add-to-collection the OPEN image.
//! Fast cull-while-viewing; current-image only.

use crate::library::{filter_widgets, icons};
use crate::metadata::MetaEdit;
use crate::state::AppState;
use ferrolite_image::{Flag, Rating};

pub fn show(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context, image_id: i64) {
    // Read the open image's current rating/flag from its in-memory row.
    let (cur_rating, cur_flag) = state
        .images
        .iter()
        .find(|r| r.id == image_id)
        .map(|r| (r.rating.get(), r.flag))
        .unwrap_or((0, Flag::None));
    let image_tags = state.visible_tags.get(&image_id).cloned().unwrap_or_default();

    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;

        // Rating: clickable stars (set N / clear on re-click).
        if let Some(v) = filter_widgets::clickable_stars(ui, cur_rating, 5) {
            state.apply_metadata_edit_to_image(ctx, image_id, MetaEdit::SetRating(Rating::new(v)));
        }

        // Flag: Pick / Reject toggle buttons.
        for (f, color, label) in [
            (Flag::Pick, crate::theme::SEMANTIC_GREEN, "Pick"),
            (Flag::Reject, crate::theme::SEMANTIC_RED, "Reject"),
        ] {
            let active = cur_flag == f;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
            if active {
                ui.painter().rect_filled(rect, 2.0, crate::theme::ACCENT_BG_SEL);
            }
            icons::flag(ui.painter(), rect.center() + egui::vec2(0.0, 5.0), 12.0, active, color);
            if resp.on_hover_text(label).clicked() {
                let new = if active { Flag::None } else { f };
                state.apply_metadata_edit_to_image(ctx, image_id, MetaEdit::SetFlag(new));
            }
        }

        // Tags dropdown: toggle tags on the open image.
        let tags = state.tags.clone();
        egui::ComboBox::from_id_salt("develop_tags")
            .selected_text("Tags ▾".trim_end_matches('▾')) // plain text; no glyph
            .show_ui(ui, |ui| {
                for t in &tags {
                    let has = image_tags.contains(&t.id);
                    if ui.selectable_label(has, &t.name).clicked() {
                        state.apply_metadata_edit_to_image(ctx, image_id, MetaEdit::ToggleTag(t.id));
                    }
                }
            });

        // Add to collection.
        let collections = state.collections.clone();
        if !collections.is_empty() {
            ui.menu_button("Add to collection", |ui| {
                for c in &collections {
                    if ui.button(&c.name).clicked() {
                        state.add_image_to_collection_now(image_id, c.id);
                        ui.close_menu();
                    }
                }
            });
        }
    });
}
```

> Note: `ComboBox::selected_text` takes a plain string — use `"Tags"` (no caret glyph). egui draws the combo's own arrow.

```rust
// ferrolite-app/src/library/mod.rs — add
pub mod develop_metadata_bar;
```

- [ ] **Step 2: Render it as a bottom panel (Develop + viewer only), above the status bar**

```rust
// ferrolite-app/src/app.rs — AFTER the existing status `TopBottomPanel::bottom("status")` block
//   (so this panel sits just above the status bar), add:
        if self.module == crate::module::Module::Develop {
            if let Some(image_id) = self.state.viewer.as_ref().map(|v| v.image_id) {
                egui::TopBottomPanel::bottom("develop_meta")
                    .exact_height(34.0)
                    .frame(
                        egui::Frame::none()
                            .fill(theme::BG_TOOLBAR)
                            .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                    )
                    .show(ctx, |ui| {
                        crate::library::develop_metadata_bar::show(ui, &mut self.state, ctx, image_id);
                    });
            }
        }
```

- [ ] **Step 3: Build + visual check**

Run: `cargo build -p ferrolite-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: clean. `/run`: opening an image shows the bottom bar; clicking stars sets the rating (filmstrip overlay updates instantly); Pick/Reject toggle; Tags toggle; Add-to-collection works.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/library/develop_metadata_bar.rs ferrolite-app/src/library/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(app): bottom Develop metadata bar (rating/flag/tags/collection for the open image)"
```

---

## Task 7: Develop keyboard shortcuts (0–5, P/X/U → open image)

**Files:**
- Modify: `ferrolite-app/src/app.rs` (the metadata-key block, ~lines 498-533)
- Test: none new (routing; edit logic covered by Task 2).

**Interfaces:**
- Consumes: `AppState::apply_metadata_edit` (Library) and `apply_metadata_edit_to_image` (Develop), `self.module`, `self.state.viewer`.

- [ ] **Step 1: Route the key edit by module**

Replace the metadata-key block so the key→`MetaEdit` mapping is computed once (guarded against text focus + the remove modal), then routed: Library (no viewer) → selection; Develop (viewer open) → the open image.

```rust
// ferrolite-app/src/app.rs — REPLACE the existing "Keyboard metadata commands" block:
        // Keyboard metadata commands: rating 0–5, flag P/X/U. In Library they apply
        // to the grid selection; in Develop they apply to the open viewer image.
        if self.state.pending_remove.is_none() && !ctx.wants_keyboard_input() {
            use ferrolite_image::{Flag, Rating};
            let edit = ctx.input(|i| {
                for n in 0..=5u8 {
                    let key = match n {
                        0 => egui::Key::Num0,
                        1 => egui::Key::Num1,
                        2 => egui::Key::Num2,
                        3 => egui::Key::Num3,
                        4 => egui::Key::Num4,
                        _ => egui::Key::Num5,
                    };
                    if i.key_pressed(key) {
                        return Some(crate::metadata::MetaEdit::SetRating(Rating::new(n)));
                    }
                }
                if i.key_pressed(egui::Key::P) {
                    Some(crate::metadata::MetaEdit::SetFlag(Flag::Pick))
                } else if i.key_pressed(egui::Key::X) {
                    Some(crate::metadata::MetaEdit::SetFlag(Flag::Reject))
                } else if i.key_pressed(egui::Key::U) {
                    Some(crate::metadata::MetaEdit::SetFlag(Flag::None))
                } else {
                    None
                }
            });
            if let Some(edit) = edit {
                if self.module.is_library() && self.state.viewer.is_none() {
                    self.state.apply_metadata_edit(ctx, edit);
                } else if let Some(image_id) = self.state.viewer.as_ref().map(|v| v.image_id) {
                    self.state.apply_metadata_edit_to_image(ctx, image_id, edit);
                }
            }
        }
```

> Note: this removes the `self.module.is_library() && self.state.viewer.is_none()` outer guard and replaces it with per-branch routing. The `P`/`X`/`U`/number keys are still suppressed while a text field has focus (`!ctx.wants_keyboard_input()`), so renaming/search are unaffected.

- [ ] **Step 2: Build + visual check**

Run: `cargo build -p ferrolite-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: clean. `/run`: in Develop, pressing `3` rates the open image (bottom bar + filmstrip update); `P`/`X`/`U` set the flag; Library keyboard still works as before.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/app.rs
git commit -m "feat(app): Develop keyboard rating/flag commands target the open image"
```

---

## Task 8: Shared right-click context menu (loupe + filmstrip)

**Files:**
- Create: `ferrolite-app/src/library/image_context_menu.rs`
- Modify: `ferrolite-app/src/library/mod.rs` (`pub mod image_context_menu;`)
- Modify: `ferrolite-app/src/library/grid.rs` (use the shared helper in `paint_cell`)
- Modify: `ferrolite-app/src/library/filmstrip.rs` (attach to thumbnails)
- Modify: `ferrolite-app/src/app.rs` or `ferrolite-app/src/viewer/…` (attach to the loupe image response)
- Test: none new (rendering; reuses Task 2 logic).

**Interfaces:**
- Produces: `pub fn show(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context, image_id: i64)` — the Rating/Flag/Tags/Add-to-collection menu for `image_id`, scoped to that image (sets it as the sole selection if not already selected, so selection-based edits target correctly), to be used inside a `response.context_menu(|ui| { … })` closure.

- [ ] **Step 1: Extract the grid menu into the shared helper**

Move the body of the grid's `resp.context_menu(...)` (the Rating/Flag/Tags/Add-to-collection submenus from `grid.rs::paint_cell`) into:

```rust
// ferrolite-app/src/library/image_context_menu.rs
//! Reusable right-click metadata menu for a single image (Rating / Flag / Tags /
//! Add-to-collection). Shared by the grid, the Develop filmstrip, and the loupe.

use crate::metadata::MetaEdit;
use crate::state::AppState;
use ferrolite_image::{Flag, Rating};

/// Render the menu for `image_id` inside a `context_menu` closure. Scopes edits to
/// this image when it is not part of the current multi-selection.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context, image_id: i64) {
    let in_selection = state.selection.contains(&image_id);
    let tags = state.tags.clone();
    let collections = state.collections.clone();
    let image_tags = state.visible_tags.get(&image_id).cloned().unwrap_or_default();

    // Helper: apply to the multi-selection if this image is in it, else just this image.
    let apply = |state: &mut AppState, ctx: &egui::Context, edit: MetaEdit| {
        if in_selection {
            state.apply_metadata_edit(ctx, edit);
        } else {
            state.apply_metadata_edit_to_image(ctx, image_id, edit);
        }
    };

    ui.menu_button("Rating", |ui| {
        if ui.button("No rating").clicked() {
            apply(state, ctx, MetaEdit::SetRating(Rating::new(0)));
            ui.close_menu();
        }
        for n in 1u8..=5 {
            let label = format!("{n} star{}", if n == 1 { "" } else { "s" });
            if ui.button(label).clicked() {
                apply(state, ctx, MetaEdit::SetRating(Rating::new(n)));
                ui.close_menu();
            }
        }
    });
    ui.menu_button("Flag", |ui| {
        if ui.button("Pick").clicked() {
            apply(state, ctx, MetaEdit::SetFlag(Flag::Pick));
            ui.close_menu();
        }
        if ui.button("Reject").clicked() {
            apply(state, ctx, MetaEdit::SetFlag(Flag::Reject));
            ui.close_menu();
        }
        if ui.button("Unflag").clicked() {
            apply(state, ctx, MetaEdit::SetFlag(Flag::None));
            ui.close_menu();
        }
    });
    if !tags.is_empty() {
        ui.menu_button("Tags", |ui| {
            for t in &tags {
                let has = image_tags.contains(&t.id);
                if ui.selectable_label(has, &t.name).clicked() {
                    apply(state, ctx, MetaEdit::ToggleTag(t.id));
                    ui.close_menu();
                }
            }
        });
    }
    if !collections.is_empty() {
        ui.menu_button("Add to collection", |ui| {
            for c in &collections {
                if ui.button(&c.name).clicked() {
                    if in_selection {
                        state.add_selection_to_collection(c.id);
                    } else {
                        state.add_image_to_collection_now(image_id, c.id);
                    }
                    ui.close_menu();
                }
            }
        });
    }
}
```

```rust
// ferrolite-app/src/library/mod.rs — add
pub mod image_context_menu;
```

Then in `grid.rs::paint_cell`, replace the inline menu body with:

```rust
// ferrolite-app/src/library/grid.rs — the context-menu attachment becomes:
    let rec_id = rec.id;
    resp.context_menu(|ui| {
        crate::library::image_context_menu::show(ui, state, ui.ctx(), rec_id);
    });
```

(Delete the now-duplicated submenu code + the `tags_snapshot`/`collections_snapshot`/`image_tags` clones that only fed it. The grid's existing "scope to this cell if not selected" behavior is now handled inside the shared helper via `in_selection`.)

- [ ] **Step 2: Attach to filmstrip thumbnails**

In `filmstrip.rs`, the per-thumb response (`resp` from `allocate_exact_size`) gets a context menu:

```rust
// ferrolite-app/src/library/filmstrip.rs — after the click handling for a thumbnail:
                    let menu_id = id;
                    resp.context_menu(|ui| {
                        crate::library::image_context_menu::show(ui, state, ui.ctx(), menu_id);
                    });
```

(`state` is already `&mut` in `filmstrip::show`; ensure the borrow is available where the menu is attached.)

- [ ] **Step 3: Attach to the loupe image**

In the Develop central-panel viewer path (`app.rs`, the `self.drive_viewer(ui, frame)` area), add a context menu on the viewer's allocated image rect for `viewer.image_id`. The simplest hook: after `drive_viewer`, interact with the central panel rect and attach the menu:

```rust
// ferrolite-app/src/app.rs — in the CentralPanel Develop branch, after drive_viewer:
                } else if self.state.viewer.is_some() {
                    self.drive_viewer(ui, frame);
                    if let Some(image_id) = self.state.viewer.as_ref().map(|v| v.image_id) {
                        let rect = ui.min_rect();
                        let resp = ui.interact(
                            rect,
                            ui.id().with("loupe_ctx"),
                            egui::Sense::click(),
                        );
                        resp.context_menu(|ui| {
                            crate::library::image_context_menu::show(
                                ui,
                                &mut self.state,
                                ui.ctx(),
                                image_id,
                            );
                        });
                    }
                } else {
```

> If the loupe interaction rect conflicts with the viewer's own pan/zoom input (the viewer consumes drag/scroll, not secondary-click), keep the context menu but ensure the interact `Sense::click()` does not swallow the pan/zoom drag — `context_menu` only reacts to secondary clicks, so primary-drag pan/zoom still reaches the viewer. If a conflict appears in testing, attach the menu to the filmstrip + grid only and note the loupe menu as deferred.

- [ ] **Step 4: Build + visual check**

Run: `cargo build -p ferrolite-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check && cargo test -p ferrolite-app`
Expected: clean; grid context menu still works (now via the shared helper); filmstrip + loupe right-click show the same menu.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/library/image_context_menu.rs ferrolite-app/src/library/mod.rs ferrolite-app/src/library/grid.rs ferrolite-app/src/library/filmstrip.rs ferrolite-app/src/app.rs
git commit -m "feat(app): shared image right-click menu on grid, filmstrip, and loupe"
```

---

## Task 9: Gate green + hold for author test

- [ ] **Step 1: Full gate**

Run: `cargo fmt --all --check`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Run: `cargo test --workspace`
Expected: all green. Fix any lint/test fallout (use the rust-build-resolver agent for stubborn lints).

- [ ] **Step 2: Commit any fixups**

```bash
git add -A && git commit -m "chore: gate green for Develop metadata & filters"
```

- [ ] **Step 3: HOLD for the author's visual test**

Per CLAUDE.md, do NOT finish the branch on green alone. Report completion and the test checklist (filter strip re-filters the strip; filmstrip shows rating/flag; bottom bar + keyboard rate/flag/tag the open image; right-click menu on grid/filmstrip/loupe; ←/→ still navigate, including after a filter removes the open image). Wait for the author's feedback; address findings; then use **superpowers:finishing-a-development-branch**.

---

## Self-Review (filled in by the planner)

**Spec coverage:**
- §4.1 Develop filter strip → Task 3 (shared widgets) + Task 4. ✓
- §4.2 filmstrip overlays → Task 5. ✓
- §4.3 bottom metadata bar → Task 6. ✓
- §4.4 keyboard shortcuts → Task 7. ✓
- §4.5 shared right-click menu (loupe + filmstrip) → Task 8. ✓
- §5 targeting (`apply_metadata_edit_to_image`, single-image collection add, selection path delegates to shared core) → Task 2. ✓
- §6 nav when filtered out (`neighbor_in_set`) → Task 1. ✓
- §7 error handling — inherited from Spec 1.5's `MetadataResult` path (unchanged); empty-set nav handled by `neighbor_in_set`. ✓
- §8 testing — pure tests in Tasks 1, 2, 3; egui rendering via build + visual. ✓
- §9 build order — Tasks 1→8 follow it. ✓

**Placeholder scan:** no TBD/TODO; every code step shows full code. egui rendering steps reference exact files + the `icons`/`filter_widgets` APIs and are gated by build + `/run`. ✓

**Type consistency:** `neighbor_in_set(&[i64], i64, Step)`; `apply_metadata_edit_to_ids/_to_image`, `add_images_to_collection`/`add_image_to_collection_now`; `filter_widgets::{clickable_stars, rating_threshold, flag_filters, tag_filter_dropdown, sort_controls, star_value_clicked}`; `develop_filter_bar::show(ui, state)->bool`; `develop_metadata_bar::show(ui, state, ctx, image_id)`; `image_context_menu::show(ui, state, ctx, image_id)`; `icons::{star, rating_stars, flag, caret, advance_width}` (from Spec 1.5). All consistent across tasks. `icons::caret(painter, center, half_w, color, down)` and `icons::flag(painter, base, h, filled, color)` and `icons::rating_stars(painter, origin, r, gap, filled, total, color)` match the Spec-1.5 signatures the implementer will confirm against `icons.rs`. ✓
