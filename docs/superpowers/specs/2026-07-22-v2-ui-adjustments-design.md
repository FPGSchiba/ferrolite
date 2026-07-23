# V2 UI Refinements & Layout Persistence Design Specification

**Date:** 2026-07-22 (Updated 2026-07-23)  
**Status:** Approved  
**Scope:** `ferrolite-app` (UI layout, widgets, settings) & `ferrolite-catalog` (nested collections schema)

---

## 1. Executive Summary

This specification addresses nine key UI refinements for the Ferrolite desktop application:
1. Persistent collapsible sections across all Develop panel tabs (fixing state snap-back).
2. Complete removal of section-level and tool-level "Reset all" buttons.
3. Slider reset arrow clearance from panel scrollbars via dedicated scrollbar margin channels.
4. Fully responsive scaling for Tone Curve (Point & Parametric) and Color Grading widgets (2x2 grid layout) on panel resize.
5. Nested collections support in the Library panel with CRUD affordances (+Set, +Sub-collection) and count badges.
6. Native resizable Develop filmstrip (`TopBottomPanel::top` resizable).
7. Persisted Right Panel width (`right_panel_width`).
8. Resizable Left Info Panel (`info_panel_width`).
9. Clean titlebar vertical margins and centered module tabs (`TabRow`).

---

## 2. Technical Architecture & Component Changes

### 2.1 Persistent Collapsible Sections (`src/develop/base_tabs.rs` & `src/settings/dto.rs`)
- All 7 section collapse booleans are fields on `Settings` (default `true`):
  - `basic_sliders_open: bool`
  - `tone_curve_open: bool`
  - `color_hsl_open: bool`
  - `color_grading_open: bool`
  - `sharpening_open: bool`
  - `noise_reduction_open: bool`
  - `optics_open: bool`
- In `base_tabs.rs`, headers use `section_header(ui, label, &mut state.settings.xxx_open)`. When toggled, `mark_settings_dirty()` is called so states persist across frames and restarts.

### 2.2 Complete Removal of "Reset all" Buttons (`src/develop/tool_panel.rs`)
- Delete lines 83-90 in `tool_panel.rs` which rendered `if ui.button("Reset all").clicked()`.
- Eliminate all section-level reset footers across Develop tabs. Individual per-control reset arrows and double-click slider resets fulfill parameter restoration.

### 2.3 Scrollbar Clearance for Sliders (`src/app.rs` & `src/widgets/slider.rs`)
- In `app.rs`, configure `SidePanel::right("develop_adjust")` frame and `ScrollArea::vertical()` with a dedicated right margin (`inner_margin(egui::Margin { left: 12.0, right: 20.0, top: 8.0, bottom: 8.0 })`).
- In `slider.rs`, calculate track right edge factoring in scrollbar inner spacing so reset arrows sit cleanly inside the content area without overlap.

### 2.4 Responsive Tone Curve & Color Grading (`src/widgets/`)
- **Tone Curve (`src/widgets/curve.rs`)**:
  - Point mode box size: `let size = ui.available_width();` (expands to full available width of panel).
  - Parametric mode preview box size: `let size = ui.available_width();` (expands to full available width).
- **Color Grading (`src/widgets/grade_widget.rs` & `color_wheel.rs`)**:
  - Render wheels in a balanced 2x2 grid (`ui.columns(2)`) where each column takes 50% width.
  - Position centered 88px wheel disc + full-width Lum slider in each grid cell.

### 2.5 Nested Collections UI & Count Badges (`src/library/panel.rs` & `collection_menu.rs`)
- **Collection Sets**: Render with `▸`/`▾` chevrons, folder set icon, title, and right-aligned count `(N)`.
- **UI Actions**:
  - Add "+ Set" button to Collections header.
  - Add "Add Sub-collection..." to parent set context menu.
- **Child Collections**: Indented by 16px with collection dot icon and photo count `(N)`.

### 2.6 Native Resizable Develop Filmstrip (`src/app.rs` & `src/library/filmstrip.rs`)
- In `app.rs`, define `egui::TopBottomPanel::top("develop_filmstrip").resizable(true).height_range(64.0..=220.0)`.
- Remove manual bottom drag allocation in `filmstrip.rs`.
- Bind height to `self.state.settings.filmstrip_height` and call `mark_settings_dirty()` when resized.

### 2.7 Persisted Panel Widths (`src/app.rs` & `src/settings/dto.rs`)
- Add fields to `Settings`:
  - `pub right_panel_width: f32` (default `300.0`, range `250.0..=450.0`)
  - `pub info_panel_width: f32` (default `300.0`, range `220.0..=450.0`)
- Bind `SidePanel::right` and `SidePanel::left` widths in `app.rs`, calling `mark_settings_dirty()` on resize.

### 2.8 Resizable Left Info Panel (`src/app.rs`)
- In `app.rs`, change `SidePanel::left("develop_info_panel")` to `.resizable(true)` and `.default_width(self.state.settings.info_panel_width)`.

### 2.9 Titlebar Margin Polish (`src/chrome/mod.rs`)
- Position `TabRow` centered inside `bar` with 2px bottom inset so the active underline stroke does not collide with the titlebar border line (`pos2(bar.left(), bar.bottom() - 1.0_f32)`).

---

## 3. Verification & Quality Gates

- **Unit Tests**:
  - Catalog unit tests for parent-child creation & count queries.
  - `Settings` DTO serialization/deserialization for all 7 collapse booleans, `right_panel_width`, `info_panel_width`, and `filmstrip_height`.
  - `EguiSlider` right margin clearance test.
- **Scoped Gate**: `cargo fmt -p ferrolite-app -- --check`, `cargo clippy -p ferrolite-app --all-targets -- -D warnings`, `cargo test -p ferrolite-app`.
- **Repo Gate**: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`.
