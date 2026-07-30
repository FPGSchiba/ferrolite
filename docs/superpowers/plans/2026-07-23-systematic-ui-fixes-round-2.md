# V2 Systematic UI Fixes Implementation Plan (Round 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the Round 2 systematic fixes: panel scrollbar clearance, responsive parametric curve preview, compact Lum sliders in 2x2 color grading grid, icon directive sweep (Phosphor font aliases), collection drag-and-drop re-parenting, persistent side panel width drag-resizing, and titlebar double border removal.

**Architecture:** Update `ferrolite-catalog` with `update_collection_parent`. Sweep `icons.rs` and codebase to replace raw symbol strings with `icons::*` Phosphor aliases. Set right inner margin `24.0` for `develop_adjust` SidePanel in `app.rs`. Make parametric curve preview fill `ui.available_width()`. Set `label_width(32.0)` on `EguiSlider` in `grade_widget.rs`. Capture `response.response.rect.width()` for `develop_info_panel` and `develop_adjust` side panels in `app.rs` and update `state.settings`. Remove duplicate titlebar background and border painting in `chrome/mod.rs`.

**Tech Stack:** Rust, `egui` 0.28, `ferrolite-app`, `ferrolite-catalog`.

## Global Constraints

- **Scope:** Changes in `ferrolite-catalog` and `ferrolite-app` crates.
- **Scoped Gate:**
  - `ferrolite-catalog`: `cargo test -p ferrolite-catalog`
  - `ferrolite-app`: `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`
- **Toolchain:** Compile cleanly on stable rustc without warnings (`-D warnings`).
- **Float-literal lint:** Suffix float literals in f32 context with `_f32`.
- **Per-Control Reset (CLAUDE.md):** Every parameter retains its individual reset arrow / double-click reset affordance.

---

## Task Breakdown

### Task 1: Catalog Collection Re-parenting API (`ferrolite-catalog`)

**Files:**
- Modify: `ferrolite-catalog/src/queries.rs`
- Modify: `ferrolite-catalog/src/catalog.rs`

**Interfaces:**
- Produces: `Catalog::update_collection_parent(&self, id: i64, parent_id: Option<i64>) -> Result<(), CatalogError>`.

- [ ] **Step 1: Write unit test in `catalog.rs` for `update_collection_parent`**
- [ ] **Step 2: Add SQL query `UPDATE collections SET parent_id = ? WHERE id = ?` in `queries.rs` and expose `update_collection_parent` in `catalog.rs`**
- [ ] **Step 3: Run scoped gate for catalog**
  `cargo test -p ferrolite-catalog`

---

### Task 2: Icon Directive Sweep & Phosphor Font Aliases (`src/icons.rs` & `src/widgets/`)

**Files:**
- Modify: `ferrolite-app/src/icons.rs`
- Modify: `ferrolite-app/src/widgets/mod.rs`
- Modify: `ferrolite-app/src/library/panel.rs`
- Modify: `ferrolite-app/src/develop/info_panel.rs`

**Interfaces:**
- Produces: `icons::CARET_RIGHT` alias; zero raw symbol characters (`▸`, `▾`, `ℹ`, `✕`) in IBM Plex text across app.

- [ ] **Step 1: Add `pub const CARET_RIGHT: &str = p::CARET_RIGHT;` to `src/icons.rs`**
- [ ] **Step 2: Update `section_header` in `widgets/mod.rs` to render `icons::CARET_RIGHT` and `icons::CARET_DOWN` with `icons::font(10.0)`**
- [ ] **Step 3: Update `library/panel.rs` and `info_panel.rs` to use `icons::CARET_RIGHT`, `icons::CARET_DOWN`, `icons::INFO`**
- [ ] **Step 4: Add unit test verifying icon font rendering in section_header**
- [ ] **Step 5: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 3: Collection Drag-and-Drop Re-parenting (`src/library/panel.rs`)

**Files:**
- Modify: `ferrolite-app/src/library/panel.rs`

**Interfaces:**
- Produces: Collection drag-and-drop re-parenting (dragging a collection onto a Collection Set assigns `parent_id = Some(set.id)`).

- [ ] **Step 1: Add `ui.dnd_set_drag_payload` with collection ID on collection row interaction**
- [ ] **Step 2: Add `ui.dnd_release_payload::<i64>` check on Collection Set header, calling `catalog.update_collection_parent(dropped_id, Some(set.id))` on drop**
- [ ] **Step 3: Add unit test in `panel.rs` for drag-and-drop re-parenting handling**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 4: Responsive Parametric Curve & Compact 2x2 Lum Sliders (`src/widgets/`)

**Files:**
- Modify: `ferrolite-app/src/widgets/curve.rs`
- Modify: `ferrolite-app/src/develop/grade_widget.rs`

**Interfaces:**
- Produces: Full-width responsive Parametric curve preview box; compact 32px label width for `Lum` sliders in 2x2 Color Grading grid.

- [ ] **Step 1: Update `curve.rs` to set parametric curve preview graph width to `ui.available_width()`**
- [ ] **Step 2: Update `grade_widget.rs` to set `.label_width(32.0)` on `EguiSlider` calls in 2x2 grid columns**
- [ ] **Step 3: Add unit test in `grade_widget.rs` testing compact 32px Lum slider track width**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 5: Side Panel Drag-Resize Width Persistence & Scrollbar Clearance (`src/app.rs`)

**Files:**
- Modify: `ferrolite-app/src/app.rs`

**Interfaces:**
- Produces: `SidePanel::left` and `SidePanel::right` drag-resize width persistence; 24px right scrollbar clearance channel.

- [ ] **Step 1: Set `inner_margin(Margin { left: 12.0, right: 24.0, top: 8.0, bottom: 8.0 })` for `SidePanel::right("develop_adjust")` in `app.rs`**
- [ ] **Step 2: Capture `res.response.rect.width()` for `develop_info_panel` and `develop_adjust` in `app.rs`, updating `state.settings` on change**
- [ ] **Step 3: Add unit test in `app.rs` testing drag-resize width persistence**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 6: Titlebar Double Border Removal & Alignment (`src/chrome/mod.rs`)

**Files:**
- Modify: `ferrolite-app/src/chrome/mod.rs`

**Interfaces:**
- Produces: Titlebar with single clean panel border line and aligned header tabs.

- [ ] **Step 1: Delete duplicate painter fill and bottom line segment in `chrome::title_bar` in `chrome/mod.rs`**
- [ ] **Step 2: Center `TabRow` vertically in titlebar with exact baseline alignment**
- [ ] **Step 3: Add unit test in `chrome/mod.rs` verifying clean titlebar rendering**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

## Verification Plan

### Automated Tests
- `cargo test -p ferrolite-catalog` to verify collection re-parenting.
- `cargo test -p ferrolite-app` to verify icon aliases, drag-and-drop re-parenting, compact Lum sliders, side panel drag-resize width persistence, and titlebar rendering.
- `cargo clippy --workspace --all-targets -- -D warnings` to verify zero warnings.

### Manual Verification
- Visual verification of:
  1. Slider reset arrows sit 24px+ to the left of the vertical scrollbar with zero overlap.
  2. Parametric curve preview box stretches across full panel width.
  3. Lum sliders in 2x2 Color Grading grid have wide, easily draggable tracks.
  4. Icons use Phosphor font (chevrons `CARET_RIGHT`/`CARET_DOWN`, `INFO`).
  5. Dragging a collection onto a Collection Set re-parents it into that set.
  6. Dragging the Left Info Panel or Right Develop Panel edge resizes the panel smoothly without snapping back on mouse release.
  7. Titlebar renders a single clean bottom border line with aligned tabs.
