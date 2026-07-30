# UI V2 Rewrite: Milestone 1 — Structural Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Decouple layout, canvas navigation, shortcuts, and event handling from `app.rs` into modular sub-files to reduce `app.rs` length from 4762 lines to <1200 lines, preparing the codebase for the V2 design rewrite.

**Architecture:** Create an `AppController` context to delegate async background callbacks, a `shortcuts` dispatcher to centralize user input commands, and a `ViewerCanvas` module to contain the heavy interactive canvas viewport loop and overlays.

**Tech Stack:** Rust, egui, eframe, wgpu, ferrolite-app

## Global Constraints
* No changes to WGPU pipeline nodes (`wgpu` compute shaders).
* No changes to SQLite catalog database schema.
* No changes to the RAW decoding pipeline (`rawler`).
* Maintain per-control reset functionality on all widgets.
* The workspace must compile and run successfully after every task.

---

### Task 1: Shortcuts Extraction

Move keyboard shortcuts processing out of `app.rs`'s `update()` method to `src/app/shortcuts.rs`.

**Files:**
* Create: `ferrolite-app/src/app/shortcuts.rs`
* Modify: `ferrolite-app/src/app.rs`
* Modify: `ferrolite-app/src/lib.rs`

**Interfaces:**
* Consumes: `AppState`, `Module`, `eframe::Frame`, `egui::Context`
* Produces: `shortcuts::dispatch(ctx, app, frame)` function

- [ ] **Step 1: Create the new shortcuts module file**
  Create [shortcuts.rs](file:///Users/schiba/Projects/ferrolite/ferrolite-app/src/app/shortcuts.rs) and declare the `dispatch` function:
  ```rust
  use crate::app::FerroliteApp;
  
  pub fn dispatch(ctx: &egui::Context, app: &mut FerroliteApp, frame: &mut eframe::Frame) {
      // Keypress shortcuts logic extracted from app.rs
  }
  ```

- [ ] **Step 2: Add shortcuts module to crate root**
  Modify [lib.rs](file:///Users/schiba/Projects/ferrolite/ferrolite-app/src/lib.rs) or [main.rs](file:///Users/schiba/Projects/ferrolite/ferrolite-app/src/main.rs) (whichever declares `app.rs` sub-modules) to declare `pub mod app;` and add `pub mod shortcuts;` inside the `app` namespace:
  ```rust
  // Inside ferrolite-app/src/lib.rs or app.rs
  pub mod app {
      pub mod shortcuts;
  }
  ```

- [ ] **Step 3: Extract shortcuts from app.rs**
  Identify keypress handling lines (roughly lines 3617 to 4124 in `app.rs`). Move them into `shortcuts::dispatch`. Adjust functions that are private to `FerroliteApp` by exposing them as public helpers inside `app.rs` (e.g., `pub(crate) fn navigate_step`, `pub(crate) fn apply_undo_redo`, `pub(crate) fn toggle_split_compare`).
  Replace the block in `app.rs` with:
  ```rust
  if !self.modal_active() {
      crate::app::shortcuts::dispatch(ctx, self, frame);
  }
  ```

- [ ] **Step 4: Verify compilation & run tests**
  Run: `cargo clippy --bin ferrolite-app --all-targets`
  Expected: Success with no compiler warnings.
  Run: test key combinations (F1, Ctrl+A, Esc, Zoom keys) inside the running app to ensure they behave exactly as before.

- [ ] **Step 5: Commit changes**
  ```bash
  git add ferrolite-app/src/app/shortcuts.rs ferrolite-app/src/app.rs
  git commit -m "refactor: extract keyboard shortcuts to src/app/shortcuts.rs"
  ```

---

### Task 2: Event Controller Extraction

Decouple async background loader event routing from `app.rs` into `src/app/controller.rs`.

**Files:**
* Create: `ferrolite-app/src/app/controller.rs`
* Modify: `ferrolite-app/src/app.rs`
* Modify: `ferrolite-app/src/lib.rs`

**Interfaces:**
* Consumes: `FerroliteApp`, `AppEvent`
* Produces: `AppController::handle_events(app, ctx, frame)`

- [ ] **Step 1: Create the controller file**
  Create [controller.rs](file:///Users/schiba/Projects/ferrolite/ferrolite-app/src/app/controller.rs):
  ```rust
  use crate::app::FerroliteApp;
  use crate::events::AppEvent;
  
  pub struct AppController;
  
  impl AppController {
      pub fn handle_events(app: &mut FerroliteApp, ctx: &egui::Context, frame: &eframe::Frame) {
          while let Ok(event) = app.state.rx.try_recv() {
              match event {
                  AppEvent::PreviewReady { image_id, linear } => {
                      app.apply_preview_ready(frame, ctx, image_id, &linear);
                      app.state.dirty = true;
                  }
                  // Map other event types...
              }
          }
      }
  }
  ```

- [ ] **Step 2: Declare controller in library root**
  Declare `pub mod controller;` inside the `app` module.

- [ ] **Step 3: Move helper callback methods out of app.rs**
  Identify the event response methods in `app.rs` (`apply_preview_ready`, `apply_full_decoded`, `apply_pyramid_ready`, `apply_preview_cache_hit`, `apply_preview_cache_miss`, `apply_lens_baked`, `set_preview_and_full`, `apply_edit`, `rebuild_mask_overlay_if_needed`, `maybe_spawn_lens_bake`, `try_auto_match_lens`, `apply_display_tail`, `redetect_display_profile`, `apply_working_space`).
  Move these methods to `controller.rs` as direct functions or methods on a controller context, making sure they mutate `FerroliteApp` appropriately and leverage its fields.
  Replace the event matching loop in `app.rs` `update()` with:
  ```rust
  crate::app::controller::AppController::handle_events(self, ctx, frame);
  ```

- [ ] **Step 4: Verify compilation & run tests**
  Run: `cargo clippy --bin ferrolite-app --all-targets`
  Expected: Success.
  Run: Load photos and edit them in Develop to ensure event pipelines (decoding, pyramid residency, bakes) execute without issues.

- [ ] **Step 5: Commit changes**
  ```bash
  git add ferrolite-app/src/app/controller.rs ferrolite-app/src/app.rs
  git commit -m "refactor: extract event handling to src/app/controller.rs"
  ```

---

### Task 3: Canvas Viewer Extraction

Decouple the canvas rendering, interactive pan/zoom calculation (`drive_viewer`), and canvas drawing from `app.rs` into `src/develop/canvas/`.

**Files:**
* Create: `ferrolite-app/src/develop/canvas/mod.rs`
* Create: `ferrolite-app/src/develop/canvas/viewer.rs`
* Create: `ferrolite-app/src/develop/canvas/overlays.rs`
* Modify: `ferrolite-app/src/app.rs`
* Modify: `ferrolite-app/src/state.rs`

**Interfaces:**
* Consumes: `AppState`, `eframe::Frame`, `egui::Ui`
* Produces: `canvas::Viewer::draw(ui, state, frame)`

- [ ] **Step 1: Define Canvas structures & CanvasState**
  Create a canvas state structure in `ferrolite-app/src/state.rs` or inside the new [develop/canvas/mod.rs](file:///Users/schiba/Projects/ferrolite/ferrolite-app/src/develop/canvas/mod.rs):
  ```rust
  #[derive(Default)]
  pub struct ViewerCanvasState {
      pub view: ferrolite_vt::ViewTransform,
      pub drag_start: Option<egui::Pos2>,
      // Other pan/zoom/crop drag state parameters moved from AppState/ViewerState
  }
  ```

- [ ] **Step 2: Move drive_viewer and math helpers**
  Move the complete interactive loop `drive_viewer` (lines 2223 to 2636 in `app.rs`) to `src/develop/canvas/viewer.rs`. Convert internal helper references (like `self.state.viewer`, `self.state.textures`) to draw parameters.
  
- [ ] **Step 3: Move overlays to centralized overlays manager**
  Create [overlays.rs](file:///Users/schiba/Projects/ferrolite/ferrolite-app/src/develop/canvas/overlays.rs). Move canvas overlays (tool palette, floating histogram, EXIF chip, crop guidelines) into standard drawing functions.
  
- [ ] **Step 4: Connect canvas rendering to CentralPanel in app.rs**
  In the `CentralPanel` section of `app.rs`, replace the raw `drive_viewer` call with the new canvas viewer module call:
  ```rust
  crate::develop::canvas::Viewer::new(image_id).show(ui, &mut self.state, frame);
  ```

- [ ] **Step 5: Verify build & interact with canvas**
  Run: `cargo test --workspace` and `cargo clippy`
  Expected: Success.
  Run: Drag, scroll/zoom, crop, and draw mask brushes on a sample raw image to verify coordinates are preserved.

- [ ] **Step 6: Commit changes**
  ```bash
  git add ferrolite-app/src/develop/canvas/ ferrolite-app/src/app.rs ferrolite-app/src/state.rs
  git commit -m "refactor: extract canvas viewer and overlays to develop/canvas"
  ```
