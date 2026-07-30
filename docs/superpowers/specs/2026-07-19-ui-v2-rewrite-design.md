# ferrolite — UI V2 Rewrite: UI Library, app.rs Split & Panel Consolidation (design)

> **Status:** Design — approved in brainstorming (2026-07-19); pending user final review of this spec, then writing-plans.
> **Date:** 2026-07-19
> **Branch:** `feat/ui-v2-rewrite`
> **Context:** Refactoring `ferrolite-app` to implement the V2 "digital darkroom" design, establishing a reusable UI widget library, and splitting up the massive `app.rs` file.
> **Proves:** Reducing `app.rs` from 4762 lines to <1200 lines, defining clear seams between UI layout and state coordination, and creating a unified visual style system for widgets (sliders, wheels, curves, tabs, lists, trees).

---

## 1. Goal & Validation

The goal of this phase is to reorganize and rewrite the user interface of Ferrolite to match the **V2 Design Specification** (`docs/design/V2/README.md`) while simultaneously solving the maintenance debt of `app.rs` (currently 4762 LOC). 

### Success Criteria:
1. **V2 Fidelity:** The running app matches the "digital darkroom" layout, colors, typography, and controls specified in V2 mockups.
2. **Reusable UI Library:** Core widgets like custom sliders, color grading wheels, tone curves, tab rows, segmented chips, rating stars, and pick/reject indicators are defined in a dedicated widgets module (`src/widgets/`) or a UI module with zero state coordination logic.
3. **`app.rs` Size Reduction:** `app.rs` functions purely as the high-level coordinator of state lifecycle and async background events. It contains less than **1,200 lines of code** (ideally <800 lines).
4. **Clean Code Split:** All panels, canvas rendering (`drive_viewer` and overlays), shortcut key handling, and background load event outcomes are decoupled from `app.rs` and moved to modular sub-files.
5. **Per-Control Reset:** Every newly consolidated control in the Light, Color, and Effects tabs respects the CLAUDE.md rule, exposing a dedicated reset-to-default button/gesture.

---

## 2. Scope

### In:
* **UI Library consolidation (`src/widgets/`):**
  * Enhance `EguiSlider` to support V2 requirements (bipolar EV, monospace value display, double-click to reset, compact labels).
  * Build `ColorGradingWheel` custom widget (painter-drawn conic gradient circle + draggable coordinate dot + Lum slider).
  * Build `ToneCurveWidget` (combining parametric curve splines and points curve canvas).
  * Build `TabRow` (custom horizontal tab bar with active accent underline and steel-blue highlights).
  * Build `SegmentedControl` / `ChipGroup` (for aspect ratio chips, RAW/JPEG selection, Format selection).
  * Build `InteractiveRatingStars` and `PickFlags` widgets.
* **Develop View Consolidation:**
  * Reorganize the 7 V1 tabs (Light, Color, Grade, Curve, Detail, Optics, Info) into exactly 3 V2 tabs: **Light** (exposure/contrast/tone-curve), **Color** (HSL swatches, grading wheels), and **Effects** (sharpen/noise-reduction/optics).
  * Move the **Info Tab** out of the right panel and place it in a toggleable Left Info Panel, triggered by a floating button.
* **`app.rs` Architectural Split:**
  * Move **Left Panel UI** (Catalog, Folders tree, Collections tree, Tags tree) entirely into `src/library/panel.rs` and `src/library/folder_tree.rs`, etc.
  * Move **Canvas UI & Interactions** (formerly `drive_viewer`, zoom/pan calculations, crop grids, mask brush strokes overlays, EXIF badge overlays) into `src/viewer/canvas.rs` or `src/develop/canvas/`.
  * Move **Event Outcomes / Transition Controllers** (handling raw loading, decode callbacks, pyramid building, lens-bake outcomes) out of `app.rs` into `src/viewer/controller.rs` or `src/app/controller.rs`.
  * Move **Export Settings & Bottom Bar UI** into `src/export_module/settings_panel.rs` and `src/export_module/bottom_bar.rs`.
  * Move **Shortcut Key Handling** out of the main `update` loop into `src/settings/shortcuts.rs` or `src/settings/keymap.rs`.

### Out:
* No changes to the RAW decoding backend or `rawler`.
* No changes to the GPU pipeline nodes (`wgpu` compute shaders).
* No changes to the database schemas (`ferrolite-catalog` SQLite schema).
* No new processing operations (ops are consolidated in terms of UI representation, but the pipeline DAG and `OpStack` logic remains unchanged).

---

## 3. Architecture of the Split

```
ferrolite-app/
  src/
    app.rs                         // State orchestrator (<1,200 lines). Handles App lifecycle, texture cache, event loop, and delegates layout.
    app/
      controller.rs                // [NEW] Handles AppEvent outcomes: apply_preview_ready, apply_full_decoded, apply_pyramid_ready, apply_lens_baked.
      shortcuts.rs                 // [NEW] Dispatches keyboard shortcuts/actions mapped from the keymap.
    widgets/                       // Unified UI Library
      mod.rs                       // Exports all widgets.
      slider.rs                    // EguiSlider (enhanced).
      color_wheel.rs               // ColorGradingWheel (new conic gradient widget).
      curve.rs                     // ToneCurveWidget (combined point & parametric UI).
      tabs.rs                      // [NEW] TabRow with V2 style.
      chips.rs                     // [NEW] SegmentedControl / ChipGroup selector.
      rating.rs                    // [NEW] RatingStars & PickFlags.
    library/
      panel.rs                     // Catalog & foldertree sidebar.
      toolbar.rs                   // Main toolbar.
      grid.rs                      // Virtualized thumbnail grid.
      filter_popup.rs              // [NEW] Metadata Filters popup window.
    develop/
      canvas/                      // [NEW] Contains canvas rendering & overlays.
        mod.rs                     // Exports canvas widget.
        viewer.rs                  // The interactive image viewer (formerly drive_viewer, zoom, pan).
        overlays.rs                // Crop grids, mask brush overlays, EXIF badge, float tool palette.
      info_panel.rs                // [NEW] Read-only info sidebar (left drawer, replacing Info tab).
      tool_panel.rs                // Develop options panel orchestrating consolidated Light/Color/Effects tabs.
      base_tabs/                   // [NEW / REFACTORED]
        mod.rs                     // Exports base_tabs() registry.
        light.rs                   // Exposure/contrast sliders + ToneCurve widget.
        color.rs                   // HSL + ColorGrading wheels.
        effects.rs                 // Sharpen/NR + Optics.
    export_module/
      settings_panel.rs            // [NEW] The Export Settings panel (reverse conventional label/control layout).
      queue_panel.rs               // [NEW] Reusable export queue grid.
      bottom_bar.rs                // Bottom actions bar.
```

---

## 4. UI Library: Reusable Widgets

To support high styling consistency, every widget will pull color design tokens directly from `theme.rs` and enforce the V2 digital darkroom design:

### 4.1 `TabRow`
* **Layout:** Flat row of button tabs. No double lines.
* **Styling:**
  * Active: `#eaf1f6` text color + 2px accent underline (`#6d97b5`).
  * Inactive: `#9a9a9a` text color + transparent underline.

### 4.2 `SegmentedControl` (Chips)
* **Layout:** Row of contiguous or wrapped rounded rect pills/chips.
* **Styling:** 3px border radius. Active state gets `accent-fill` (`#232b30`) + `accent-border` (`#34464f`) + `accent-text` (`#cfe0ec`).

### 4.3 `ColorGradingWheel`
* **Layout:** Circular conic gradient selector representing Hue and Saturation, plus a luminance `EguiSlider` aligned underneath.
* **Styling:** Drawn using `Painter::circle_filled` / conic shader approximation. A white circle handle indicates selected coordinate. Center is white, outer edge is full saturation.

### 4.4 `ToneCurveWidget`
* **Layout:** Interactive curve graph. Supports two modes toggled inside the section:
  * **Point Curve:** Diagonal line with custom nodes. Draggable node points, double/right-click to delete.
  * **Parametric Curve:** Sliders at the bottom (Highlights/Lights/Darks/Shadows) modifying regions defined by split thresholds.

---

## 5. Reorganizing `develop/base_tabs.rs`

The V1 `base_tabs` implementation is spread across separate structs. We will consolidate them into exactly three structs:

### 5.1 `LightTab`
Combines basic exposure sliders, white balance, and the **Tone Curve** widget:
* **Collapsible Header:** "Tone Curve"
  * In point curve mode: displays the interactive chart.
  * In parametric curve mode: displays parametric region sliders (Highlights, Lights, Darks, Shadows) + split thresholds (Darks Split, Mid Split, Highlights Split).
* **Reset actions:** Independent reset arrows for Exposure/Contrast, WB, and Curve.

### 5.2 `ColorTab`
Combines Hue/Sat/Lum (HSL) adjustments and **Color Grading**:
* **HSL Section:** Row of 8 rounded color swatches (Red, Orange, Yellow, Green, Cyan, Blue, Purple, Pink). Selecting a swatch reveals Hue, Saturation, and Luminance sliders for that specific color range.
* **Collapsible Header:** "Color Grading"
  * Renders 4 wheels: Shadows, Midtones, Highlights, Global.
  * Includes Balance (-100..100) and Blending (0..100) sliders.

### 5.3 `EffectsTab`
Combines sharpening, noise reduction, and optics:
* **Sharpening:** Amount, Radius, Detail.
* **Noise Reduction:** Luminance, Detail, Color, Color Detail. Shows small "AI" label indicating future AI track expansion.
* **Collapsible Header:** "Optics"
  * Lens profile status and "Choose lens..." picker.
  * Distortion checkbox & Vignette slider.

---

## 6. Implementation Milestones

To manage risk and allow for structured visual verification, the implementation plan will be executed in three separate plans/milestones:

### Milestone 1: Structural Extraction & app.rs Split
Extract logic from `app.rs` without changing the V1 styling:
* **Task 1.1:** Move app-level event handling loops (`apply_full_decoded`, `apply_pyramid_ready`, `apply_lens_baked`, `set_preview_and_full`) to `src/app/controller.rs`.
* **Task 1.2:** Move app-level keyboard shortcuts to `src/app/shortcuts.rs`.
* **Task 1.3:** Extract left sidebar panels and export panels to separate modules (`src/library/panel.rs`, `src/export_module/settings_panel.rs`, `src/export_module/queue_panel.rs`).
* **Task 1.4:** Extract `drive_viewer` and overlays into `src/develop/canvas/`.
* *Validation:* Verify that the application builds, runs, and functions exactly like V1, but with a highly simplified `app.rs` (<1,200 lines).

### Milestone 2: Reusable UI Library Implementation
Implement the V2 design widgets in `src/widgets/`:
* **Task 2.1:** Implement the `TabRow` and `SegmentedControl` widgets.
* **Task 2.2:** Build the custom painter-based `ColorGradingWheel` widget.
* **Task 2.3:** Consolidate parametric and point curve rendering into the unified `ToneCurveWidget`.
* **Task 2.4:** Enhance `EguiSlider` to handle bipolar highlights and layout spacing matching the V2 specs.
* *Validation:* Add unit tests for widgets where applicable and verify they build correctly.

### Milestone 3: V2 Theme & Layout Integration
Combine the new widgets and modular panels to deliver the high-fidelity V2 visual redesign:
* **Task 3.1:** Re-theme base panels using V2 colors and typography tokens.
* **Task 3.2:** Reorganize Develop panels to use consolidated `LightTab`, `ColorTab`, and `EffectsTab` structures.
* **Task 3.3:** Build the Left Info Panel drawer and wire it to the floating `Info` pill button on the canvas.
* **Task 3.4:** Implement the V2 titlebar and header toolbar layout, including the floating Metadata Filters popup.
* *Validation:* Visually verify the application matches the design sheets (`Ferrolite.dc.html`, `EguiSlider.dc.html`) and check per-control reset behaviors.

---

## 7. Decisions Recorded (Resolved during Brainstorming, 2026-07-19)

The following decisions were finalized during the brainstorming phase:
1. **UI Library Placement (Option A):** Reuse and consolidate widgets under the existing [ferrolite-app/src/widgets/](file:///Users/schiba/Projects/ferrolite/ferrolite-app/src/widgets) directory rather than creating a new workspace crate. This prevents dependency cycle issues and theme duplication.
2. **Canvas State Isolation (Option A):** Extract interactive canvas state variables (pan offset, zoom, dragging coordinates) into a dedicated `ViewerCanvasState` struct inside `src/develop/canvas/` rather than keeping them flat on the global `ViewerState`.
3. **Centralized Overlays (Option A):** Group all floating overlay logic (palette, EXIF, histogram, brush indicator) in a single centralized overlay manager in `src/develop/canvas/overlays.rs` using `egui::Area`.
4. **App Controller Extraction (Option A):** Decouple WGPU-heavy and async event-handling methods from `app.rs` by placing them in an `AppController` context in `src/app/controller.rs`.
5. **Shortcuts Decoupling (Option A):** Centralize all keyboard shortcuts detection and key interception in `src/app/shortcuts.rs` called from the main frame update loop.
