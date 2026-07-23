# V2 Systematic UI Refinements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the verified systematic fixes for all 9 UI items: persistent section disclosure states, complete removal of "Reset all" buttons, right-margin scrollbar clearance for sliders, fully responsive Tone Curve and 2x2 Color Grading grid, CRUD UI & count badges for nested collections, native resizable filmstrip top panel, persisted left & right panel widths, resizable Left Info Panel, and titlebar vertical alignment.

**Architecture:** Extend `Settings` DTO in `settings/dto.rs` with all 7 section collapse booleans (`basic_sliders_open`, `color_hsl_open`, `sharpening_open`, `noise_reduction_open`, `tone_curve_open`, `color_grading_open`, `optics_open`) plus `right_panel_width` and `info_panel_width`. Update `base_tabs.rs` to bind headers to `state.settings.xxx_open`. Delete `Reset all` footer in `tool_panel.rs`. Configure `SidePanel::right` and `SidePanel::left` inner margins and resizable bindings in `app.rs`. Refactor `curve.rs` to scale to `ui.available_width()`. Refactor `grade_widget.rs` to lay out 4 wheels in a balanced 2-column grid (`ui.columns(2)`). Add "+ Set" and "+ Sub-collection" UI buttons in `library/panel.rs` with count badges. Make top filmstrip panel natively resizable (`TopBottomPanel::top().resizable(true)`).

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

### Task 1: Settings DTO Layout Fields & State Persistence (`src/settings/` & `src/state.rs`)

**Files:**
- Modify: `ferrolite-app/src/settings/dto.rs`
- Modify: `ferrolite-app/src/state.rs`
- Modify: `ferrolite-app/src/app.rs`

**Interfaces:**
- Produces: `Settings` DTO containing `basic_sliders_open`, `color_hsl_open`, `sharpening_open`, `noise_reduction_open`, `tone_curve_open`, `color_grading_open`, `optics_open`, `right_panel_width`, `info_panel_width`, `filmstrip_height`, `show_info_panel`.

- [ ] **Step 1: Add all 7 collapse booleans, `right_panel_width`, and `info_panel_width` to `Settings` in `settings/dto.rs` with `#[serde(default)]` and default functions**
- [ ] **Step 2: Add unit tests in `dto.rs` testing serde roundtripping for all layout fields**
- [ ] **Step 3: Update `AppState::new()` in `state.rs` to bind layout fields**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 2: Persistent Section Headers & Removal of "Reset all" Buttons (`src/develop/`)

**Files:**
- Modify: `ferrolite-app/src/develop/base_tabs.rs`
- Modify: `ferrolite-app/src/develop/tool_panel.rs`

**Interfaces:**
- Produces: All 7 Develop section headers bound to `state.settings.xxx_open` calling `mark_settings_dirty()`; complete deletion of `Reset all` footer in `tool_panel.rs`.

- [ ] **Step 1: Refactor `base_tabs.rs` to bind `BASIC SLIDERS`, `COLOR (HSL)`, `SHARPENING`, `NOISE REDUCTION`, `TONE CURVE`, `COLOR GRADING`, `OPTICS` to `state.settings.xxx_open`**
- [ ] **Step 2: Delete `if ui.button("Reset all").clicked()` footer block in `tool_panel.rs`**
- [ ] **Step 3: Add unit tests in `base_tabs.rs` testing persistent toggle states for all 7 sections**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 3: Panel Scrollbar Clearance & Resizable Side/Top Panels (`src/app.rs`)

**Files:**
- Modify: `ferrolite-app/src/app.rs`
- Modify: `ferrolite-app/src/library/filmstrip.rs`

**Interfaces:**
- Produces: `SidePanel::right("develop_adjust")` with right inner margin clearance (`right: 20.0`) and persisted width; `SidePanel::left("develop_info_panel")` with `.resizable(true)` and persisted width; `TopBottomPanel::top("develop_filmstrip")` with native `.resizable(true)`.

- [ ] **Step 1: Configure `SidePanel::right("develop_adjust")` in `app.rs` with `.default_width(self.state.settings.right_panel_width)` and `inner_margin(Margin { left: 12.0, right: 20.0, top: 8.0, bottom: 8.0 })`**
- [ ] **Step 2: Configure `SidePanel::left("develop_info_panel")` in `app.rs` with `.resizable(true)` and `.default_width(self.state.settings.info_panel_width)`**
- [ ] **Step 3: Configure `TopBottomPanel::top("develop_filmstrip")` in `app.rs` with `.resizable(true)` and `.height_range(64.0..=220.0)`**
- [ ] **Step 4: Remove manual bottom drag handle in `filmstrip.rs`**
- [ ] **Step 5: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 4: Responsive Tone Curve & 2x2 Color Grading Grid (`src/widgets/`)

**Files:**
- Modify: `ferrolite-app/src/widgets/curve.rs`
- Modify: `ferrolite-app/src/widgets/grade_widget.rs`
- Modify: `ferrolite-app/src/widgets/color_wheel.rs`

**Interfaces:**
- Produces: Full-width responsive Tone Curve (Point & Parametric view) box sizing; balanced 2x2 grid layout for Color Grading wheels with symmetric 50% width columns and centered Lum sliders.

- [ ] **Step 1: Update `curve.rs` to set square box size and parametric preview size to `ui.available_width()`**
- [ ] **Step 2: Update `grade_widget.rs` to lay out the 4 wheels using `ui.columns(2)` with 50% width cells and centered 88px discs + full-width Lum sliders**
- [ ] **Step 3: Add unit tests for responsive curve sizing and 2x2 color wheel layout**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 5: Nested Collection Sets CRUD UI & Count Badges (`src/library/`)

**Files:**
- Modify: `ferrolite-app/src/library/panel.rs`
- Modify: `ferrolite-app/src/library/collection_menu.rs`

**Interfaces:**
- Produces: "+ Set" button in Collections header, "Add Sub-collection..." context menu item, photo count badges `(N)` for collection sets and child items.

- [ ] **Step 1: Add "+ Set" button to Collections header in `panel.rs`**
- [ ] **Step 2: Add "Add Sub-collection..." option in `collection_menu.rs`**
- [ ] **Step 3: Update collection set and collection item tree rendering in `panel.rs` to display photo count badges `(N)`**
- [ ] **Step 4: Add unit tests for collection set creation, sub-collection creation, and count badges**
- [ ] **Step 5: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 6: Titlebar Vertical Alignment & Tab Formatting (`src/chrome/`)

**Files:**
- Modify: `ferrolite-app/src/chrome/mod.rs`

**Interfaces:**
- Produces: Vertically centered titlebar `TabRow` navigation tabs with 2px bottom padding, keeping active underline stroke clean and clear of titlebar border.

- [ ] **Step 1: Update `chrome/mod.rs` `center_rect` bounds to inset 2px from `bar.bottom()`**
- [ ] **Step 2: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

## Verification Plan

### Automated Tests
- `cargo test -p ferrolite-catalog` to verify nested collection queries and schema.
- `cargo test -p ferrolite-app` to verify all 7 persistent collapse booleans, panel width persistence, responsive curve box math, 2x2 color wheel layout, and collection CRUD UI.
- `cargo clippy --workspace --all-targets -- -D warnings` to verify zero warnings across workspace.

### Manual Verification
- Hands-on visual testing of all 9 items:
  1. Expand/collapse any section (Basic Sliders, Sharpening, Noise Reduction, HSL, Tone Curve, Color Grading, Optics) — stays open/closed across frames and restarts.
  2. Confirm total absence of "Reset all" footer button.
  3. Confirm slider reset arrows sit comfortably to the left of the scrollbar track with zero overlap.
  4. Confirm Tone Curve and Color Grading expand smoothly across the full width when resizing panel.
  5. Create a Collection Set, add a Sub-collection inside it, and verify photo count badges `(N)`.
  6. Drag top filmstrip bottom border to resize height.
  7. Drag right panel or Left Info Panel border to resize width, restart app, verify widths are restored.
  8. Confirm Left Info Panel is resizable.
  9. Confirm titlebar navigation tabs are vertically centered and un-clipped.
