# V2 Systematic UI Fixes & Polish Design Specification (Round 3)

**Date:** 2026-07-24  
**Status:** Approved  
**Scope:** `ferrolite-app` (UI layout, widgets, icons, settings, library panel)

---

## 1. Executive Summary

This specification addresses the final 8 UI polish items:
1. Scrollbar clearance inside `ScrollArea` by applying 16px right inner margin to `ScrollArea` content frame.
2. Responsive Parametric preview box expanding dynamically to `ui.available_width()`.
3. Symmetrical alignment of Lum sliders beneath 88px color wheel discs in 2x2 grid.
4. Drag-and-drop visual drag ghost and target row highlight rectangles for collection sets.
5. Stable Left Info Panel drag-resizing persistence without snapping back.
6. Titlebar active tab 2px bottom underline bar.
7. Removal of redundant `+ Set` button from COLLECTIONS section header.

---

## 2. Technical Architecture & Component Changes

### 2.1 Scrollbar Margin Clearance inside `ScrollArea` (`src/app.rs`)
- Wrap `tool_panel::show(...)` inside `ScrollArea::vertical()` in `app.rs` with `egui::Frame::none().inner_margin(egui::Margin { left: 0.0, right: 16.0, top: 0.0, bottom: 0.0 })`.
- This ensures sliders and Tone Curve box stop 16px to the left of the scrollbar thumb with zero overlap.

### 2.2 Responsive Parametric Preview Box (`src/develop/curve_widget_parametric.rs`)
- In `draw_overlay`, allocate rect using `egui::vec2(ui.available_width(), OVERLAY_H)` instead of fixed `OVERLAY_W = 200.0`.

### 2.3 Symmetrical Color Wheel Lum Sliders (`src/develop/grade_widget.rs`)
- Symmetrically center Lum slider track beneath the 88px color wheel disc center and expand slider track width.

### 2.4 Collection Drag-and-Drop Visual Ghost & Drop Highlights (`src/library/panel.rs`)
- Display a floating drag ghost tooltip under pointer (`"Moving [Collection Name]"`) during drag.
- Paint a 1.5px accent border highlight around target Collection Set header rows hovered during drag.

### 2.5 Stable Left Info Panel Resizing Persistence (`src/app.rs`)
- Update `state.settings.info_panel_width` ONLY on `drag_stopped()` or when `available_width()` changes cleanly.

### 2.6 Titlebar Tab Active Underline Bar (`src/widgets/tab_row.rs`)
- Paint a 2px high `theme::ACCENT` underline stroke at the bottom of the selected `TabRow` tab.

### 2.7 Remove "+ Set" Button (`src/library/panel.rs`)
- Remove the `+ Set` button from the COLLECTIONS header in `panel.rs`.

---

## 3. Verification & Quality Gates

- **Scoped Gate**: `cargo fmt -p ferrolite-app -- --check`, `cargo clippy -p ferrolite-app --all-targets -- -D warnings`, `cargo test -p ferrolite-app`.
- **Repo Gate**: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`.
