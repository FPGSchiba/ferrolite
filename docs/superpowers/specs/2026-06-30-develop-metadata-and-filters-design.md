# ferrolite — Spec 1.6: Develop metadata & filters (design)

> **Status:** Design — approved by user; pending writing-plans.
> **Date:** 2026-06-30
> **Parent:** `2026-06-30-tags-and-filters-design.md` (Spec 1.5 — the Library tags/ratings/
> flags/collections model + toolbar this extends) and `2026-06-28-ferrolite-v1-architecture-map.md`
> (§5 cross-cutting contracts). Read those first.
> **Proves:** brings Spec 1.5's organizing model (rating / flag / tags / collections) and
> filtering into the **Develop module**, optimized for a fast cull-while-viewing workflow.
> **Branch:** continues on `feat/tags-and-filters` (same feature family).
> **Out of scope:** all edit ops / the adjustment-panel UI (Spec 2); filmstrip multi-select;
> search + the metadata-range popover inside Develop (stay in Library for those).

---

## 1. Goal

The Develop module currently shows the filmstrip + the loupe viewer with ←/→ navigation, but
offers no way to organize the image you're looking at and no filter controls. This slice adds a
**fast cull-while-viewing** workflow: rate/flag/tag the open image (keyboard-first), add it to a
collection, and re-filter/sort the navigated set — all without leaving Develop.

> open an image → rate/flag/tag it (keys or a compact bar) → see cull state on the filmstrip →
> re-filter the strip → ←/→ to the next image. Esc still returns to Library.

A key enabler already holds: `state.images` *is* the filtered/sorted `LibraryQuery` result, so the
filmstrip already reflects the active filter/source/sort. This slice adds the **controls** and the
**editing affordances**, reusing Spec 1.5's plumbing (`apply_metadata_edit`,
`add_selection_to_collection`, the toolbar filter widgets, the grid context menu, the `icons`
drawing helpers).

---

## 2. Scope

**In:** a compact filter strip in the Develop top bar; rating/flag overlays on filmstrip thumbs;
a bottom current-image metadata bar (rating, flag, tags, add-to-collection); Develop keyboard
metadata shortcuts targeting the open image; right-click context menu on the loupe image and
filmstrip thumbs; navigation when the open image is filtered out of the set.

**Out:** edit ops / adjustment panel (Spec 2); filmstrip multi-selection and batch edits (Develop
edits are **current-image only**); search field + metadata-range popover inside Develop; any change
to the persistence model (rating→XMP, the rest→SQLite — unchanged from Spec 1.5).

---

## 3. Layout

```
┌──────────────────────── title bar ────────────────────────┐
│ ★≥ filter · ⚑ ⚐ · Tags ▾ · Sort ▾ ↕        (filter row, ~28px)
│ [ filmstrip: thumbs w/ rating+flag overlays, current lit ] │   top panel
├────────────────────────────────────────────────────────────┤
│                  viewer  (loupe zoom/pan)                   │   central
├────────────────────────────────────────────────────────────┤
│ ★★★☆☆(click)   ⚑ Pick   ⚐ Reject   Tags ▾   + Collection ▾ │   bottom bar (~34px)
└──────────────────────── status bar ───────────────────────┘
Keys (Develop, no text focus): 0–5 = rating · P / X / U = flag → the open image
```

The Develop top panel grows from one row (filmstrip only) to two: a slim **filter row** above the
existing filmstrip. A new **bottom bar** (shown only in Develop with a viewer open) carries the
current-image metadata controls. The central viewer is unchanged.

All icons are **drawn shapes** (the `icons` module — `star`, `flag`, `caret`, `rating_stars`),
never font glyphs, consistent with the Spec 1.5 visual-fix round.

---

## 4. Components

### 4.1 Develop filter strip (`ferrolite-app/src/library/develop_filter_bar.rs`, new)
A compact row reusing the same `state.filter` fields and widgets as the Library toolbar:
rating-threshold stars, flag-filter toggles, Tags dropdown (+ Any/All), and the Sort combo +
direction caret. Returns `changed`; the caller sets `state.dirty` so the read pool re-queries and
the filmstrip refreshes. **Excluded** (kept in Library): the search field and the metadata-range
popover. The shared widget logic (e.g. the star-threshold row, flag toggles, tag dropdown) is
factored so the Library toolbar and this strip don't duplicate it.

### 4.2 Filmstrip overlays (`library/filmstrip.rs`)
Each filmstrip thumbnail draws a compact rating (small `icons::rating_stars`) and flag
(`icons::flag`) overlay from its `ImageRecord`, plus the existing accent outline on the current
image. The filmstrip already iterates `state.images` (which carry `rating`/`flag`); tag dots are
optional and may be omitted to keep small thumbs legible. Stays virtualized (visible cells only,
unchanged).

### 4.3 Current-image metadata bar (`ferrolite-app/src/develop/metadata_bar.rs`, new)
A bottom `TopBottomPanel` rendered only when `module == Develop && viewer.is_some()`. For the open
image (`viewer.image_id`):
- **Rating:** five drawn stars; click star N to set rating N; click the active rating to clear (0).
- **Flag:** Pick / Reject buttons (drawn flag icons) that toggle the open image's flag.
- **Tags:** a dropdown listing the global vocabulary with a checkmark per tag the image has;
  clicking toggles it.
- **Add to collection:** a menu listing collections; clicking adds the open image.
It reflects the open image's live state (read from its `ImageRecord` + `visible_tags`).

### 4.4 Keyboard shortcuts in Develop (`ferrolite-app/src/app.rs`)
Enable `0`–`5` (rating) and `P` / `X` / `U` (flag) when `module == Develop`, a viewer is open, and
no text field holds focus. They target the **open viewer image**, not the grid selection. This
requires a targeting entry point (see §5).

### 4.5 Right-click context menu (loupe + filmstrip)
Attach the existing context menu (Rating / Flag / Tags / Add-to-collection — the grid's
`paint_cell` menu) to the loupe image response and to filmstrip thumbnails, scoped to that image.
Extracted into a reusable helper so the grid and Develop share one implementation.

---

## 5. Targeting the open image (data flow)

Spec 1.5's `AppState::apply_metadata_edit(ctx, edit)` operates on `state.selection` (fallback
`state.selected`). Develop edits must hit `viewer.image_id` regardless of grid selection. Add:

- `AppState::apply_metadata_edit_to_image(&mut self, ctx, image_id: i64, edit: MetaEdit)` — the
  single-image core (optimistic in-memory update of that image's `ImageRecord` row + its
  `visible_tags`, then the off-thread `spawn_metadata_write` persist: SQLite for all axes + the
  `xmp:Rating` sidecar for rating). The existing selection-based `apply_metadata_edit` is
  refactored to resolve its target set and delegate per id, so both share one path (DRY).
- `AppState::add_image_to_collection_now(&mut self, image_id, coll_id)` — single-image collection
  add (the bottom bar / context menu use this; the existing `add_selection_to_collection` can
  delegate to it).

Optimistic update + off-thread persist + the `MetadataResult` event (revert-on-DB-failure,
warn-on-sidecar-failure) are unchanged from Spec 1.5 — the filmstrip overlay and bottom bar reflect
the in-memory change immediately, the job persists, `ctx.request_repaint()` follows. Nothing slow
runs on the UI thread (CLAUDE.md §1).

---

## 6. Navigation when the open image is filtered out

A filter change re-runs the query; the open image may no longer be in `state.images`. Behavior:
- **Keep showing** the open image (do not auto-close or jump) — the user is still looking at it.
- **←/→** navigate within the *new* set: a pure helper
  `neighbor_in_set(images: &[ImageRecord], current_id, step) -> Option<i64>` returns the next/prev
  image id, and when `current_id` is absent from the set, falls back to the first (Next) / last
  (Prev) entry. Clamped (non-cyclic), mirroring the existing `viewer::nav::neighbor_index`.
- This helper is a **pure, unit-tested function**.

---

## 7. Error handling

- Empty filtered set in Develop → filmstrip is empty; ←/→ are no-ops; the open image stays shown.
  No panic.
- Metadata write failures behave exactly as Spec 1.5 (`MetadataResult`): DB failure → reload truth
  (the bottom bar/overlay revert on the next refresh); sidecar (rating) failure → a status-bar
  warning, DB value kept.
- A read-only directory (cannot write `xmp:Rating`) surfaces the existing warning; the rating still
  updates in SQLite so the UI stays consistent.

---

## 8. Testing (TDD where logic exists; egui rendering via build + visual)

- `neighbor_in_set` — pure: next/prev within a set, current-absent fallback to first/last, empty
  set, single element, clamping at ends.
- `apply_metadata_edit_to_image` — `AppState::for_test()`: a rating/flag/tag edit updates the
  targeted image's in-memory record / `visible_tags` (the persist job needs a real `ctx`, so the
  in-memory portion is the load-bearing assertion, as in Spec 1.5's H3/VF tests).
- Shared filter-widget factoring — the `FilterState`→`LibraryQuery` mapping is already covered
  (Spec 1.5 H1); the Develop strip drives the same fields, so no new query test is needed.
- egui rendering (filter strip, bottom bar, filmstrip overlays, context menu) — verified by
  `cargo build` + clippy + the author's visual test; no golden tests (no GPU pass here).
- Gate: `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
  `cargo test --workspace` green; then hold for the author's visual test (CLAUDE.md rule) before
  finishing.

---

## 9. Build order

1. Pure helpers: `neighbor_in_set` (tests first); refactor `apply_metadata_edit` →
   `apply_metadata_edit_to_image` + `add_image_to_collection_now` (tests).
2. Extract the shared filter widgets so the toolbar and the Develop strip reuse one implementation.
3. Develop filter strip in the top panel (above the filmstrip).
4. Filmstrip rating/flag overlays.
5. Bottom current-image metadata bar.
6. Develop keyboard shortcuts (0–5, P/X/U) → open image; ←/→ uses `neighbor_in_set`.
7. Shared right-click context-menu helper on the loupe image + filmstrip thumbs.
8. Gate green → hold for the author's visual test.

---

## 10. Decisions recorded (resolved during brainstorming, 2026-06-30)

| Question | Decision | Rationale |
|---|---|---|
| Primary workflow | **Fast cull-while-viewing** | Keyboard-first, compact controls, minimal panels — speed over a heavy management panel. |
| Filters in Develop | **Compact filter strip** above the filmstrip | Re-filter the navigated set in place; reuse toolbar widgets; search + metadata popover stay in Library. |
| Edit target | **Current (open) image only** | Predictable loupe/review semantics; no filmstrip multi-selection state to build. |
| Metadata-edit surface | **Bottom bar** (Lightroom loupe style) | Keeps the top for filmstrip + filters; conventional placement. |
| Filmstrip overlays | **Add rating + flag** (drawn icons) | At-a-glance cull feedback; tag dots optional for legibility. |
| Targeting | **`apply_metadata_edit_to_image(id, …)`**, existing selection path delegates to it | Edits hit the viewer image regardless of grid selection; DRY. |
| Branch | **Same `feat/tags-and-filters`** | Same Tags & Filters feature family, extended into Develop. |
