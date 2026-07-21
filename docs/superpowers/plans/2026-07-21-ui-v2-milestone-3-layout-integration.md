# UI V2 Rewrite — Milestone 3: V2 Theme & Layout Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the Ferrolite UI V2 redesign by consolidating Develop adjustment controls into three single-row tabs (Light, Color, Effects) with V2 widgets, building the 300px Left Info Panel and canvas toggle HUD, updating the V2 Titlebar and Toolbar with anchored Metadata Filters popup, and implementing the reverse-layout V2 Export Panel.

**Architecture:** Integrate Milestone 2 widgets (`TabRow`, `SegmentedControl`, `ColorGradingWheel`, `ToneCurveWidget`, `EguiSlider`) directly into app panels. Add `show_info_panel` to `AppState` to drive the left info drawer. Structure Develop panel into 3 consolidated base tabs (Light, Color, Effects). Update Chrome titlebar to 30px with logo mark and version string. Add Metadata Filters popup panel in Library toolbar. Implement reverse control-left/label-right export settings layout.

**Tech Stack:** Rust, `egui` 0.28, `ferrolite-app`, `ferrolite-pipeline`, Phosphor icons.

## Global Constraints

- **Scope:** Changes in `ferrolite-app` crate. **Scoped gate:** `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`.
- **Toolchain:** Compile cleanly under stable rustc without warnings (`-D warnings`).
- **Float-literal lint:** Suffix float literals in f32 context with `_f32`.
- **Per-Control Reset (CLAUDE.md):** Every editable parameter exposes a per-control reset (double-click or reset arrow via `widgets::draw_reset_arrow`).
- **Icons (CLAUDE.md):** Icons come from `icons.rs` / `egui-phosphor`, not raw emoji or ad-hoc painter shapes.

---

## File Structure

- **Modify `ferrolite-app/src/state.rs`**: Add `pub show_info_panel: bool` field to `AppState`.
- **Modify `ferrolite-app/src/develop/base_tabs.rs`**: Consolidate adjustment controls into three tabs (**Light**, **Color**, **Effects**). Integrate `ToneCurveWidget` into Light tab, HSL swatches + `ColorGradingWheel` into Color tab, and Sharpening/NR/Optics into Effects tab. Remove `InfoTab`.
- **Modify `ferrolite-app/src/develop/tool_panel.rs`**: Use `TabRow` for rendering base tabs on a single row.
- **[NEW] `ferrolite-app/src/develop/info_panel.rs`**: Read-only 300px left info panel listing camera, lens, focal, aperture, shutter, ISO, timestamp, dimensions, and zoom.
- **Modify `ferrolite-app/src/develop/info_overlay.rs`**: Add floating `ℹ Info` pill button on canvas (~132px from bottom) that toggles `state.show_info_panel`.
- **Modify `ferrolite-app/src/chrome/mod.rs`**: 30px titlebar (`#111111` bg, 1px `#262626` border) with logo square ("F"), version string "v0.1.2", and `TabRow` navigation.
- **Modify `ferrolite-app/src/library/toolbar.rs`**: 38px toolbar with Metadata Filters popup panel anchored under "Metadata" button (Camera, Lens, Rating, File Type segmented chips, Exposure sliders).
- **[NEW/MODIFY] `ferrolite-app/src/export_module/settings_panel.rs`**: Export settings panel with control-left / label-right layout, format/bit-depth/effort `SegmentedControl` chips, and metadata checkboxes.
- **Modify `ferrolite-app/src/export_module/mod.rs`**: Expose `settings_panel`.

---

## Task Breakdown

### Task 1: State Extension & Left Info Panel (`src/state.rs`, `src/develop/info_panel.rs`, `src/develop/info_overlay.rs`)

**Files:**
- Modify: `ferrolite-app/src/state.rs`
- Create: `ferrolite-app/src/develop/info_panel.rs`
- Modify: `ferrolite-app/src/develop/info_overlay.rs`
- Modify: `ferrolite-app/src/develop/mod.rs`

**Interfaces:**
- Produces: `AppState.show_info_panel: bool`, `pub fn show_info_panel(ui: &mut egui::Ui, state: &AppState)`, `draw_info_toggle_button(ui: &egui::Ui, show_info: &mut bool)`.

- [ ] **Step 1: Write unit tests for `ImageFacts` display strings in `info_panel.rs`**
- [ ] **Step 2: Add `pub show_info_panel: bool` (default `false`) to `AppState` in `src/state.rs`**
- [ ] **Step 3: Create `src/develop/info_panel.rs` rendering the 300px left panel**
  - Background `#1a1a1a`, 1px `#262626` right border.
  - Rows: Camera, Lens, Focal, Aperture, Shutter, ISO, Captured, Size, Zoom.
  - Label column 66px (`#7a7a7a`), value column (`#d0d0d0`). Read-only, no reset.
- [ ] **Step 4: Update `src/develop/info_overlay.rs` to render the floating `ℹ Info` pill button**
  - Positioned bottom-left ~132px from bottom edge (above EXIF chip).
  - Toggles `state.show_info_panel` when clicked.
- [ ] **Step 5: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 2: Develop Base Tabs Consolidation (`src/develop/base_tabs.rs` & `src/develop/tool_panel.rs`)

**Files:**
- Modify: `ferrolite-app/src/develop/base_tabs.rs`
- Modify: `ferrolite-app/src/develop/tool_panel.rs`

**Interfaces:**
- Produces: `base_tabs()` containing `[LightTab, ColorTab, EffectsTab]`, single-row `tab_row` rendering in `tool_panel.rs`.

- [ ] **Step 1: Write unit tests for `LightTab`, `ColorTab`, and `EffectsTab` tab IDs in `base_tabs.rs`**
- [ ] **Step 2: Refactor `LightTab` in `base_tabs.rs`**
  - Exposure, Contrast, Temp, Tint, Highlights, Shadows, Whites, Blacks sliders.
  - Integrated `ToneCurveWidget` (Point and Parametric mode).
- [ ] **Step 3: Refactor `ColorTab` in `base_tabs.rs`**
  - 8 HSL color swatches (red/orange/yellow/green/cyan/blue/purple/pink) with Hue/Sat/Lum sliders.
  - integrated `ColorGradingWheel` blocks (Shadows, Midtones, Highlights, Global) with Blending and Balance sliders.
- [ ] **Step 4: Refactor `EffectsTab` in `base_tabs.rs`**
  - Sharpening (Amount, Radius, Detail).
  - Noise Reduction (Luminance, Detail, Color, Color Detail).
  - Optics (lens selection, Distortion, Vignette).
- [ ] **Step 5: Update `tool_panel.rs` to render base tabs using `tab_row` on a single row**
- [ ] **Step 6: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 3: V2 Titlebar & Toolbar with Metadata Filters Popup (`src/chrome/mod.rs`, `src/library/toolbar.rs`)

**Files:**
- Modify: `ferrolite-app/src/chrome/mod.rs`
- Modify: `ferrolite-app/src/library/toolbar.rs`

**Interfaces:**
- Produces: 30px V2 Titlebar layout with logo square and version string, 38px Toolbar with anchored Metadata Filters popup.

- [ ] **Step 1: Write unit tests for Metadata Filter popup state toggling in `toolbar.rs`**
- [ ] **Step 2: Update `chrome/mod.rs` titlebar rendering**
  - Height 30px, `#111111` bg, 1px `#262626` bottom border.
  - Logo mark (14×14 accent square with "F") + "FERROLITE" header text.
  - Nav tabs (Library / Develop / Export) using `TabRow`.
  - Right-aligned version string "v0.1.2" (`IBM Plex Mono`, `10.5px`, `#6a6a6a`) + window controls.
- [ ] **Step 3: Update `library/toolbar.rs` layout and add Metadata Filters popup**
  - Height 38px, `#1a1a1a` bg, 1px `#262626` bottom border.
  - Search field (210px), Sort combo, star ratings, pick/reject flag buttons, Tags combo, Subfolders checkbox, "Metadata" button.
  - Anchored Metadata Filters popup panel (300px wide, `#1d1d1d` bg): Camera, Lens, Rating combos, FILE TYPE chips (`SegmentedControl`), Exposure range sliders, Apply/Close buttons.
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

### Task 4: V2 Export Settings Panel (`src/export_module/settings_panel.rs`)

**Files:**
- Create: `ferrolite-app/src/export_module/settings_panel.rs`
- Modify: `ferrolite-app/src/export_module/mod.rs`
- Modify: `ferrolite-app/src/export_module/queue_list.rs`

**Interfaces:**
- Produces: `pub fn export_settings_panel(ui: &mut egui::Ui, state: &mut AppState)`.

- [ ] **Step 1: Write unit tests for export settings values and format/bit-depth defaults in `settings_panel.rs`**
- [ ] **Step 2: Implement `export_settings_panel` in `src/export_module/settings_panel.rs`**
  - Width 300px, `#1a1a1a` bg, 1px `#262626` left border.
  - Reverse layout: control on the left, label on the right.
  - Format combo (JPEG, PNG, TIFF, WebP, AVIF, JXL).
  - Output color space combo (sRGB, AdobeRGB, Rec2020, DisplayP3, ProPhoto).
  - Bit depth `SegmentedControl` (8-bit / 16-bit).
  - Quality slider + label.
  - Effort `SegmentedControl` (Fast / Balanced / Best).
  - Checkboxes: Copy EXIF, Embed ICC profile, Strip metadata.
- [ ] **Step 3: Wire `export_settings_panel` into Export view layout in `queue_list.rs`**
- [ ] **Step 4: Run scoped gate**
  `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`

---

## Verification Plan

### Automated Tests
- Run `cargo test -p ferrolite-app` to verify all unit tests (info panel facts formatting, base tabs setup, metadata filter toggling, export settings defaults).
- Run `cargo clippy --workspace --all-targets -- -D warnings` to verify zero warnings.
- Run `cargo fmt --all -- --check` to verify code formatting.

### Manual Verification
- Hands-on visual testing of all V2 layouts:
  1. **Titlebar**: Verify 30px height, logo mark ("F"), version string ("v0.1.2"), nav tab switching.
  2. **Develop Base Tabs**: Verify 3 tabs (Light, Color, Effects) on a single `TabRow`, ToneCurveWidget in Light, ColorGradingWheel in Color, Sharpening/NR/Optics in Effects.
  3. **Left Info Drawer**: Click floating `ℹ Info` button on canvas to verify 300px left panel opens/closes without canvas overlap.
  4. **Library Toolbar**: Click "Metadata" button to verify anchored 300px Metadata Filters popup panel appears with `SegmentedControl` chips.
  5. **Export Panel**: Verify control-left/label-right reverse layout and format/bit-depth/effort `SegmentedControl` chips.
