# V2 Systematic UI Fixes & Polish Design Specification (Round 2)

**Date:** 2026-07-23  
**Status:** Approved  
**Scope:** `ferrolite-app` (UI layout, widgets, icons, settings) & `ferrolite-catalog` (collection parent updates)

---

## 1. Executive Summary

This specification addresses the remaining UI polish items from visual testing:
1. Panel scrollbar margin clearance for sliders and tone curve editor.
2. Responsive Parametric curve preview box & compact Lum slider layout in 2x2 Color Grading wheels.
3. Full compliance with `CLAUDE.md` icon directives (replacing raw symbols with `icons::*` Phosphor aliases) and drag-and-drop re-parenting for collections.
4. Correct egui `SidePanel` drag-resize width persistence for Left Info Panel and Right Develop Panel.
5. Titlebar double border removal and precise vertical tab alignment.

---

## 2. Technical Architecture & Component Changes

### 2.1 Panel Scrollbar Margin Clearance (`src/app.rs` & `src/widgets/slider.rs`)
- In `app.rs`, set `SidePanel::right("develop_adjust")` inner margin to `inner_margin(egui::Margin { left: 12.0, right: 24.0, top: 8.0, bottom: 8.0 })`.
- Configure `ScrollArea::vertical().scrollbar_width(10.0)` with clear right padding.
- In `slider.rs`, reserve 24px right clearance away from `rect.right()`.

### 2.2 Responsive Parametric Preview & Compact 2x2 Lum Sliders (`src/widgets/`)
- **Parametric Curve Preview (`src/widgets/curve.rs`)**: Set parametric curve graph preview rect width to `ui.available_width()`.
- **2x2 Color Grading Sliders (`src/develop/grade_widget.rs`)**: Pass `.label_width(32.0)` to `EguiSlider` instances in `grade_widget.rs` so Lum sliders have over 100px of drag space in each 50% column.

### 2.3 Icon Directive Sweep & Collection Drag-and-Drop Re-parenting (`src/icons.rs` & `src/library/panel.rs`)
- **Icon Directive Sweep**:
  - Add `pub const CARET_RIGHT: &str = p::CARET_RIGHT;` in `src/icons.rs`.
  - Replace all raw string glyphs (`▸`, `▾`, `ℹ`, `✕`, `↺`) in `widgets/mod.rs`, `library/panel.rs`, `develop/`, and `chrome/` with `icons::CARET_RIGHT`, `icons::CARET_DOWN`, `icons::INFO`, `icons::CLOSE`, `icons::RESET`.
- **Collection Drag-and-Drop Re-parenting (`src/library/panel.rs` & `ferrolite-catalog`)**:
  - Add `catalog.update_collection_parent(collection_id, parent_id)` in `ferrolite-catalog`.
  - Enable egui `ui.dnd_set_drag_payload` on collection rows and `ui.dnd_release_payload::<i64>` on Collection Set headers, calling `update_collection_parent` on drop.

### 2.4 Persistent Panel Width Drag-Resizing (`src/app.rs`)
- In `app.rs`, capture `response.response.rect.width()` from `SidePanel::left("develop_info_panel")` and `SidePanel::right("develop_adjust")`.
- If the width differs from `state.settings.info_panel_width` or `state.settings.right_panel_width`, update `state.settings` and call `mark_settings_dirty()`.

### 2.5 Titlebar Double Border Removal & Tab Alignment (`src/chrome/mod.rs` & `src/app.rs`)
- Remove duplicate painter rect and 1px border line in `chrome/title_bar` (`src/chrome/mod.rs`).
- Center `TabRow` vertically in `titlebar` matching header typography height.

---

## 3. Verification & Quality Gates

- **Scoped Gate**: `cargo fmt -p ferrolite-app -- --check`, `cargo clippy -p ferrolite-app --all-targets -- -D warnings`, `cargo test -p ferrolite-app`.
- **Repo Gate**: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`.
