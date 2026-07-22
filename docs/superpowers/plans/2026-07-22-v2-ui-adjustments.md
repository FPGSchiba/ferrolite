# V2 UI Refinements & Layout Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the eight V2 UI refinements: nested collection sets, slider reset scrollbar clearance, collapsible Develop sections, removal of redundant "Reset all" section buttons, responsive scaling for Tone Curve & Color Grading widgets, resizable Develop filmstrip, and full layout settings persistence.

**Architecture:** Extend SQLite schema in `ferrolite-catalog` with `parent_id` for nested collections. Add `section_header` helper and adjust track right margin in `slider.rs`. Update `base_tabs.rs` to wrap Develop sections in collapsible headers and remove section-level reset buttons. Enable responsive square sizing in `curve.rs` and auto-fitting grid in `color_wheel.rs`. Add vertical drag splitter in `filmstrip.rs`. Add layout fields to `settings/dto.rs` for automatic disk persistence.

**Tech Stack:** Rust, `egui` 0.28, `ferrolite-app`, `ferrolite-catalog` (SQLite), `rusqlite`.

## Global Constraints

- **Scope:** Changes in `ferrolite-catalog` and `ferrolite-app` crates.
- **Scoped Gate:**
  - `ferrolite-catalog`: `cargo test -p ferrolite-catalog`
  - `ferrolite-app`: `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`
- **Toolchain:** Compile cleanly on stable rustc without warnings (`-D warnings`).
- **Float-literal lint:** Suffix float literals in f32 context with `_f32`.
- **Per-Control Reset (CLAUDE.md):** Every parameter retains its individual reset arrow / double-click reset affordance.

---

## File Structure

- **Modify `ferrolite-catalog/src/schema.rs`**: Add `parent_id INTEGER` column to `collections` table.
- **Modify `ferrolite-catalog/src/model.rs`**: Add `pub parent_id: Option<i64>` to `CollectionRecord`.
- **Modify `ferrolite-catalog/src/catalog.rs` / `queries.rs` / `read_pool.rs`**: Update collection creation and listing queries to handle `parent_id`.
- **Modify `ferrolite-app/src/widgets/slider.rs`**: Increase slider track right margin (`VALUE_W + 12.0 + RESET_W + 8.0`) to guarantee 24px clearance from panel scrollbars.
- **Modify `ferrolite-app/src/widgets/mod.rs`**: Add `section_header` widget helper.
- **Modify `ferrolite-app/src/develop/base_tabs.rs`**: Wrap Develop panel sections in collapsible headers; remove bottom section "Reset all" buttons.
- **Modify `ferrolite-app/src/widgets/curve.rs`**: Dynamically scale tone curve editor box (`ui.available_width().clamp(180.0, 320.0)`).
- **Modify `ferrolite-app/src/widgets/color_wheel.rs`**: Render color grading wheels in a responsive 2-column/1-column grid.
- **Modify `ferrolite-app/src/library/filmstrip.rs`**: Add draggable height splitter handle (64.0..220.0px range).
- **Modify `ferrolite-app/src/library/panel.rs`**: Render nested collection sets and child collections hierarchy.
- **Modify `ferrolite-app/src/settings/dto.rs`**: Add layout persistence fields (`show_info_panel`, `filmstrip_height`, section collapse booleans).
- **Modify `ferrolite-app/src/state.rs` & `app.rs`**: Wire settings dirty marking on layout change.

---

## Task Breakdown

### Task 1: Catalog Nested Collections Schema (`ferrolite-catalog`)

**Files:**
- Modify: `ferrolite-catalog/src/schema.rs`
- Modify: `ferrolite-catalog/src/model.rs`
- Modify: `ferrolite-catalog/src/catalog.rs`
- Modify: `ferrolite-catalog/src/queries.rs`
- Modify: `ferrolite-catalog/src/read_pool.rs`

**Interfaces:**
- Produces: `CollectionRecord { id: i64, name: String, color: Color, sort_order: i64, parent_id: Option<i64> }`, `Catalog::create_collection_with_parent(&self, name: &str, color: Color, parent_id: Option<i64>)`.

- [ ] **Step 1: Write unit tests in `queries.rs` for parent_id collection filtering**
- [ ] **Step 2: Add `parent_id INTEGER` column to `collections` schema in `schema.rs`**
- [ ] **Step 3: Update `CollectionRecord` struct in `model.rs`**
- [ ] **Step 4: Update collection create & read queries in `queries.rs`, `catalog.rs`, `read_pool.rs`**
- [ ] **Step 5: Run scoped gate for catalog**
  `cargo test -p ferrolite-catalog`

---

### Task 2: Slider Reset Clearance & Collapsible Section Headers (`src/widgets/`)

**Files:**
- Modify: `ferrolite-app/src/widgets/slider.rs`
- Modify: `ferrolite-app/src/widgets/mod.rs`

**Interfaces:**
- Produces: 24px scrollbar right margin clearance for `EguiSlider`, `pub fn section_header(ui: &mut egui::Ui, label: &str, is_open: &mut bool) -> egui::Response`.

- [ ] **Step 1: Write unit tests for `slider.rs` right clearance math and `section_header` toggle in `widgets/mod.rs`**
- [ ] **Step 2: Adjust `EguiSlider::ui` track right margin in `slider.rs`**
  - Reserve `VALUE_W + 12.0 + RESET_W + 8.0` from `rect.right()` so reset arrow icon is never covered by panel scrollbars.
- [ ] **Step 3: Implement `section_header` in `src/widgets/mod.rs`**
  - Font: `10px` monospace, `600` weight, letter-spaced, `#6a6a6a`.
  - Chevron `▸`/`▾` + section title + 1px `#232323` divider line.
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 3: Develop Collapsible Sections & Responsive Widget Scaling (`src/develop/`)

**Files:**
- Modify: `ferrolite-app/src/develop/base_tabs.rs`
- Modify: `ferrolite-app/src/widgets/curve.rs`
- Modify: `ferrolite-app/src/widgets/color_wheel.rs`

**Interfaces:**
- Produces: Collapsible sections across `LightTab`, `ColorTab`, `EffectsTab` with section reset buttons removed; responsive curve sizing and color grading grid.

- [ ] **Step 1: Write unit tests for dynamic curve sizing and color grading grid math**
- [ ] **Step 2: Refactor `base_tabs.rs` section layouts**
  - Wrap `BASIC SLIDERS`, `TONE CURVE`, `COLOR (HSL)`, `COLOR GRADING`, `SHARPENING`, `NOISE REDUCTION`, `OPTICS` in collapsible headers.
  - Remove all bottom "Reset all" buttons from section footers.
- [ ] **Step 3: Update `curve.rs` curve editor box size**
  - Use `let size = ui.available_width().clamp(180.0, 320.0);`.
- [ ] **Step 4: Update `color_wheel.rs` color grading layout**
  - Layout 4 color wheels in a 2-column grid when width $\ge 280\text{px}$, 1-column when narrower.
- [ ] **Step 5: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 4: Resizable Develop Filmstrip & Nested Collections UI (`src/library/`)

**Files:**
- Modify: `ferrolite-app/src/library/filmstrip.rs`
- Modify: `ferrolite-app/src/library/panel.rs`

**Interfaces:**
- Produces: Draggable filmstrip height splitter, nested collection set tree rendering in left sidebar.

- [ ] **Step 1: Write unit tests for filmstrip height drag clamping and collection tree building**
- [ ] **Step 2: Add vertical drag splitter in `filmstrip.rs`**
  - Height range `64.0..=220.0`, default `96.0`. Mouse drag updates height.
- [ ] **Step 3: Update `library/panel.rs` collection tree rendering**
  - Render Collection Sets (items with `parent_id == None` having children) with `▸`/`▾` chevrons + count.
  - Render child collections indented 16px.
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 5: UI Layout & Settings Persistence (`src/settings/` & `src/state.rs`)

**Files:**
- Modify: `ferrolite-app/src/settings/dto.rs`
- Modify: `ferrolite-app/src/state.rs`
- Modify: `ferrolite-app/src/app.rs`

**Interfaces:**
- Produces: `Settings` DTO layout fields (`show_info_panel`, `filmstrip_height`, `tone_curve_open`, `color_grading_open`, `optics_open`) persisted on change.

- [ ] **Step 1: Write unit test in `settings/dto.rs` for layout fields JSON serialization/deserialization**
- [ ] **Step 2: Add layout fields to `Settings` struct in `settings/dto.rs`**
- [ ] **Step 3: Connect `AppState` layout properties to `state.settings` in `app.rs`**
- [ ] **Step 4: Call `mark_settings_dirty()` whenever filmstrip height or section collapse state is toggled**
- [ ] **Step 5: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

## Verification Plan

### Automated Tests
- `cargo test -p ferrolite-catalog` to verify nested collection queries and schema.
- `cargo test -p ferrolite-app` to verify all widget clearance math, filmstrip drag clamping, section headers, and settings DTO serialization.
- `cargo clippy --workspace --all-targets -- -D warnings` to verify zero warnings across workspace.

### Manual Verification
- Hands-on visual testing of all 8 adjustments:
  1. Collapsible Develop sections with clear titles.
  2. Nested collections tree in Library left sidebar.
  3. Slider reset arrows comfortably clear of right scrollbar.
  4. No section-level "Reset all" buttons.
  5. Tone Curve & Color Grading wheels resizing smoothly on panel drag.
  6. Resizable top filmstrip in Develop view.
  7. UI layout states persisting across app restarts.
