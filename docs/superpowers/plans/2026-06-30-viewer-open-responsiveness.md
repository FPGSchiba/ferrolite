# Viewer Open Responsiveness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make double-clicking an image switch to the Develop viewer immediately, with a loading spinner during decode, so it never reads as "stuck."

**Architecture:** Two coordinated edits in `ferrolite-app`: (1) `open_record` requests an egui repaint after switching to Develop so the next frame draws the viewer without waiting for stray input; (2) `viewer::paint` shows a centered spinner + "Loading…" while the first pixel hasn't arrived.

**Tech Stack:** Rust 2021, egui/eframe 0.29.

## Global Constraints

- Stay on branch `feat/viewer-and-vt-ladder`; do NOT create a new branch. `cargo fmt --all` before committing. Keep `cargo clippy --workspace --all-targets -- -D warnings` exit 0 and `cargo test --workspace` green. Conventional commit, no attribution footer.
- App-layer only; do not touch `ferrolite-vt`/`ferrolite-gpu`/decode. No new repaint wiring beyond `open_record` (decode jobs + `drive_viewer` already repaint correctly).
- Spinner shows only while `!state.loaded` (the no-first-pixel window); it must vanish once the preview is uploaded.

---

## Task 1: Immediate switch + loading spinner

**Files:**
- Modify: `ferrolite-app/src/app.rs` — `open_record` signature + body; its 4 call sites.
- Modify: `ferrolite-app/src/viewer/mod.rs` — `paint` loading branch.

**Interfaces:**
- Changed: `fn open_record(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame, rec: &ferrolite_catalog::ImageRecord)` (adds `ctx`).
- `viewer::paint` signature is unchanged (`pub fn paint(ui: &mut egui::Ui, state: &mut ViewerState, show_full: bool) -> bool`); only its `!state.loaded` branch changes.

- [ ] **Step 1: Add `ctx` to `open_record` and request a repaint**

In `ferrolite-app/src/app.rs`, change the helper (currently around line 235):
```rust
/// The single image-open path: cancel the previously-open viewer's in-flight
/// tile jobs, open the new image's two-tier load, switch to Develop, and request
/// a repaint so the viewer is drawn on the very next frame (otherwise egui would
/// idle on the grid until the next input event, which reads as a stall).
fn open_record(
    &mut self,
    ctx: &egui::Context,
    frame: &mut eframe::Frame,
    rec: &ferrolite_catalog::ImageRecord,
) {
    if let Some(old) = self.state.viewer.as_ref() {
        let old_id = old.image_id;
        old.cancel_loads();
        self.cancel_viewer_tiles(frame, old_id);
    }
    self.state.open_image_in_viewer(rec);
    self.module = crate::module::Module::Develop;
    ctx.request_repaint();
}
```

- [ ] **Step 2: Update the 4 call sites to pass `ctx`**

In `app.rs::update`, every `self.open_record(frame, &rec)` becomes `self.open_record(ctx, frame, &rec)`. There are four (grid double-click dispatch after the central panel; filmstrip click dispatch after the toolbar panel; the Enter handler; the arrow-key handler). Find them with:
```
grep -n "open_record(" ferrolite-app/src/app.rs
```
Update each call to insert `ctx,` as the first argument. (egui's `&Context` is `ctx` in `update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame)`.)

- [ ] **Step 3: Build to verify the signature change compiles**

Run: `cargo build -p ferrolite-app`
Expected: success (all four call sites updated; if one was missed, the compiler names it).

- [ ] **Step 4: Add the loading spinner to `viewer::paint`**

In `ferrolite-app/src/viewer/mod.rs`, the `paint` function currently fills the rect black and, when `!state.loaded`, returns `true` (animate). Replace the trailing `else { /* not ready */ true }` branch so it also draws a centered spinner + label. The function already has `rect`, `viewport`, and `ui`. Change the final block:
```rust
    if state.loaded {
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewerCallback {
                image_id: state.image_id,
                view: state.view,
                viewport,
                show_full,
            },
        ));
        false
    } else {
        // First pixel not ready yet: show a spinner + "Loading…" so the decode
        // wait reads as working, and keep animating so it spins + we pick up the
        // preview as soon as it arrives.
        let center = rect.center();
        let spinner_size = 32.0;
        let spinner_rect = egui::Rect::from_center_size(
            center - egui::vec2(0.0, 10.0),
            egui::vec2(spinner_size, spinner_size),
        );
        ui.put(spinner_rect, egui::Spinner::new().size(spinner_size));
        ui.painter().text(
            center + egui::vec2(0.0, 22.0),
            egui::Align2::CENTER_CENTER,
            "Loading…",
            egui::FontId::proportional(12.0),
            crate::theme::TEXT_DIM,
        );
        true
    }
```
(`egui::Spinner` is in egui 0.29; `.size(..)` sets its diameter. `ui.put(rect, widget)` places a widget at an explicit rect. The black fill earlier in `paint` stays.)

- [ ] **Step 5: Verify build + lints + tests**

Run:
```bash
cargo build -p ferrolite-app
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: build OK; fmt clean; clippy exit 0; all tests pass (no test changes — the existing `viewer::` math/crossfade tests still pass; this is egui rendering with no new pure logic).

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/app.rs ferrolite-app/src/viewer/mod.rs
git commit -m "feat(app): instant viewer switch on open + loading spinner during decode"
```

- [ ] **Step 7: Manual smoke (user)**

Double-click a large RAW: the view switches to Develop immediately (no input needed to "unstick" it), a spinner + "Loading…" shows briefly, then the preview appears and sharpens to full-res. Repeat via filmstrip click, Enter, and arrow keys — each switches promptly.

---

## Self-Review

**Spec coverage:** §3.1 immediate repaint → Task 1 steps 1-2; §3.2 spinner → step 4; §4 files → both modified; §5 testing/gate → step 5. ✓

**Placeholder scan:** none — both edits show complete code.

**Type consistency:** `open_record(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame, rec: &ImageRecord)` used consistently; the 4 call sites all pass `ctx` first. `viewer::paint` signature unchanged; only the `!loaded` branch body changes. `egui::Spinner::new().size(..)`, `ui.put`, `egui::Align2::CENTER_CENTER`, `theme::TEXT_DIM` are existing APIs.
