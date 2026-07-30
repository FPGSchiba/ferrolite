# Systematic UI Fixes Round 4 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the 11 author-approved Round-4 UX items from
`docs/superpowers/specs/2026-07-29-systematic-ui-fixes-round-4-design.md` (crop overhaul
excluded — separate spec).

**Architecture:** All items are localized UI/plumbing changes in `ferrolite-app`, plus one
catalog-query extension in `ferrolite-catalog` (Task 5) that the filter UI tasks depend on.
Pure decision logic (defaults predicate, range clamp/snap, cycle check, filmstrip snap policy)
is extracted into unit-testable helpers.

**Tech Stack:** Rust, egui 0.29 / eframe, rusqlite (pinned 0.32 — do NOT bump), existing
`ferrolite-jobs` event patterns.

## Global Constraints

- Icons ONLY via `ferrolite-app/src/icons.rs` Phosphor aliases; never raw glyphs or hand-drawn
  `Painter` icons.
- Every new editable/filter control gets a per-control reset affordance (reuse
  `widgets::draw_reset_arrow` / the `EguiSlider` reset column visual language).
- Keybound controls show their key via `Keymap::hint(action)` in tooltips.
- Never block the UI thread; catalog writes go through the existing writer/job paths.
- App-state tests are hermetic: `state.settings = Settings::default()` right after
  `AppState::new()`.
- Scoped gate per task: `cargo fmt -p <crate> -- --check`, `cargo clippy -p <crate>
  --all-targets -- -D warnings`, `cargo test -p <crate>` (plus `-p ferrolite-app` whenever
  `ferrolite-catalog` is touched). Commit per task with conventional-commit messages.
- rusqlite is PINNED at 0.32 (libsqlite3-sys 0.38 breaks on stable 1.92) — no dependency bumps.

---

### Task 1: REGION TONES section (spec D1)

**Files:**
- Modify: `ferrolite-app/src/develop/base_tabs.rs` (LightTab::show, ~lines 46-73; tests
  `test_all_eight_section_headers_bound_and_persist` ~line 796,
  `mask_scope_uses_its_own_section_flags` ~line 846)
- Modify: `ferrolite-app/src/develop/curve_widget.rs` (~line 162: remove the
  `curve_widget_parametric::show` call from the tone-curve body)
- Modify: `ferrolite-app/src/settings/dto.rs` (Settings struct + defaults)

**Interfaces:**
- Produces: `Settings.region_tones_open: bool` (default `true`) and
  `Settings.mask_region_tones_open: bool` (default `true`), following the existing
  `tone_curve_open`/`mask_tone_curve_open` pair pattern (serde defaults so old settings files
  load).
- `curve_widget_parametric::show(ui, scoped, tc)` keeps its signature; it is now called from
  `base_tabs.rs`, which must fetch `tc` = the scoped set's `tone_curve` clone the same way
  `curve_widget::show` resolves its set (via `scoped.set()`; on `None` render the
  `MASK_NONE_HINT` faint label exactly like `curve_widget::show` does).

- [ ] **Step 1: failing test.** In `base_tabs.rs` tests, extend
  `test_all_eight_section_headers_bound_and_persist`'s flag list with `region_tones_open` and
  `mask_region_tones_open` (rename the test to
  `test_all_section_headers_bound_and_persist` — the "eight" is already wrong at nine). Run:
  `cargo test -p ferrolite-app section_headers` — expect FAIL (unknown fields).
- [ ] **Step 2: settings fields.** Add both fields to `Settings` (serde `#[serde(default =
  "default_true")]` matching the existing section-flag pattern in `settings/dto.rs`).
- [ ] **Step 3: move the widget.** Remove the `curve_widget_parametric::show` call from
  `curve_widget.rs` (delete its `param_out` block, ~lines 161-167, keeping `out` handling
  intact). In `base_tabs.rs` `LightTab::show`, directly below the TONE CURVE section block,
  add a new section using the exact existing pattern:

```rust
let open = if scope_is_mask {
    &mut state.settings.mask_region_tones_open
} else {
    &mut state.settings.region_tones_open
};
section_header(ui, "REGION TONES", open);
if *open {
    ui.label(
        egui::RichText::new(
            "Region-based curve tones — complements the Basic sliders, \
             which weight by pixel brightness.",
        )
        .color(theme::TEXT_FAINT)
        .size(11.0_f32),
    );
    ui.add_space(4.0_f32);
    match scoped.set() {
        Some(set) => {
            let tc = set.tone_curve.clone();
            if let Some(o) = curve_widget_parametric::show(ui, &scoped, &tc) {
                out = Some(o);
            }
        }
        None => {
            ui.label(egui::RichText::new(scope::MASK_NONE_HINT).color(theme::TEXT_FAINT));
        }
    }
}
```

  (Adapt variable names to the surrounding function; `curve_widget.rs`'s existing
  `!param_out.commit → scoped.adjusting` handling moves with the call site.)
- [ ] **Step 4:** `cargo test -p ferrolite-app` — the extended persistence test and the full
  suite pass.
- [ ] **Step 5: Commit** `feat(develop): parametric H/S/W/B gets its own REGION TONES section`

### Task 2: Right gutter / scrollbar offset (spec D2)

**Files:**
- Modify: `ferrolite-app/src/app.rs` (~lines 2119-2167, the `develop_adjust` SidePanel)

The dead gutter is the stacked right inset: outer `Frame` right margin 24 + inner `Frame`
right margin 16, while the scrollbar (bar_width 10) floats inside the ScrollArea. Target: one
consistent ~8px visual padding between content and scrollbar, scrollbar hugging the panel
edge.

- [ ] **Step 1:** Change the outer Frame right margin 24→8 and the inner Frame right margin
  16→8 (keep left/top/bottom). Build and eyeball via `cargo run` if convenient; the change is
  visual-only.
- [ ] **Step 2:** `cargo clippy -p ferrolite-app --all-targets -- -D warnings` +
  `cargo test -p ferrolite-app`.
- [ ] **Step 3: Commit** `fix(develop): scrollbar hugs the adjustments panel edge (kill the
  dead right gutter)`

### Task 3: Info pill docks bottom-left when overlay hidden (spec D3)

**Files:**
- Modify: `ferrolite-app/src/develop/info_overlay.rs` (`draw_toggle_button`, ~line 50)
- Test: same file (position-selection helper test)

Today the pill always floats 132px above the bottom edge (where the overlay's top edge sits
when shown). Wanted: overlay hidden ⇒ pill anchors at the canvas bottom-left (the overlay's
own `MARGIN`=12 corner position); overlay shown ⇒ current position.

- [ ] **Step 1: failing test.** Extract a pure helper and test it:

```rust
/// Y-offset (up from the canvas bottom edge) of the info pill's anchor.
/// Overlay visible: sit above the overlay box. Hidden: sit at the corner margin.
pub(crate) fn pill_bottom_offset(overlay_visible: bool, overlay_height: f32) -> f32 {
    if overlay_visible {
        overlay_height + 2.0 * MARGIN
    } else {
        MARGIN
    }
}

#[test]
fn pill_docks_to_corner_when_overlay_hidden() {
    assert_eq!(pill_bottom_offset(false, 120.0), MARGIN);
    assert!(pill_bottom_offset(true, 120.0) > 120.0);
}
```

  Run `cargo test -p ferrolite-app pill_docks` — FAIL (helper missing).
- [ ] **Step 2:** Implement the helper; make `draw_toggle_button` position the pill's `Area`
  using it (it already receives `show_info_panel`; measure/estimate the overlay height the
  same way the current 132px constant was derived — replace the magic 132 with the helper).
- [ ] **Step 3:** `cargo test -p ferrolite-app` green.
- [ ] **Step 4: Commit** `fix(develop): info pill docks bottom-left while the overlay is
  hidden`

### Task 4: Subfolders toggle → Folders tree header (spec L1)

**Files:**
- Modify: `ferrolite-app/src/library/toolbar.rs` (~line 90: remove the checkbox)
- Modify: `ferrolite-app/src/library/panel.rs` (~line 51: the "FOLDERS" header label)

- [ ] **Step 1:** Remove the `ui.checkbox(&mut state.include_subfolders, "Subfolders")` from
  the toolbar.
- [ ] **Step 2:** In `panel.rs`, replace the plain `colored_label(.., "FOLDERS")` with a
  horizontal row: the label left, and right-aligned (via
  `ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), ..)`) a small checkbox
  bound to `state.include_subfolders` labeled `Subfolders`, with
  `.on_hover_text("Include images in subfolders")`. Match the header's faint styling.
- [ ] **Step 3:** `cargo test -p ferrolite-app`; grep to confirm no other "Subfolders" site
  remains in the toolbar.
- [ ] **Step 4: Commit** `feat(library): Subfolders toggle lives with the Folders tree, not
  the filters`

### Task 5: Wire lens / file-type / aperture / focal into the catalog query (enabler)

**Files:**
- Modify: `ferrolite-catalog/src/query.rs` (`LibraryQuery` ~lines 52-62 + SQL assembly)
- Modify: `ferrolite-app/src/library/filter.rs` (`to_query` ~lines 108-154;
  `FilterState.file_type` becomes a set — see Task 8 interface below)
- Test: `ferrolite-catalog` query tests + `filter.rs` tests

**Interfaces:**
- Produces on `LibraryQuery`: `lens: Option<String>`, `file_types:
  Vec<ferrolite_image::FileKind-like discriminant already used for the images table>` (match
  however the images table stores kind/extension — inspect the schema; if the table stores an
  extension or a kind integer, filter on that column with an `IN (…)` clause),
  `aperture: Option<(f32, f32)>`, `focal: Option<(f32, f32)>` — each translating to a SQL
  predicate exactly like the existing `iso: Option<(u32,u32)>` (BETWEEN min AND max on the
  metadata columns). All parameterized — NO string-formatted SQL.
- `FilterState.file_type: Option<FileTypeChip>` CHANGES to `file_types:
  std::collections::BTreeSet<FileTypeChip>` (empty = all). `to_query` maps every populated
  filter; nothing silently dropped anymore.

- [ ] **Step 1: failing tests.** In `ferrolite-catalog`'s query tests, add cases: a lens-name
  filter matches only rows with that lens; a two-entry file-type set matches both kinds and
  excludes a third; aperture/focal `(min,max)` include boundary values and exclude outside
  values. In `filter.rs` tests: `to_query` forwards `lens`, `file_types`, `aperture`, `focal`
  verbatim. Run both suites — FAIL.
- [ ] **Step 2:** Implement `LibraryQuery` fields + SQL (`IN` list built with one `?`
  placeholder per entry; ranges as `BETWEEN ? AND ?`, skipping rows whose metadata column is
  NULL).

> **AMENDMENT (author-approved after the initial BLOCKED report — the schema has no
> lens/aperture/focal columns and `kind` is 2-way):**
> 1. **Schema v7 migration**: add `lens TEXT`, `aperture REAL`, `focal_length REAL`
>    (nullable) to `images`, following the existing versioned-migration pattern in
>    `ferrolite-catalog/src/schema.rs` (SCHEMA_VERSION 6 → 7).
> 2. **Ingest persists them**: `NewImage::from_metadata`
>    (`ferrolite-catalog/src/model.rs:105-132`) copies `lens`, `aperture`,
>    `focal_length` from `ferrolite_decode::Metadata`; insert/update SQL carries the new
>    columns.
> 3. **File type = path extension**: no schema change. The SQL predicate matches on the
>    lower-cased path extension; `FileTypeChip` becomes `{ Raw, Jpeg, Png, Tiff }`
>    (HEIC dropped — not ingestable). `Raw` maps to the raw-extension list `scan.rs`
>    accepts; `Jpeg` = `jpg`/`jpeg`; `Png` = `png`; `Tiff` = `tif`/`tiff`.
> 4. Backfill of pre-v7 rows is **Task 14** (separate) — do NOT implement it here.
- [ ] **Step 3:** Update `to_query` to forward everything; fix `FilterState` field fallout
  (compile errors in toolbar.rs are expected — leave the toolbar rendering single-choice for
  now by selecting into/out of the set for the current chip; Task 8 replaces the UI).
- [ ] **Step 4:** `cargo test -p ferrolite-catalog && cargo test -p ferrolite-app` green;
  scoped clippy/fmt on both crates.
- [ ] **Step 5: Commit** `feat(catalog): lens/file-type/aperture/focal filters actually reach
  the query`

### Task 6: Dual-handle range-slider widget (enabler for spec L3)

**Files:**
- Create: `ferrolite-app/src/widgets/range_slider.rs`
- Modify: `ferrolite-app/src/widgets/mod.rs` (export)

**Interfaces:**
- Produces:

```rust
pub struct RangeSlider<'a> {
    pub label: &'static str,
    pub lo: &'a mut f32,
    pub hi: &'a mut f32,
    pub min: f32,
    pub max: f32,
    /// Detents to snap handles to (sorted ascending, includes min & max).
    pub detents: &'a [f32],
    /// Log-scaled track position when true (ISO/aperture), linear otherwise.
    pub log: bool,
    pub decimals: usize,
    pub unit: &'static str,
}
impl<'a> egui::Widget for RangeSlider<'a> { /* … */ }

/// Pure: snap `v` to the nearest detent, then clamp so lo <= hi holds for the
/// handle being moved (`moving_lo`).
pub fn snap_and_clamp(v: f32, detents: &[f32], other: f32, moving_lo: bool) -> f32;
/// Pure: track fraction for value `v` on [min,max], log or linear.
pub fn track_fraction(v: f32, min: f32, max: f32, log: bool) -> f32;
```

- Visuals follow `EguiSlider` (`widgets/slider.rs`): same label column width, track height,
  fill between the two handles, value readout `"{lo}–{hi}{unit}"`, and the same reset-arrow
  column (reset ⇒ `lo = min, hi = max`). Reuse `slider.rs`'s `math` helpers where they fit.

- [ ] **Step 1: failing tests** for the pure helpers: snapping picks the nearest detent;
  moving the lo handle above hi clamps to hi (and vice versa); `track_fraction` is 0 at min, 1
  at max, and monotone; log mode: `track_fraction(100, 50, 102400, true)` ≈ `(ln 100 − ln 50)
  / (ln 102400 − ln 50)`. Run — FAIL (module missing).
- [ ] **Step 2:** Implement helpers, then the widget: two draggable handles hit-tested by
  nearest-handle-to-pointer, drag updates through `snap_and_clamp`, `mark_changed` on
  movement, reset column matching `EguiSlider`.
- [ ] **Step 3:** Tests green; scoped gate on `ferrolite-app`.
- [ ] **Step 4: Commit** `feat(widgets): dual-handle RangeSlider (log/linear, detent snapping,
  per-control reset)`

### Task 7: Metadata popup uses range sliders (spec L3)

**Files:**
- Modify: `ferrolite-app/src/library/toolbar.rs` (~lines 286-351, the three sliders)

**Interfaces:**
- Consumes `widgets::RangeSlider` (Task 6) and the Task-5 query wiring.
- Bounds/detents (exact values):
  - ISO: min 50, max 102_400, log track, detents = full stops
    `[50, 100, 200, 400, 800, 1600, 3200, 6400, 12800, 25600, 51200, 102400]`.
  - Aperture: min 0.7, max 32.0, log track, detents = the standard third-stop series
    `[0.7, 0.8, 0.9, 1.0, 1.1, 1.2, 1.4, 1.6, 1.8, 2.0, 2.2, 2.5, 2.8, 3.2, 3.5, 4.0, 4.5,
    5.0, 5.6, 6.3, 7.1, 8.0, 9.0, 10.0, 11.0, 13.0, 14.0, 16.0, 18.0, 20.0, 22.0, 25.0, 29.0,
    32.0]`, 1 decimal.
  - Focal: min 8.0, max 1200.0, linear track, detents = every 1 mm (generate with
    `(8..=1200).map(|v| v as f32)` into a cached `Vec`), 0 decimals, unit `" mm"`.
- Filter mapping: handles at FULL range ⇒ store `None` (filter inactive); anything narrower ⇒
  `Some((lo, hi))` (ISO as `(u32, u32)`). The per-control reset restores full range ⇒ `None`.

- [ ] **Step 1:** Replace the three `EguiSlider` blocks with `RangeSlider`s per the table;
  map to/from the `FilterState` options as specified (read `None` as full-range handles).
- [ ] **Step 2:** Add a `filter.rs` unit test: a full-range tuple never reaches the query
  (state maps to `None`), a narrowed range does.
- [ ] **Step 3:** Scoped gate on `ferrolite-app`.
- [ ] **Step 4: Commit** `feat(library): ISO/aperture/focal become real min-max range filters`

### Task 8: Multi-select, resettable file-type chips (spec L4)

**Files:**
- Modify: `ferrolite-app/src/library/toolbar.rs` (~lines 256-269) and
  `ferrolite-app/src/library/filter.rs` (`FileTypeChip`, `file_types` set — struct change
  already landed in Task 5)
- Check: `ferrolite-app/src/widgets/chips.rs` (`SegmentedControl` — add a multi-select mode
  or render the chips as individual toggle chips with the same styling)

- [ ] **Step 1: failing test** in `filter.rs`: toggling a chip in/out of `file_types` behaves
  as a set; empty set ⇒ `to_query` sends no file-type predicate.
- [ ] **Step 2:** Render each `FileTypeChip` variant as an independently toggleable chip
  (selected = in the set). Keep the existing chip visual language. Empty set is the "all"
  state; add a small per-control reset arrow beside the chip row that clears the set (only
  enabled when non-empty).
- [ ] **Step 3:** Scoped gate on `ferrolite-app`.
- [ ] **Step 4: Commit** `feat(library): file-type filter is multi-select and resettable`

### Task 9: Reset-all-filters button (spec L2)

**Files:**
- Modify: `ferrolite-app/src/library/filter.rs` (add `is_default`, `reset_all`)
- Modify: `ferrolite-app/src/library/toolbar.rs` (button at the end of the filter cluster)
- Modify (if needed): `ferrolite-app/src/icons.rs` (alias `RESET_FILTERS` →
  `phosphor::ARROW_COUNTER_CLOCKWISE` — reuse the existing reset alias if one exists)

**Interfaces:**

```rust
impl FilterState {
    /// True when every user-facing filter is at its default (search empty, no rating/flag
    /// constraint, no tags, no metadata filters, empty file_types). Sort order does NOT
    /// count — it is a view preference, not a filter.
    pub fn is_default(&self) -> bool;
    /// Reset every user-facing filter to default; leaves sort_key/sort_desc untouched.
    pub fn reset_all(&mut self);
}
```

- [ ] **Step 1: failing tests:** fresh state `is_default()`; each individual filter (search
  text, min_rating, a flag, a tag, camera, lens, iso, aperture, focal, a file-type chip)
  flips it false; `reset_all` restores `is_default()` while preserving a non-default sort.
- [ ] **Step 2:** Implement both; add the toolbar button (tool-button styling, icon from
  `icons.rs`), `add_enabled(!state.filter.is_default(), …)`, hover text "Reset all filters"
  / disabled hover "All filters are at default". Clicking calls `reset_all` and re-runs the
  query the same way the other filter widgets do.
- [ ] **Step 3:** Scoped gate on `ferrolite-app`.
- [ ] **Step 4: Commit** `feat(library): one-click reset for every filter`

### Task 10: Collection hierarchy drag-and-drop (spec L5)

**Files:**
- Modify: `ferrolite-app/src/library/panel.rs` (collection tree rows, ~line 202+; the
  root-header unparent drop at ~176-201 already exists — reuse its pattern)
- Modify: `ferrolite-app/src/library/drag.rs` (collection drag payload)
- Test: `panel.rs` or a new `library/collection_tree.rs` module if panel.rs would exceed ~800
  lines — cycle-check unit tests

**Interfaces:**

```rust
/// Payload for dragging a collection row (distinct from DraggedImages).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DraggedCollection(pub i64);

/// Pure: true if making `dragged` a child of `target` would create a cycle
/// (target is dragged itself or one of dragged's descendants) given the
/// child→parent map.
pub fn would_create_cycle(
    dragged: i64,
    target: i64,
    parent_of: &std::collections::HashMap<i64, Option<i64>>,
) -> bool;
```

- [ ] **Step 1: failing tests** for `would_create_cycle`: self-drop is a cycle; dropping onto
  own child/grandchild is a cycle; dropping onto a sibling or unrelated node is not; dropping
  onto its current parent is NOT a cycle (it's a no-op move — allowed).
- [ ] **Step 2:** Make collection rows draggable with `DraggedCollection` payload (reuse
  `drag.rs`'s drag-chip pattern); make collection rows drop targets that accept
  `DraggedCollection` (in addition to the existing image payload). On release: if
  `would_create_cycle` ⇒ no write, flash the target row red for ~0.6s (store a
  `(row_id, until: f64)` in the panel's egui temp data using `ui.input(|i| i.time)`, painted
  as a red-tinted row fill while active — no timers, no threads); else call the existing
  `writer.update_collection_parent(dragged, Some(target))` path (same as the root-header
  unparent flow, which stays as-is).
- [ ] **Step 3:** Verify drop-target highlight renders via the same `ACCENT_BG_SEL` fill
  `drag.rs::row_drop_target` uses.
- [ ] **Step 4:** Scoped gate on `ferrolite-app` (+`ferrolite-catalog` if its API needed any
  touch — it should NOT; `update_collection_parent` exists).
- [ ] **Step 5: Commit** `feat(library): drag collections onto collections to nest them
  (cycle-safe)`

### Task 11: Filmstrip free-scroll (spec N1)

**Files:**
- Modify: `ferrolite-app/src/library/filmstrip.rs` (~lines 146-151)
- Test: same file

Today `scroll_to_rect(.., Align::Center)` fires EVERY frame for the current image — that is
the snap-back. Wanted: center only when the selection CHANGES (nav key, click, programmatic
open), never while the user free-scrolls.

**Interfaces:**

```rust
/// Pure: should this frame auto-center the strip on `current`?
/// Centers exactly once per selection change.
pub(crate) fn should_center(current: Option<i64>, last_centered: Option<i64>) -> bool {
    current.is_some() && current != last_centered
}
```

- [ ] **Step 1: failing tests:** same id twice ⇒ false; changed id ⇒ true; `None` current ⇒
  false; None→Some ⇒ true.
- [ ] **Step 2:** Track `last_centered: Option<i64>` (in `AppState`'s existing
  filmstrip/canvas UI-state struct, or egui temp data keyed by the strip id — prefer the
  state struct for testability). Call `scroll_to_rect` only when `should_center(..)`, then
  record `last_centered = current`.
- [ ] **Step 3:** Scoped gate on `ferrolite-app`.
- [ ] **Step 4: Commit** `fix(develop): filmstrip free-scrolls; snaps only on navigation`

### Task 12: Titlebar active-module underline (spec N2)

**Files:**
- Modify: `ferrolite-app/src/widgets/tabs.rs` (`TabRow` active styling)
- Check: `ferrolite-app/src/chrome/mod.rs` tests (~328-390) still pass

- [ ] **Step 1:** In `TabRow`'s draw for the active tab, add a 2px accent-colored underline
  rule (theme accent color, inset ~6px from the label's left/right edges, drawn at the tab
  rect's bottom) per the V2 design. Keep the existing active-tab text styling.
- [ ] **Step 2:** `cargo test -p ferrolite-app chrome` + full scoped gate.
- [ ] **Step 3: Commit** `feat(chrome): active module tab gets the V2 underline accent`

### Task 13: Global keybind column alignment (spec N3)

**Files:**
- Modify: `ferrolite-app/src/settings/ui/keyboard.rs` (~lines 162-231)

egui `Grid` auto-sizes per group, so each section aligns independently. Fix: compute ONE
global label-column width and impose it on every row.

- [ ] **Step 1:** Before rendering groups, compute
  `let label_w = GROUPS.iter().flat_map(|(_, actions)| actions.iter()).map(|a|
  ui.fonts(|f| /* galley width of the action's label at the row font */)).fold(0.0, f32::max)`
  (use the same `FontId` `draw_row` uses for the label; round up + a small pad).
- [ ] **Step 2:** In `draw_row`, render the label via `ui.add_sized([label_w, ROW_H],
  egui::Label::new(..).halign(left))` (or set the Grid's first-column `min_col_width` to
  `label_w` on every group's Grid) so all sections share the width.
- [ ] **Step 3:** Scoped gate on `ferrolite-app`.
- [ ] **Step 4: Commit** `fix(settings): keybind labels align to one global column across
  sections`

### Task 14: Metadata backfill job for pre-v7 catalog rows (author-approved amendment)

**Files:**
- Create: `ferrolite-app/src/library/meta_backfill.rs` (job spawn + batching)
- Modify: `ferrolite-catalog` (a query listing image `(id, path)` where `lens IS NULL AND
  aperture IS NULL AND focal_length IS NULL`, and an update setting the three columns)
- Modify: `ferrolite-app/src/events.rs` + the event apply path (one new event delivering a
  batch of backfilled rows)
- Modify: `ferrolite-app/src/app.rs` or `state.rs` (one-shot spawn after catalog open)

**Interfaces:**
- Consumes: the existing EXIF read used by `develop/meta_read.rs`
  (`ferrolite_decode` metadata request) and the Task-5 columns.
- Produces: `spawn_meta_backfill(jobs, catalog/read_pool, tx, ctx) -> JobHandle` — a
  Background-priority, cancellable job that walks NULL-metadata rows in batches (e.g. 64),
  reads EXIF per file (skipping unreadable/missing files permanently by writing an empty
  string / keeping NULL — decide and document), sends one event per batch; the UI-thread
  handler writes the batch through the catalog writer and bumps `state.dirty` ONCE per batch
  so active metadata filters refresh.

- [ ] **Step 1: failing test** in ferrolite-catalog: the NULL-metadata listing returns
  exactly the rows with all three columns NULL; the batch update sets them and removes rows
  from the listing.
- [ ] **Step 2:** Implement the catalog queries; then the job (Background priority,
  cancellation token honored between files; never on the UI thread) and the event plumbing.
  One-shot spawn per app run, after the catalog opens, only if the NULL-count is > 0.
- [ ] **Step 3:** Scoped gate on `ferrolite-catalog` + `ferrolite-app`.
- [ ] **Step 4: Commit** `feat(catalog): background EXIF backfill fills lens/aperture/focal
  for pre-v7 rows`

---

## Post-plan (coordinator, not a task)

- Repo gate on latest stable (`rustup update stable` first) once all tasks land.
- Author visual test checklist for the round.
- Fold accepted changes into `docs/design/V2/README.md`.
