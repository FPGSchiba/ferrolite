# Library Drag-and-Drop (images → collections / tags) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user drag the selected image(s) from the Library thumbnail grid and drop them onto a collection row or a tag row in the left panel to add those images to the collection / apply the tag.

**Architecture:** Reuse the drag-and-drop foundation proven by the Export queue (`export_module/queue_list.rs`): egui's native `DragAndDrop` payload + manual drop detection (egui 0.29.1 has **no** `dnd_drop_zone`, so drop targets poll the payload + pointer, exactly as the queue does). The grid cell becomes a drag *source* carrying a `DraggedImages(Vec<i64>)` payload (the whole multi-selection when the grabbed image is selected, else just that image). The left-panel collection/tag rows become drop *targets* that highlight on hover-with-payload and, on release, call the existing `AppState::add_images_to_collection` (collections) or an add-only tag apply (tags). No new persistence — reuses the cache-safe writer plumbing already in `state.rs`.

**Tech Stack:** Rust, egui/eframe 0.29.1, existing `ferrolite-app` Library UI + catalog plumbing.

## Global Constraints

- **egui 0.29.1** — `ui.dnd_drop_zone` does **not** exist; use the manual pattern from `export_module/queue_list.rs`: `egui::DragAndDrop::payload::<T>(ctx)` to peek an active drag, `ui.ctx().pointer_interact_pos()` for the pointer, `rect.contains(pointer)` for the hit test, `ui.input(|i| i.pointer.any_released())` for the drop, and `egui::DragAndDrop::take_payload::<T>(ctx)` to consume it. Do not add an external DnD crate.
- **Reuse existing plumbing, don't duplicate it:** collections use `AppState::add_images_to_collection(&self, ids: &[i64], coll_id: i64)` (state.rs:682); tags reuse `AppState::apply_metadata_edit_to_ids(ctx, ids, MetaEdit::ToggleTag(tag_id))` (state.rs:329) applied only to images lacking the tag (add-only semantics — see Task 3). Both paths already persist off-thread and are cache-safe.
- **CLAUDE.md responsiveness:** all drop actions reuse the existing off-thread persist jobs; the UI-thread work is only egui painting + `HashSet`/`Vec` bookkeeping. No new blocking work. The grid stays virtualized (do not add per-frame O(all-images) work).
- **Selection is authoritative in memory:** `AppState::selection: HashSet<i64>` (state.rs:121), `AppState::selected: Option<i64>` (state.rs:41). Do not change selection semantics; the drag only *reads* the selection.
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean; no `unwrap()` outside tests; typed, immutable-by-default.
- **Gate (necessary, not sufficient):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → then **hold for Jann's hands-on visual test** (drag feel, drop-target highlight, that the correct images land in the collection / get the tag) before finishing the branch. egui UI has no golden tests.
- **Branch:** `feat/library-dnd` (already created off `main`, pushed).

---

## File Structure

**Created:**
- `ferrolite-app/src/library/drag.rs` — the shared drag payload type `DraggedImages(Vec<i64>)`, the pure `ids_for_drag(grabbed, &selection) -> Vec<i64>` selection helper, and a small cursor-following drag chip painter. Unit-tested pure functions live here.

**Modified:**
- `ferrolite-app/src/library/grid.rs` — make each cell a drag source: change its `Sense` to `click_and_drag()`, set the `DraggedImages` payload on `drag_started()`, and (while a drag is active) paint the drag chip. Existing click/selection behavior is preserved.
- `ferrolite-app/src/library/panel.rs` — collection rows and tag rows become drop targets: highlight on hover-with-payload, and on release apply the add-to-collection / apply-tag action.
- `ferrolite-app/src/library/mod.rs` (or wherever `library` submodules are declared) — add `pub mod drag;`.
- `ferrolite-app/src/state.rs` — add a small `add_tag_to_images(ctx, ids, tag_id)` helper (add-only tag apply built on the existing `apply_metadata_edit_to_ids`), plus a pure `ids_missing_tag` filter used by it. (Only if Task 3's inline approach is preferred as a method; see Task 3.)

---

## Task 1: Drag payload + grid cells as drag sources

**Files:**
- Create: `ferrolite-app/src/library/drag.rs`
- Modify: `ferrolite-app/src/library/grid.rs`
- Modify: `ferrolite-app/src/library/mod.rs` (declare `pub mod drag;`)

**Interfaces:**
- Produces:
  - `pub struct DraggedImages(pub Vec<i64>)` — `#[derive(Clone)]`; used as an egui `DragAndDrop` payload (egui requires `Send + Sync + 'static`, which `Vec<i64>` satisfies).
  - `pub fn ids_for_drag(grabbed: i64, selection: &std::collections::HashSet<i64>) -> Vec<i64>` — if `grabbed` is in `selection`, returns all selected ids (sorted ascending for determinism); otherwise returns `vec![grabbed]`. Never empty.
  - `pub fn draw_drag_chip(ctx: &egui::Context, count: usize)` — while a drag is active, paints a small accent chip ("N image(s)") at the pointer on a foreground layer.

- [ ] **Step 1: Write the failing test for the selection helper.** Create `ferrolite-app/src/library/drag.rs`:
```rust
//! Shared drag-and-drop payload + helpers for dragging Library grid images onto
//! collection/tag rows. Mirrors the export-queue DnD pattern (egui native
//! `DragAndDrop` payload + manual drop detection).

use std::collections::HashSet;

/// The images being dragged from the grid. egui `DragAndDrop` payload type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraggedImages(pub Vec<i64>);

/// Which images a drag starting on `grabbed` should carry: the whole
/// multi-selection when `grabbed` is part of it, otherwise just `grabbed`.
/// Result is sorted ascending and never empty.
pub fn ids_for_drag(grabbed: i64, selection: &HashSet<i64>) -> Vec<i64> {
    if selection.contains(&grabbed) && selection.len() > 1 {
        let mut ids: Vec<i64> = selection.iter().copied().collect();
        ids.sort_unstable();
        ids
    } else {
        vec![grabbed]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grabbed_in_multiselection_drags_all_sorted() {
        let sel: HashSet<i64> = [3, 1, 2].into_iter().collect();
        assert_eq!(ids_for_drag(2, &sel), vec![1, 2, 3]);
    }

    #[test]
    fn grabbed_outside_selection_drags_only_itself() {
        let sel: HashSet<i64> = [1, 2].into_iter().collect();
        assert_eq!(ids_for_drag(9, &sel), vec![9]);
    }

    #[test]
    fn single_selection_drags_only_grabbed() {
        // A lone selected image drags just itself (len==1 → not "multi").
        let sel: HashSet<i64> = [5].into_iter().collect();
        assert_eq!(ids_for_drag(5, &sel), vec![5]);
    }
}
```

- [ ] **Step 2: Add the drag chip painter (no test — visual).** Append to `drag.rs`:
```rust
/// Paint a small chip that follows the cursor while a `DraggedImages` drag is
/// active, so the user sees how many images they're dragging. Drawn on a
/// foreground area layer so it sits above panels.
pub fn draw_drag_chip(ctx: &egui::Context, count: usize) {
    let Some(pos) = ctx.pointer_interact_pos() else {
        return;
    };
    let text = if count == 1 {
        "1 image".to_string()
    } else {
        format!("{count} images")
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("library_drag_chip"),
    ));
    let anchor = pos + egui::vec2(12.0, 8.0);
    let galley = painter.layout_no_wrap(
        text,
        egui::FontId::proportional(11.0),
        crate::theme::TEXT_PRIMARY,
    );
    let pad = egui::vec2(6.0, 3.0);
    let rect = egui::Rect::from_min_size(anchor, galley.size() + pad * 2.0);
    painter.rect_filled(rect, 3.0, crate::theme::ACCENT);
    painter.galley(anchor + pad, galley, crate::theme::TEXT_PRIMARY);
}
```
Declare the module: in `ferrolite-app/src/library/mod.rs` add `pub mod drag;` alongside the other `pub mod` lines.

- [ ] **Step 3: Run the tests to verify they pass.**

Run: `cargo test -p ferrolite-app library::drag`
Expected: 3 tests PASS.

- [ ] **Step 4: Make grid cells drag sources.** In `ferrolite-app/src/library/grid.rs` `paint_cell` (the cell interaction is at line ~343: `let resp = ui.interact(rect, ui.id().with(("cell", rec.id)), egui::Sense::click());`), change the sense to `click_and_drag()` and set the payload on drag start. Replace that `ui.interact(...)` line and add, immediately after the existing click-handling block:
```rust
    let resp = ui.interact(
        rect,
        ui.id().with(("cell", rec.id)),
        egui::Sense::click_and_drag(),
    );
    // ... existing click / ctrl / shift selection handling stays unchanged ...

    // Begin a drag carrying the selection (or just this image).
    if resp.drag_started() {
        let ids = crate::library::drag::ids_for_drag(rec.id, &state.selection);
        egui::DragAndDrop::set_payload(ui.ctx(), crate::library::drag::DraggedImages(ids));
    }
```
Note: `paint_cell` currently takes `state: &mut AppState` — confirm `state.selection` is reachable there (it is; `paint_cell` already borrows `state`). If `paint_cell`'s signature doesn't carry `state`, thread `&state.selection` in (grep the actual signature; the exploration shows `paint_cell(ui, state, &rec, img_rect, queued)` — `state` is present).

- [ ] **Step 5: Draw the drag chip while dragging.** In `grid.rs` `show()` (top-level grid render), after the scroll area is drawn, add:
```rust
    if egui::DragAndDrop::has_payload_of_type::<crate::library::drag::DraggedImages>(ui.ctx()) {
        if let Some(p) = egui::DragAndDrop::payload::<crate::library::drag::DraggedImages>(ui.ctx()) {
            crate::library::drag::draw_drag_chip(ui.ctx(), p.0.len());
        }
    }
```
(If `has_payload_of_type` is not the exact egui 0.29.1 name, drop it and just use the `if let Some(p) = DragAndDrop::payload::<_>()` guard — verify against the vendored egui source, same as `queue_list.rs` did.)

- [ ] **Step 6: Build + clippy.**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean. (Behavior — that a click still selects and a drag now starts a payload — is confirmed in the final visual test.)

- [ ] **Step 7: Commit.**
```bash
git add ferrolite-app/src/library/drag.rs ferrolite-app/src/library/grid.rs ferrolite-app/src/library/mod.rs
git commit -m "feat(app): Library grid cells as drag sources (DraggedImages payload)"
```

---

## Task 2: Collection rows as drop targets

**Files:**
- Modify: `ferrolite-app/src/library/panel.rs`

**Interfaces:**
- Consumes: `DraggedImages` (Task 1); `AppState::add_images_to_collection(&self, ids: &[i64], coll_id: i64)` (state.rs:682); `egui::DragAndDrop` peek/take.
- Produces: collection rows that highlight when a drag hovers them and add the dragged images on release.

- [ ] **Step 1: Add drop handling to the collection row.** In `panel.rs` the collection row is a `ui.horizontal(|ui| { ... })` (line ~176) whose name label response is `name_resp` (line ~207) and whose id is `c.id`. After the row's `ui.horizontal(...)` returns its `InnerResponse`, capture the row rect and add drop handling. Concretely, wrap the row so you have `let row_resp = ui.horizontal(|ui| { ... }).response;` then:
```rust
    // Drop target: dragging images onto a collection row adds them to it.
    let row_rect = row_resp.rect;
    if let Some(dragged) =
        egui::DragAndDrop::payload::<crate::library::drag::DraggedImages>(ui.ctx())
    {
        if let Some(pointer) = ui.ctx().pointer_interact_pos() {
            if row_rect.contains(pointer) {
                // Highlight the row as a valid drop target.
                ui.painter().rect_filled(
                    row_rect.expand2(egui::vec2(4.0, 1.0)),
                    3.0,
                    theme::ACCENT_BG_SEL,
                );
                if ui.input(|i| i.pointer.any_released()) {
                    let ids = dragged.0.clone();
                    // take_payload prevents re-applying on later rows this frame.
                    egui::DragAndDrop::take_payload::<crate::library::drag::DraggedImages>(ui.ctx());
                    state.add_images_to_collection(&ids, c.id);
                    state.warning = Some(format!(
                        "Added {} image(s) to \"{}\".",
                        ids.len(),
                        c.name
                    ));
                }
            }
        }
    }
```
Note on borrow/ordering: draw the highlight BEFORE mutating `state` (the `state.add_images_to_collection` call). `c.name`/`c.id` are read from the `state.collections` iteration — clone `c.name`/copy `c.id` before the mutable `state` call if the borrow checker complains (the exploration shows collections are cloned/iterated; capture `let (cid, cname) = (c.id, c.name.clone());` at the top of the row if needed).

- [ ] **Step 2: Build + clippy.**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: Commit.**
```bash
git add ferrolite-app/src/library/panel.rs
git commit -m "feat(app): drop images onto a collection row to add them"
```

---

## Task 3: Tag rows as drop targets (add-only)

**Files:**
- Modify: `ferrolite-app/src/state.rs`
- Modify: `ferrolite-app/src/library/panel.rs`

**Interfaces:**
- Produces:
  - `AppState::add_tag_to_images(&mut self, ctx: &egui::Context, ids: &[i64], tag_id: TagId)` — applies the tag to every id that does NOT already have it (add-only), reusing `apply_metadata_edit_to_ids(ctx, &missing, MetaEdit::ToggleTag(tag_id))`.
  - `fn ids_missing_tag(ids: &[i64], tag_id: TagId, visible_tags: &HashMap<i64, Vec<TagId>>) -> Vec<i64>` — pure filter, unit-tested.
- Consumes: `MetaEdit::ToggleTag(TagId)` (metadata.rs:15); `AppState::apply_metadata_edit_to_ids` (state.rs:329); `AppState::visible_tags: HashMap<i64, Vec<TagId>>` (state.rs).

Rationale: dragging images onto a tag means "tag these," i.e. **add** the tag. `ToggleTag` alone would *remove* it from images that already have it. So we filter to images lacking the tag first, then toggle-on only those — reusing the existing persist path with no new `MetaEdit` variant.

- [ ] **Step 1: Write the failing test for the pure filter.** In `state.rs`'s test module (or a small `#[cfg(test)]` near the helper), add:
```rust
    #[test]
    fn ids_missing_tag_filters_those_already_tagged() {
        use ferrolite_image::TagId;
        let t = TagId(7);
        let other = TagId(9);
        let mut vt: std::collections::HashMap<i64, Vec<TagId>> = std::collections::HashMap::new();
        vt.insert(1, vec![t]);        // already has t
        vt.insert(2, vec![other]);    // has a different tag
        vt.insert(3, vec![]);         // untagged
        // id 4 absent from the map → treated as missing the tag
        let got = super::ids_missing_tag(&[1, 2, 3, 4], t, &vt);
        assert_eq!(got, vec![2, 3, 4]);
    }
```
(Confirm `TagId`'s constructor/tuple field against `ferrolite-image` — the exploration notes `TagId` is a newtype over `i64` with inner `.0`.)

- [ ] **Step 2: Implement the pure filter + the add-only method.** In `state.rs`:
```rust
/// Images (in input order) that do NOT already carry `tag_id`. Images absent
/// from `visible_tags` are treated as missing the tag (so they get it).
pub(crate) fn ids_missing_tag(
    ids: &[i64],
    tag_id: ferrolite_image::TagId,
    visible_tags: &std::collections::HashMap<i64, Vec<ferrolite_image::TagId>>,
) -> Vec<i64> {
    ids.iter()
        .copied()
        .filter(|id| {
            visible_tags
                .get(id)
                .map(|tags| !tags.contains(&tag_id))
                .unwrap_or(true)
        })
        .collect()
}
```
And in `impl AppState`:
```rust
    /// Add `tag_id` to every image in `ids` that doesn't already have it
    /// (add-only; reuses the toggle path so persistence is unchanged).
    pub fn add_tag_to_images(
        &mut self,
        ctx: &egui::Context,
        ids: &[i64],
        tag_id: ferrolite_image::TagId,
    ) {
        let missing = ids_missing_tag(ids, tag_id, &self.visible_tags);
        if missing.is_empty() {
            return;
        }
        self.apply_metadata_edit_to_ids(ctx, &missing, crate::metadata::MetaEdit::ToggleTag(tag_id));
    }
```

- [ ] **Step 3: Run the filter test.**

Run: `cargo test -p ferrolite-app state::` (or the specific test name)
Expected: `ids_missing_tag_filters_those_already_tagged` PASSES.

- [ ] **Step 4: Add drop handling to the tag row.** In `panel.rs` the tag row is a `ui.horizontal(|ui| { ... })` (line ~281) with `name_resp` at line ~319 and tag id `t.id` (type `TagId`). Mirror Task 2's collection drop block, but call the add-only tag method. After the tag row's `ui.horizontal(...)` returns:
```rust
    let row_rect = row_resp.rect;
    if let Some(dragged) =
        egui::DragAndDrop::payload::<crate::library::drag::DraggedImages>(ui.ctx())
    {
        if let Some(pointer) = ui.ctx().pointer_interact_pos() {
            if row_rect.contains(pointer) {
                ui.painter().rect_filled(
                    row_rect.expand2(egui::vec2(4.0, 1.0)),
                    3.0,
                    theme::ACCENT_BG_SEL,
                );
                if ui.input(|i| i.pointer.any_released()) {
                    let ids = dragged.0.clone();
                    let tag_id = t.id;
                    let tag_name = t.name.clone();
                    egui::DragAndDrop::take_payload::<crate::library::drag::DraggedImages>(ui.ctx());
                    state.add_tag_to_images(ctx, &ids, tag_id);
                    state.warning =
                        Some(format!("Tagged {} image(s) with \"{}\".", ids.len(), tag_name));
                }
            }
        }
    }
```
Note: `panel::show` must have an `egui::Context` in scope for `add_tag_to_images` — the exploration shows `panel::show(ui, state, ctx)` receives `ctx`. Use that `ctx`. Copy `t.id` / clone `t.name` before the mutable `state` call to satisfy the borrow checker (tags are iterated from `state.tags`, cloned per the existing pattern).

- [ ] **Step 5: Build + clippy + test.**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app`
Expected: clean + green.

- [ ] **Step 6: Commit.**
```bash
git add ferrolite-app/src/state.rs ferrolite-app/src/library/panel.rs
git commit -m "feat(app): drop images onto a tag row to apply the tag (add-only)"
```

---

## Final gate (before holding for the author's visual test)

- [ ] **Step 1:** `cargo fmt --check` — no diff.
- [ ] **Step 2:** `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- [ ] **Step 3:** `cargo test --workspace` — green (new pure tests: `ids_for_drag` ×3, `ids_missing_tag` ×1).
- [ ] **Step 4: STOP and hold for Jann's visual test:**
  - From the grid, drag a single unselected image onto a collection → it's added; onto a tag → it gets the tag.
  - Select several images, drag one of them → all selected land in the collection / get the tag (the chip shows the count).
  - Drop targets highlight only while hovered with a drag; dropping in empty space does nothing.
  - Dragging an image already carrying a tag onto that tag is a no-op for it (add-only), and doesn't remove the tag.
  - Normal click / ctrl-click / shift-click selection still works (drag didn't break it).

---

## Self-Review (checked against the design + codebase)

**Coverage:** drag source on grid cells (Task 1) ✓; collection drop (Task 2) ✓; tag drop, add-only (Task 3) ✓; multi-selection payload ✓ (`ids_for_drag`); reuse of existing persist plumbing ✓ (`add_images_to_collection`, `apply_metadata_edit_to_ids`); no new DnD dependency ✓ (native `DragAndDrop`, manual detection like `queue_list.rs`).

**Placeholder scan:** the only "verify against egui 0.29.1" markers are the DnD method names (`has_payload_of_type`, `set_payload`, `take_payload`, `payload`) — all already used by `export_module/queue_list.rs` in this same crate/version, so they are known-good; the plan points the implementer at that file as the reference.

**Type consistency:** `DraggedImages(Vec<i64>)` is the single payload type across grid (set) and panel (take); `TagId` used consistently in `ids_missing_tag`/`add_tag_to_images`/the tag row; `add_images_to_collection(&[i64], i64)` and `apply_metadata_edit_to_ids(ctx, &[i64], MetaEdit)` match the mapped signatures.

**Reuse note:** this consumes the drag-source pattern the Export queue introduced; the payload differs (multi-id `DraggedImages` vs the queue's single `i64`), which is why it's a new payload type rather than shared infrastructure — matches the reuse note left in `queue_list.rs`.
