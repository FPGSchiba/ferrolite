# V2 UI Refinements & Layout Persistence Design Specification

**Date:** 2026-07-22  
**Status:** Approved  
**Scope:** `ferrolite-app` (UI layout, widgets, settings) & `ferrolite-catalog` (nested collections schema)

---

## 1. Executive Summary

This specification addresses eight key UI refinements for the Ferrolite desktop application:
1. Collapsible sections across all Develop panel tabs.
2. Nested collections support in the Library panel.
3. Slider reset arrow clearance from panel scrollbars.
4. Removal of section-level "Reset all" buttons.
5. Standardized section title headers.
6. Responsive scaling for Tone Curve and Color Grading widgets on panel resize.
7. Drag-resizable filmstrip in Develop view.
8. Persisted UI layout state (panel widths, filmstrip height, drawer toggles, section disclosure states) saved to settings DTO and restored on app launch.

---

## 2. Technical Architecture & Component Changes

### 2.1 Collapsible Sections (`src/develop/base_tabs.rs`)
- Every section in the Develop right panel is wrapped in a disclosure header with `▸`/`▾` chevrons.
- **Light Tab**:
  - `BASIC SLIDERS` (Exposure, Contrast, Temp, Tint, Highlights, Shadows, Whites, Blacks) — expanded by default.
  - `TONE CURVE` (`ToneCurveWidget`) — collapsed by default.
- **Color Tab**:
  - `COLOR (HSL)` (8 color range swatches + Hue/Sat/Lum) — expanded by default.
  - `COLOR GRADING` (4 `ColorGradingWheel` blocks + Blending/Balance) — collapsed by default.
- **Effects Tab**:
  - `SHARPENING` (Amount, Radius, Detail) — expanded by default.
  - `NOISE REDUCTION` (Luminance, Detail, Color, Color Detail) — expanded by default.
  - `OPTICS` (Lens picker, Distortion, Vignette) — collapsed by default.
- Section disclosure states are backed by `AppState.settings` so user preferences persist across sessions.

### 2.2 Nested Collections (`ferrolite-catalog` & `src/library/panel.rs`)
- **Database Schema**: Add `parent_id INTEGER` column to the `collections` table in SQLite (`ferrolite-catalog`), default `NULL`.
- **Model**: `CollectionRecord` includes `pub parent_id: Option<i64>`.
- **UI Rendering (`library/panel.rs`)**:
  - **Collection Sets** (collections with child items): Render with `▸`/`▾` chevrons, folder icon, and right-aligned count of child photos.
  - **Child Collections**: Indented by 16px beneath their parent set with a dot icon.
  - **Flat Collections**: Top-level items without children render with a collection dot icon.

### 2.3 Slider Reset Arrow Scrollbar Clearance (`src/widgets/slider.rs`)
- In `EguiSlider::ui`, increase the track right inset calculation from `VALUE_W + 8.0 + RESET_W` to `VALUE_W + 12.0 + RESET_W + 8.0` (reserving 24px clearance from the right panel bounding box).
- Prevents collision or overlap between the 16×16px per-control reset arrow target and the vertical scrollbar track.

### 2.4 Section Header Standardization (`src/widgets/mod.rs`)
- Add reusable `section_header(ui: &mut egui::Ui, label: &str, is_open: &mut bool) -> egui::Response` widget helper:
  - Font: `IBM Plex Mono`, `10px`, 1px letter-spacing, `600` weight, `#6a6a6a` text color.
  - Includes a `▸` / `▾` chevron indicator.
  - Renders a subtle 1px divider line (`#232323`) below the header.
- All section-level "Reset all" buttons are removed from section footers.

### 2.5 Responsive Scaling for Tone Curve & Color Grading (`src/widgets/`)
- **Tone Curve (`src/widgets/curve.rs`)**:
  - Dynamic box sizing: `let size = ui.available_width().clamp(180.0, 320.0);`.
  - Curve grid, polyline sampling, node grab hits, and parametric sliders scale proportionally with `size`.
- **Color Grading (`src/widgets/color_wheel.rs`)**:
  - Renders the four wheels in a 2-column grid when available width $\ge 280\text{px}$, falling back to a 1-column layout when narrower.
  - Centers each 88px wheel disc inside its grid cell.

### 2.6 Resizable Develop Filmstrip (`src/library/filmstrip.rs`)
- Add a horizontal drag handle (`egui::Sense::drag()`) at the bottom boundary of the Develop top filmstrip.
- Height range: `64.0` to `220.0` pixels (default `96.0`).
- Cursor changes to `egui::CursorIcon::ResizeVertical` on hover.

### 2.7 Layout Persistence (`src/settings/dto.rs` & `src/state.rs`)
- Add fields to `Settings` DTO:
  - `pub show_info_panel: bool` (default `false`)
  - `pub filmstrip_height: f32` (default `96.0`)
  - `pub tone_curve_open: bool` (default `false`)
  - `pub color_grading_open: bool` (default `false`)
  - `pub optics_open: bool` (default `false`)
- Auto-saved on mutation via `mark_settings_dirty()` and written off-thread by `save_settings_if_dirty()`.

---

## 3. Verification & Quality Gates

- **Unit Tests**:
  - Catalog tests verifying `parent_id` hierarchy in `ferrolite-catalog`.
  - `EguiSlider` right margin clearance math unit test.
  - `section_header` toggle state test.
  - `Settings` DTO serialization/deserialization persistence test.
- **Scoped Gate**: `cargo fmt -p ferrolite-app -- --check`, `cargo clippy -p ferrolite-app --all-targets -- -D warnings`, `cargo test -p ferrolite-app`.
- **Repo Gate**: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`.
