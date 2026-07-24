# V2 Systematic UI Fixes Implementation Plan (Round 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the Round 3 systematic UI fixes: ScrollArea 16px right margin padding for scrollbar clearance, responsive Parametric curve preview box, symmetrical Lum sliders in 2x2 color grading grid, collection drag-and-drop drag ghost & drop highlight rectangles, stable side panel drag-resizing persistence, active titlebar tab 2px bottom underline bar, and removal of "+ Set" button.

**Architecture:** Update `ferrolite-app` crate (`app.rs`, `curve_widget_parametric.rs`, `grade_widget.rs`, `panel.rs`, `tab_row.rs`).

**Tech Stack:** Rust, `egui` 0.28, `ferrolite-app`.

## Global Constraints

- **Scope:** Changes in `ferrolite-app` crate.
- **Scoped Gate:**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`
- **Toolchain:** Compile cleanly on stable rustc without warnings (`-D warnings`).
- **Float-literal lint:** Suffix float literals in f32 context with `_f32`.
- **Per-Control Reset (CLAUDE.md):** Every parameter retains its individual reset arrow / double-click reset affordance.

---

## Task Breakdown

### Task 1: ScrollArea Right Clearance Padding & Stable Panel Resizing Persistence (`src/app.rs`)

**Files:**
- Modify: `ferrolite-app/src/app.rs`

**Interfaces:**
- Produces: 16px right margin padding container inside `ScrollArea` (preventing scrollbar overlap); stable `SidePanel` drag-resizing persistence updating settings on `drag_stopped()`.

- [ ] **Step 1: Wrap `tool_panel::show(...)` inside `ScrollArea::vertical()` with `egui::Frame::none().inner_margin(egui::Margin { left: 0.0, right: 16.0, top: 0.0, bottom: 0.0 })`**
- [ ] **Step 2: Update `state.settings.info_panel_width` and `right_panel_width` on `drag_stopped()` or clean size change without feedback loop**
- [ ] **Step 3: Add unit test in `app.rs` verifying panel width persistence and scrollbar clearance margin**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 2: Responsive Parametric Curve Preview (`src/develop/curve_widget_parametric.rs`)

**Files:**
- Modify: `ferrolite-app/src/develop/curve_widget_parametric.rs`

**Interfaces:**
- Produces: Full-width responsive Parametric curve preview graph box scaling dynamically to `ui.available_width()`.

- [ ] **Step 1: Modify `draw_overlay` in `curve_widget_parametric.rs` to allocate preview rect using `egui::vec2(ui.available_width(), OVERLAY_H)`**
- [ ] **Step 2: Add unit test verifying parametric preview graph box rect allocation across multiple container widths**
- [ ] **Step 3: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 3: Symmetrical Color Grading Wheel Lum Sliders (`src/develop/grade_widget.rs`)

**Files:**
- Modify: `ferrolite-app/src/develop/grade_widget.rs`

**Interfaces:**
- Produces: Lum sliders symmetrically centered beneath 88px color wheel discs in 2x2 layout with maximum draggable track width.

- [ ] **Step 1: Modify `wheel_row()` in `grade_widget.rs` to align Lum slider track symmetrically under color wheel center**
- [ ] **Step 2: Add unit test verifying Lum slider centering and track alignment**
- [ ] **Step 3: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 4: Collection Drag Ghost Tooltip, Drop Highlights, and Remove "+ Set" Button (`src/library/panel.rs`)

**Files:**
- Modify: `ferrolite-app/src/library/panel.rs`

**Interfaces:**
- Produces: Floating drag ghost tooltip (`"Moving [Collection Name]"`) during drag; 1.5px accent drop target highlight rectangle on hovered Collection Sets; removal of `+ Set` button.

- [ ] **Step 1: Remove `+ Set` button from COLLECTIONS section header in `panel.rs`**
- [ ] **Step 2: Add floating drag ghost tooltip under pointer during active drag (`"Moving [Name]"`)**
- [ ] **Step 3: Add 1.5px accent highlight rectangle around Collection Set target rows hovered during drag**
- [ ] **Step 4: Add unit tests in `panel.rs` for drag ghost and target drop highlight rendering**
- [ ] **Step 5: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 5: Active Titlebar Tab Underline Bar (`src/widgets/tab_row.rs`)

**Files:**
- Modify: `ferrolite-app/src/widgets/tab_row.rs`

**Interfaces:**
- Produces: 2px high `theme::ACCENT` underline bar on the bottom edge of the selected tab in `TabRow`.

- [ ] **Step 1: Modify `TabRow::ui` in `tab_row.rs` to paint a 2px high accent underline bar on the bottom edge of the active tab**
- [ ] **Step 2: Add unit test in `tab_row.rs` verifying active tab underline stroke painting**
- [ ] **Step 3: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

## Verification Plan

### Automated Tests
- `cargo test -p ferrolite-app` to verify all widget, layout, drag-and-drop, and titlebar changes.
- `cargo clippy --workspace --all-targets -- -D warnings` to verify zero warnings across workspace.

### Manual Verification
- Visual verification of:
  1. Slider reset arrows sit comfortably to the left of the vertical scrollbar with zero overlap.
  2. Parametric preview graph box expands dynamically with panel resizing.
  3. Lum sliders are centered directly beneath the color wheel discs in 2x2 layout.
  4. Dragging a collection displays a floating `"Moving [Name]"` ghost under pointer and highlights target set with an accent border.
  5. Dragging the Info Panel edge resizes smoothly without snapping back.
  6. Titlebar tabs (`Library | Develop | Export`) display a 2px accent underline under the active tab.
  7. "+ Set" button is removed from COLLECTIONS header.
