# UI V2 Rewrite — Milestone 2: Reusable UI Library Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build and enhance the custom visual widget suite in `ferrolite-app/src/widgets/` following the V2 Design Specification (`docs/design/V2/README.md`), drawing styling directly from `theme.rs` design tokens, and enforcing the per-control reset convention.

**Architecture:** Extend `theme.rs` with V2 design tokens (`ACCENT_FILL`, `ACCENT_BORDER`, `ACCENT_TEXT`, `TEXT_ACTIVE`, `TEXT_INACTIVE`). Create `tabs.rs` (`TabRow`) and `chips.rs` (`SegmentedControl`), enhance `color_wheel.rs` (`ColorGradingWheel` with combined luminance slider), extend `curve.rs` (`ToneCurveWidget` supporting Point & Parametric curve editing with split sliders), and update `slider.rs` (`EguiSlider` with configurable label widths and bipolar fills). All widgets implement per-control resets or double-click reset affordances.

**Tech Stack:** Rust, `egui` 0.28, `ferrolite-app` design tokens (`theme.rs`), `ferrolite_pipeline`.

## Global Constraints

- **Scope:** Changes in `ferrolite-app` crate (`src/theme.rs`, `src/widgets/`). **Scoped gate:** `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`.
- **Toolchain:** Code must compile cleanly on latest stable rustc without warnings (`-D warnings`).
- **Float-literal lint:** Suffix any new egui/GPU float literal in f32 context with `_f32`.
- **Per-Control Reset (CLAUDE.md):** Every adjustable widget/slider MUST expose a per-control reset (double-click or reset arrow via `widgets::draw_reset_arrow`).
- **Icons (CLAUDE.md):** Icons come from `icons.rs` / `egui-phosphor`, not raw emoji or ad-hoc painter shapes.

---

## File Structure

- **Modify `ferrolite-app/src/theme.rs`**: Add V2 design tokens (`ACCENT_FILL`, `ACCENT_BORDER`, `ACCENT_TEXT`, `TEXT_ACTIVE`, `TEXT_INACTIVE`).
- **[NEW] `ferrolite-app/src/widgets/tabs.rs`**: `TabRow` widget — horizontal bar of selectable tabs with text `#eaf1f6`, 2px `#6d97b5` accent underline when active, and `#9a9a9a` when inactive.
- **[NEW] `ferrolite-app/src/widgets/chips.rs`**: `SegmentedControl` widget — contiguous/wrapped rounded rect buttons with 3px border radius, styled with V2 `ACCENT_FILL`, `ACCENT_BORDER`, and `ACCENT_TEXT`.
- **Modify `ferrolite-app/src/widgets/color_wheel.rs`**: Add `ColorGradingWheel` combining the 88px circular color wheel with an aligned luminance slider beneath it.
- **Modify `ferrolite-app/src/widgets/curve.rs`**: Add `ToneCurveWidget` supporting both Point Curve interaction and Parametric Curve split sliders (Highlights, Lights, Darks, Shadows + Shadow split/Midtone split/Highlight split thresholds).
- **Modify `ferrolite-app/src/widgets/slider.rs`**: Enhance `EguiSlider` builder with custom label-width option (`label_w`), ensuring bipolar fill and monospace numbers.
- **Modify `ferrolite-app/src/widgets/mod.rs`**: Expose new widgets (`tabs`, `chips`, `ColorGradingWheel`, `ToneCurveWidget`, `TabRow`, `SegmentedControl`).

---

## Task Breakdown

### Task 1: Theme Tokens Extension & `EguiSlider` Label Width Enhancement

**Files:**
- Modify: `ferrolite-app/src/theme.rs`
- Modify: `ferrolite-app/src/widgets/slider.rs`

**Interfaces:**
- Produces in `theme.rs`: `pub const ACCENT_FILL: Color32`, `pub const ACCENT_BORDER: Color32`, `pub const ACCENT_TEXT: Color32`, `pub const TEXT_ACTIVE: Color32`, `pub const TEXT_INACTIVE: Color32`.
- Produces in `slider.rs`: `EguiSlider::label_width(self, w: f32) -> Self`.

- [ ] **Step 1: Write unit tests for theme tokens and `EguiSlider` builder in tests module**
- [ ] **Step 2: Add theme constants to `theme.rs`**
  ```rust
  pub const ACCENT_FILL: Color32 = Color32::from_rgb(0x23, 0x2b, 0x30);
  pub const ACCENT_BORDER: Color32 = Color32::from_rgb(0x34, 0x46, 0x4f);
  pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0xcf, 0xe0, 0xec);
  pub const TEXT_ACTIVE: Color32 = Color32::from_rgb(0xea, 0xf1, 0xf6);
  pub const TEXT_INACTIVE: Color32 = Color32::from_rgb(0x9a, 0x9a, 0x9a);
  ```
- [ ] **Step 3: Add `custom_label_w: Option<f32>` field and `.label_width(w)` builder method to `EguiSlider` in `slider.rs`**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 2: Build `TabRow` Widget (`src/widgets/tabs.rs`)

**Files:**
- Create: `ferrolite-app/src/widgets/tabs.rs`
- Modify: `ferrolite-app/src/widgets/mod.rs`

**Interfaces:**
- Produces: `pub struct TabRow<'a, T> { ... }`, `pub fn tab_row<T: PartialEq + Clone>(ui: &mut egui::Ui, current: &mut T, tabs: &[(T, &'a str)]) -> Response`.

- [ ] **Step 1: Write unit test for `TabRow` selection logic in `tabs.rs`**
- [ ] **Step 2: Implement `TabRow` rendering in `src/widgets/tabs.rs`**
  - Horizontal layout with bottom separator.
  - Active tab text: `theme::TEXT_ACTIVE` (`#eaf1f6`), 2px bottom stroke with `theme::ACCENT` (`#6d97b5`).
  - Inactive tab text: `theme::TEXT_INACTIVE` (`#9a9a9a`), no stroke.
  - Click updates `current` value and requests repaint.
- [ ] **Step 3: Export `pub mod tabs;` and `pub use tabs::{tab_row, TabRow};` in `widgets/mod.rs`**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 3: Build `SegmentedControl` Widget (`src/widgets/chips.rs`)

**Files:**
- Create: `ferrolite-app/src/widgets/chips.rs`
- Modify: `ferrolite-app/src/widgets/mod.rs`

**Interfaces:**
- Produces: `pub fn segmented_control<T: PartialEq + Clone>(ui: &mut egui::Ui, id_source: impl std::hash::Hash, current: &mut T, options: &[(T, &str)]) -> Response`.

- [ ] **Step 1: Write unit test for `segmented_control` option selection in `chips.rs`**
- [ ] **Step 2: Implement `segmented_control` rendering in `src/widgets/chips.rs`**
  - Contiguous horizontal row of buttons with 3px border radius (`Rounding::same(3.0_f32)`).
  - Active state: `fill` = `theme::ACCENT_FILL` (`#232b30`), `stroke` = 1px `theme::ACCENT_BORDER` (`#34464f`), `text_color` = `theme::ACCENT_TEXT` (`#cfe0ec`).
  - Inactive state: `fill` = `theme::BG_BASE` (`#141414`), `stroke` = 1px `theme::BORDER_STRONG` (`#2a2a2a`), `text_color` = `theme::TEXT_DIM` (`#8a8a8a`).
- [ ] **Step 3: Export `pub mod chips;` and `pub use chips::segmented_control;` in `widgets/mod.rs`**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 4: Build `ColorGradingWheel` Widget (`src/widgets/color_wheel.rs`)

**Files:**
- Modify: `ferrolite-app/src/widgets/color_wheel.rs`
- Modify: `ferrolite-app/src/widgets/mod.rs`

**Interfaces:**
- Produces: `pub struct ColorGradingEdit { pub hue: f32, pub sat: f32, pub lum: f32, pub commit: bool }`, `pub fn color_grading_wheel(ui: &mut egui::Ui, id_source: impl std::hash::Hash, label: &str, hue: f32, sat: f32, lum: f32) -> Option<ColorGradingEdit>`.

- [ ] **Step 1: Write unit tests for `color_grading_wheel` math and reset logic in `color_wheel.rs`**
- [ ] **Step 2: Implement `color_grading_wheel` in `color_wheel.rs`**
  - Displays centered 88px circular color wheel disc (`color_wheel` with white thumb).
  - Below disc: label (e.g. "Shadows") and aligned luminance slider (`-100.0..=100.0` range, default `0.0`, bipolar fill).
  - Per-control reset arrow resets both (hue=0, sat=0, lum=0).
- [ ] **Step 3: Export `ColorGradingEdit` and `color_grading_wheel` in `widgets/mod.rs`**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 5: Build `ToneCurveWidget` (`src/widgets/curve.rs`)

**Files:**
- Modify: `ferrolite-app/src/widgets/curve.rs`
- Modify: `ferrolite-app/src/widgets/mod.rs`

**Interfaces:**
- Produces:
  `pub enum ToneCurveTab { Point, Parametric }`,
  `pub struct ParametricCurveValues { pub highlights: f32, pub lights: f32, pub darks: f32, pub shadows: f32, pub shadow_split: f32, pub midtone_split: f32, pub highlight_split: f32 }`,
  `pub struct ToneCurveEdit { pub points: Option<Vec<(f32, f32)>>, pub parametric: Option<ParametricCurveValues>, pub mode: CurveMode, pub reset: bool, pub commit: bool }`,
  `pub fn tone_curve_widget(ui: &mut egui::Ui, id_source: impl std::hash::Hash, active_tab: &mut ToneCurveTab, points: &[(f32, f32)], mode: CurveMode, style: &CurveStyle, parametric: &ParametricCurveValues) -> Option<ToneCurveEdit>`.

- [ ] **Step 1: Write unit tests for `ParametricCurveValues` default & reset calculation in `curve.rs`**
- [ ] **Step 2: Implement `tone_curve_widget` tab switcher (Point vs. Parametric) and Parametric split sliders in `curve.rs`**
  - Mode tab switcher: Point vs Parametric using `SegmentedControl`.
  - In Point mode: render interactive `curve_editor` (draggable node points, double-click / right-click to delete, Delete key, reset button).
  - In Parametric mode: render tone curve preview graph + 4 region sliders (Highlights, Lights, Darks, Shadows: `-100..=100`, bipolar, default `0.0`) + 3 split threshold sliders (Shadow Split `0.1..0.4`, Midtone Split `0.4..0.7`, Highlight Split `0.7..0.9`).
  - Implement per-control reset arrows for parametric sliders and a global "Reset curve" button.
- [ ] **Step 3: Export `ToneCurveTab`, `ParametricCurveValues`, `ToneCurveEdit`, and `tone_curve_widget` in `widgets/mod.rs`**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

## Verification Plan

### Automated Tests
- Run `cargo test -p ferrolite-app` to verify all widget unit tests (theme colors, slider math, color wheel calculations, curve defaults, parametric range math).
- Run `cargo clippy -p ferrolite-app --all-targets -- -D warnings` to verify warning-free compilation on stable rustc.
- Run `cargo fmt -p ferrolite-app -- --check` to confirm formatting compliance.

### Manual Verification
- Render widgets in egui app UI and visually test interaction affordances:
  1. `TabRow`: Click tabs to verify active underline (`#6d97b5`) and text color switch (`#eaf1f6` vs `#9a9a9a`).
  2. `SegmentedControl`: Click chips to verify `ACCENT_FILL` (`#232b30`) + `ACCENT_BORDER` (`#34464f`) selection style with 3px border radius.
  3. `ColorGradingWheel`: Drag thumb on 88px disc + drag luminance slider beneath it; test per-control reset arrow.
  4. `ToneCurveWidget`: Toggle between Point Curve and Parametric Curve; test dragging nodes, double/right-clicking to delete, and dragging parametric split sliders.
  5. `EguiSlider`: Verify bipolar fill expanding from center for exposure/temp/tint, monospace formatting, and double-click reset.
