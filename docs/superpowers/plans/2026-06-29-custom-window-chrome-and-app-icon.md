# Custom Window Chrome & App Icon Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the OS (winit) title bar with a single custom borderless title bar (drag, window controls, resize, 1px border) and give the app a designed Ferrolite icon (Concept A faceted-F) used both as the title-bar mark and the OS/taskbar window icon.

**Architecture:** A new `chrome` module owns the window-chrome concern, keeping `app.rs` as the panel-layout orchestrator. `main.rs` turns off native decorations and sets the window icon. The icon is defined **once** as axis-aligned rectangle geometry in `chrome/icon.rs`: the same geometry is painted in egui for the ~18px title-bar mark **and** software-rasterized to an RGBA buffer for the OS `IconData` — so no image/SVG runtime dependency and no binary asset divergence. (This refines the spec's "bundled PNG" to "procedurally generated RGBA," honoring the spec's minimal-dependency directive; the master SVG is still committed as the design source of truth.)

**Tech Stack:** Rust, egui/eframe 0.29 (`ViewportBuilder`, `ViewportCommand`, `UiBuilder`, `Painter`, `IconData`), wgpu 22. No new crate dependencies.

## Global Constraints

- **License:** GPL-3.0-only (`license.workspace = true`). (Architecture map §2.)
- **GUI:** pure egui + wgpu. No WebView, no Tauri. No new dependencies for this plan.
- **Design tokens authoritative:** colors from `docs/design/ferrolite-design-system.md` §2. ACCENT `#6d97b5`, ACCENT_BRIGHT (accent.bright) `#a9c7dd`, BORDER_STRONG `#2a2a2a`, BG_TITLEBAR `#161616`, BG_TOOLBAR `#1d1d1d`, TEXT_DIM `#8a8a8a`, semantic red `#c75450`.
- **Title/menu bar height:** 30px (design system §4). Tabs centered; window controls on the right (all 3 OSes).
- **Pinned versions:** eframe/egui/egui-wgpu 0.29, wgpu 22, toolchain `stable` / rust-version 1.85. egui API calls below are written for 0.29; if a signature differs on the resolved patch, adjust minimally and note it (same convention as the foundation plan).
- **Files focused:** 200–400 lines/file target, 800 max.
- **Frequent commits:** one commit per task minimum; conventional-commit messages.
- **Intermediate dead_code:** new `chrome` items are unused until `app.rs`/`main.rs` wire them (Tasks 4–5). `dead_code`/`unused` warnings are EXPECTED in Tasks 1–3 — do NOT add `#[allow(dead_code)]` and do NOT gate intermediate tasks on `clippy -D warnings`. The final task (5) must reach `clippy --all-targets -- -D warnings` clean.

---

## File structure (this plan)

```
ferrolite-app/
  assets/icon/
    ferrolite.svg                 # master Concept-A artwork (design source of truth; not loaded at runtime)
  src/
    chrome/
      mod.rs                      # `pub mod icon; pub mod window_controls;` + `title_bar(...)`
      icon.rs                     # F geometry consts; paint_mark() (egui) + icon_rgba() (RGBA raster) + tests
      window_controls.rs          # WindowAction + pure command() (+tests) + controls_ui() rendering
    theme.rs                      # add ACCENT_BRIGHT token; drop dead_code allows once consumed
    app.rs                        # call chrome::title_bar(...); add 1px window border
    main.rs                       # decorations(false), resizable, min size, with_icon(...), mod chrome;
    widgets/slider.rs             # use theme::ACCENT_BRIGHT (de-dup, mirrors earlier ACCENT consolidation)
```

---

### Task 1: Icon geometry, procedural mark, and RGBA rasterizer

**Files:**
- Create: `ferrolite-app/src/chrome/mod.rs`, `ferrolite-app/src/chrome/icon.rs`, `ferrolite-app/assets/icon/ferrolite.svg`
- Modify: `ferrolite-app/src/main.rs` (add `mod chrome;`), `ferrolite-app/src/theme.rs` (add `ACCENT_BRIGHT`), `ferrolite-app/src/widgets/slider.rs` (use `theme::ACCENT_BRIGHT`)

**Interfaces:**
- Consumes: `theme::{ACCENT, ACCENT_BRIGHT}`, `egui::{Painter, Rect, Color32, pos2}`.
- Produces:
  - `chrome::icon::paint_mark(painter: &egui::Painter, rect: egui::Rect)` — paints the faceted F (transparent bg) fitted into `rect` (used by the title bar).
  - `chrome::icon::icon_rgba(px: u32) -> Vec<u8>` — `px*px*4` RGBA8 of the F-on-rounded-tile (used for `IconData`).
  - F geometry constants (64-unit design space).

- [ ] **Step 1: Add the ACCENT_BRIGHT design token to theme.rs**

In `ferrolite-app/src/theme.rs`, add next to `ACCENT` (design system §2 accent.bright):
```rust
pub const ACCENT_BRIGHT: Color32 = Color32::from_rgb(0xa9, 0xc7, 0xdd);
```
Add a token test in theme.rs's `#[cfg(test)] mod tests`:
```rust
#[test]
fn accent_bright_token_matches_design_system() {
    assert_eq!(ACCENT_BRIGHT, Color32::from_rgb(169, 199, 221)); // #a9c7dd
}
```

- [ ] **Step 2: De-duplicate slider's ACCENT_BRIGHT (mirror the earlier ACCENT consolidation)**

In `ferrolite-app/src/widgets/slider.rs`, remove the local `const ACCENT_BRIGHT: Color32 = ...;` and replace its use(s) with `theme::ACCENT_BRIGHT` (the file already has `use crate::theme;`). Leave the other §5 widget-only tokens (TRACK, FILL_IDLE, HANDLE_IDLE, HANDLE_BORDER, LABEL, VALUE_IDLE) local.

- [ ] **Step 3: Create the master SVG (design source of truth)**

`ferrolite-app/assets/icon/ferrolite.svg`:
```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64">
  <rect x="2" y="2" width="60" height="60" rx="13" fill="#161a1f" stroke="#2a2a2a"/>
  <polygon points="20,16 46,16 46,25 29,25 29,31 42,31 42,40 29,40 29,49 20,49" fill="#6d97b5"/>
  <polygon points="20,16 46,16 46,25 29,25 29,31 20,31" fill="#a9c7dd" opacity="0.6"/>
</svg>
```

- [ ] **Step 4: Write the failing tests for the rasterizer**

Create `ferrolite-app/src/chrome/icon.rs` with the geometry + tests first (tests reference functions not yet written → RED):
```rust
//! Ferrolite app icon (Concept A "faceted F"). One geometry, two renderings:
//! `paint_mark` for the egui title-bar mark, `icon_rgba` for the OS IconData.
//! All geometry is in a 64x64 design space (see assets/icon/ferrolite.svg).

use crate::theme;
use egui::{Color32, Painter, Rect, Rounding};

// F is a union of axis-aligned rectangles in 64-space: (x0, y0, x1, y1).
const STEM: [f32; 4] = [20.0, 16.0, 29.0, 49.0];
const TOP_ARM: [f32; 4] = [29.0, 16.0, 46.0, 25.0];
const MID_ARM: [f32; 4] = [29.0, 31.0, 42.0, 40.0];
// Bright facet over the top band of the F (x 20..46, y 16..25), drawn semi-transparent.
const FACET: [f32; 4] = [20.0, 16.0, 46.0, 25.0];
const FACET_ALPHA: u8 = 153; // 0.6 * 255

// OS-icon tile (icon only; the title-bar mark is transparent).
const TILE_BG: Color32 = Color32::from_rgb(0x16, 0x1a, 0x1f);
const TILE_RECT: [f32; 4] = [2.0, 2.0, 62.0, 62.0];
const TILE_RADIUS: f32 = 13.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn px_at(buf: &[u8], px: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * px + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn rgba_buffer_has_correct_length() {
        assert_eq!(icon_rgba(64).len(), 64 * 64 * 4);
        assert_eq!(icon_rgba(32).len(), 32 * 32 * 4);
    }

    #[test]
    fn f_stem_pixel_is_accent() {
        // 64-space (24,35) is inside the stem; at px=64 that's pixel (24,35).
        let buf = icon_rgba(64);
        let [r, g, b, a] = px_at(&buf, 64, 24, 35);
        assert_eq!([r, g, b], [0x6d, 0x97, 0xb5]);
        assert_eq!(a, 255);
    }

    #[test]
    fn facet_band_pixel_is_brighter_than_accent() {
        // 64-space (24,20): stem within the bright facet band -> blended brighter.
        let buf = icon_rgba(64);
        let [_, _, b, a] = px_at(&buf, 64, 24, 20);
        assert!(b > 0xb5, "facet blue {b} should exceed accent blue 0xb5");
        assert_eq!(a, 255);
    }

    #[test]
    fn tile_interior_outside_f_is_tile_color() {
        // 64-space (40,45): inside tile, outside the F.
        let buf = icon_rgba(64);
        let [r, g, b, a] = px_at(&buf, 64, 40, 45);
        assert_eq!([r, g, b], [0x16, 0x1a, 0x1f]);
        assert_eq!(a, 255);
    }

    #[test]
    fn rounded_corner_is_transparent() {
        // 64-space (4,4): outside the rounded tile corner -> fully transparent.
        let buf = icon_rgba(64);
        let [_, _, _, a] = px_at(&buf, 64, 4, 4);
        assert_eq!(a, 0);
    }
}
```

- [ ] **Step 5: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-app icon::`
Expected: FAIL (compile error — `icon_rgba` not defined).

- [ ] **Step 6: Implement the rasterizer and the egui mark**

Append to `ferrolite-app/src/chrome/icon.rs`:
```rust
fn in_rect(x: f32, y: f32, r: &[f32; 4]) -> bool {
    x >= r[0] && x < r[2] && y >= r[1] && y < r[3]
}

/// Coverage of the rounded tile at design-space point (x,y): 1.0 inside, 0.0 outside.
fn tile_covered(x: f32, y: f32) -> bool {
    let [x0, y0, x1, y1] = TILE_RECT;
    if x < x0 || x >= x1 || y < y0 || y >= y1 {
        return false;
    }
    // Round the four corners.
    let r = TILE_RADIUS;
    let cx = if x < x0 + r {
        x0 + r
    } else if x > x1 - r {
        x1 - r
    } else {
        x
    };
    let cy = if y < y0 + r {
        y0 + r
    } else if y > y1 - r {
        y1 - r
    } else {
        y
    };
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

/// Returns the RGBA at design-space point (x,y), or None if fully transparent.
fn sample(x: f32, y: f32) -> Option<[u8; 4]> {
    if !tile_covered(x, y) {
        return None;
    }
    let mut col = TILE_BG.to_array(); // [r,g,b,a]
    if in_rect(x, y, &STEM) || in_rect(x, y, &TOP_ARM) || in_rect(x, y, &MID_ARM) {
        col = theme::ACCENT.to_array();
    }
    if in_rect(x, y, &FACET) {
        // Blend ACCENT_BRIGHT (alpha 0.6) over current color.
        let b = theme::ACCENT_BRIGHT.to_array();
        let a = FACET_ALPHA as u32;
        for i in 0..3 {
            col[i] = ((b[i] as u32 * a + col[i] as u32 * (255 - a)) / 255) as u8;
        }
    }
    col[3] = 255;
    Some(col)
}

/// `px*px*4` RGBA8 of the F-on-tile, 2x2 supersampled for smooth tile edges.
pub fn icon_rgba(px: u32) -> Vec<u8> {
    let scale = px as f32 / 64.0;
    let mut buf = vec![0u8; (px * px * 4) as usize];
    for y in 0..px {
        for x in 0..px {
            let mut acc = [0u32; 4];
            for sy in 0..2 {
                for sx in 0..2 {
                    let fx = (x as f32 + 0.25 + 0.5 * sx as f32) / scale;
                    let fy = (y as f32 + 0.25 + 0.5 * sy as f32) / scale;
                    if let Some(c) = sample(fx, fy) {
                        for i in 0..4 {
                            acc[i] += c[i] as u32;
                        }
                    }
                }
            }
            let i = ((y * px + x) * 4) as usize;
            for k in 0..4 {
                buf[i + k] = (acc[k] / 4) as u8;
            }
        }
    }
    buf
}

fn scaled(r: &[f32; 4], origin: egui::Pos2, s: f32) -> Rect {
    Rect::from_min_max(
        egui::pos2(origin.x + r[0] * s, origin.y + r[1] * s),
        egui::pos2(origin.x + r[2] * s, origin.y + r[3] * s),
    )
}

/// Paint the faceted-F mark (no tile, transparent bg) fitted into `rect`.
pub fn paint_mark(painter: &Painter, rect: Rect) {
    // The F occupies design-space x 20..46, y 16..49 (26 x 33). Fit it into rect.
    let s = (rect.width() / 26.0).min(rect.height() / 33.0);
    // origin so that design point (20,16) maps near rect.min, vertically centered.
    let origin = egui::pos2(rect.left() - 20.0 * s, rect.center().y - 32.5 * s);
    for r in [&STEM, &TOP_ARM, &MID_ARM] {
        painter.rect_filled(scaled(r, origin, s), Rounding::ZERO, theme::ACCENT);
    }
    let facet = Color32::from_rgba_unmultiplied(
        theme::ACCENT_BRIGHT.r(),
        theme::ACCENT_BRIGHT.g(),
        theme::ACCENT_BRIGHT.b(),
        FACET_ALPHA,
    );
    painter.rect_filled(scaled(&FACET, origin, s), Rounding::ZERO, facet);
}
```
Note: `Color32::to_array()` returns `[u8;4]`; `Rounding::ZERO` and `Painter::rect_filled(rect, rounding, color)` are egui 0.29. If `rect_filled` wants `impl Into<Rounding>`, `0.0` also works.

- [ ] **Step 7: Create the chrome module file**

`ferrolite-app/src/chrome/mod.rs`:
```rust
//! Custom window chrome: the borderless title bar, window controls, and app icon.
pub mod icon;
```
(The `window_controls` submodule and `title_bar` fn are added in Tasks 2–3.)

- [ ] **Step 8: Wire the chrome module into the crate**

In `ferrolite-app/src/main.rs`, add alongside the other `mod` lines:
```rust
mod chrome;
```

- [ ] **Step 9: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-app icon:: theme::`
Expected: PASS (icon: 5 tests, theme: 3 tests). `cargo build -p ferrolite-app` compiles (expect dead_code on `paint_mark` until Task 3 — that's fine).

- [ ] **Step 10: Commit**

```bash
git add ferrolite-app/src/chrome ferrolite-app/src/theme.rs ferrolite-app/src/widgets/slider.rs ferrolite-app/src/main.rs ferrolite-app/assets/icon
git commit -m "feat(app): Ferrolite faceted-F icon geometry, egui mark + RGBA rasterizer"
```

---

### Task 2: Window controls — pure command mapping + button rendering

**Files:**
- Create: `ferrolite-app/src/chrome/window_controls.rs`
- Modify: `ferrolite-app/src/chrome/mod.rs` (add `pub mod window_controls;`)

**Interfaces:**
- Consumes: `egui::{Ui, ViewportCommand, Color32, ...}`, `theme`.
- Produces:
  - `chrome::window_controls::WindowAction { Minimize, ToggleMaximize, Close }` (`Copy`, `PartialEq`, `Debug`).
  - `chrome::window_controls::command(action: WindowAction, is_maximized: bool) -> egui::ViewportCommand` (pure).
  - `chrome::window_controls::controls_ui(ui: &mut egui::Ui) -> Option<WindowAction>` — renders the three right-aligned buttons; returns the clicked action, if any.

- [ ] **Step 1: Write the failing tests for the pure mapping**

Create `ferrolite-app/src/chrome/window_controls.rs`:
```rust
//! Window control buttons (minimize / maximize-restore / close) for the
//! borderless title bar, plus the pure action->command mapping.

use egui::ViewportCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    Minimize,
    ToggleMaximize,
    Close,
}

/// Map a control action to the egui viewport command to send.
pub fn command(action: WindowAction, is_maximized: bool) -> ViewportCommand {
    match action {
        WindowAction::Minimize => ViewportCommand::Minimized(true),
        WindowAction::ToggleMaximize => ViewportCommand::Maximized(!is_maximized),
        WindowAction::Close => ViewportCommand::Close,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimize_maps_to_minimized_true() {
        assert!(matches!(
            command(WindowAction::Minimize, false),
            ViewportCommand::Minimized(true)
        ));
    }

    #[test]
    fn close_maps_to_close() {
        assert!(matches!(command(WindowAction::Close, true), ViewportCommand::Close));
    }

    #[test]
    fn toggle_maximize_flips_both_states() {
        assert!(matches!(
            command(WindowAction::ToggleMaximize, false),
            ViewportCommand::Maximized(true)
        ));
        assert!(matches!(
            command(WindowAction::ToggleMaximize, true),
            ViewportCommand::Maximized(false)
        ));
    }
}
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-app window_controls::`
Expected: PASS (3 tests). (`command`/`WindowAction` are used by the tests, so no dead_code on them yet; `controls_ui` is added next and will be dead until Task 3.)

- [ ] **Step 3: Add the button rendering**

Append to `ferrolite-app/src/chrome/window_controls.rs`:
```rust
use crate::theme;
use egui::{pos2, Align2, Color32, FontId, Rect, Sense, Stroke, Ui, Vec2};

const BTN_W: f32 = 44.0;
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xc7, 0x54, 0x50); // semantic red

/// Render the three window-control buttons right-to-left (close is rightmost).
/// Returns the action whose button was clicked this frame, if any.
pub fn controls_ui(ui: &mut Ui) -> Option<WindowAction> {
    let mut clicked = None;
    // Order matters in a right_to_left layout: first added sits rightmost.
    for (action, glyph) in [
        (WindowAction::Close, "\u{2715}"),         // ✕
        (WindowAction::ToggleMaximize, "\u{25A1}"), // □
        (WindowAction::Minimize, "\u{2013}"),       // –
    ] {
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(BTN_W, ui.available_height()), Sense::click());
        let hover = resp.hovered();
        if hover {
            let bg = if action == WindowAction::Close {
                CLOSE_HOVER
            } else {
                theme::BG_TOOLBAR
            };
            ui.painter().rect_filled(rect, 0.0, bg);
        }
        let fg = if hover && action == WindowAction::Close {
            Color32::WHITE
        } else {
            theme::TEXT_DIM
        };
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(13.0),
            fg,
        );
        if resp.clicked() {
            clicked = Some(action);
        }
        // silence unused imports for pos2/Rect/Stroke if not otherwise used:
        let _ = (pos2(0.0, 0.0), Rect::NOTHING, Stroke::NONE);
    }
    clicked
}
```
Note: trim the `let _ = ...` line and any unused imports to keep clippy clean — it is only a convenience to avoid churn while drafting. `BG_TOOLBAR` and `TEXT_DIM` are existing theme tokens. egui 0.29: `allocate_exact_size`, `Painter::text`, `Align2::CENTER_CENTER`, `FontId::proportional`.

- [ ] **Step 4: Export the submodule**

In `ferrolite-app/src/chrome/mod.rs`, add under the existing `pub mod icon;`:
```rust
pub mod window_controls;
```

- [ ] **Step 5: Verify build + tests**

Run: `cargo test -p ferrolite-app window_controls::` (3 pass) and `cargo build -p ferrolite-app` (compiles; `controls_ui` dead until Task 3 — expected).

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/chrome/window_controls.rs ferrolite-app/src/chrome/mod.rs
git commit -m "feat(app): window controls with pure action->ViewportCommand mapping"
```

---

### Task 3: Title bar assembly

**Files:**
- Modify: `ferrolite-app/src/chrome/mod.rs` (add `title_bar`)

**Interfaces:**
- Consumes: `module::Module`, `theme`, `chrome::icon`, `chrome::window_controls`, `egui::{Context, Ui, ...}`.
- Produces: `chrome::title_bar(ctx: &egui::Context, ui: &mut egui::Ui, module: &mut Module, version: &str)` — renders the full bar contents into `ui` (caller supplies the 30px `TopBottomPanel`), handling drag/maximize and window-control commands.

- [ ] **Step 1: Implement `title_bar`**

Append to `ferrolite-app/src/chrome/mod.rs`:
```rust
use crate::module::Module;
use crate::theme;
use egui::{vec2, Align, Context, Layout, Sense, UiBuilder};

/// Render the borderless title bar contents. `ui` is the 30px top panel's ui.
/// Left: icon + wordmark + menu labels. Center: Library/Develop tabs.
/// Right: version + window controls. Empty space drags the window.
pub fn title_bar(ctx: &Context, ui: &mut egui::Ui, module: &mut Module, version: &str) {
    let bar = ui.max_rect();

    // 1) Drag region over the whole bar (added first => lowest input priority;
    //    widgets drawn after take their clicks, empty space starts a window drag).
    let drag = ui.interact(bar, ui.id().with("titlebar_drag"), Sense::click_and_drag());
    if drag.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }

    // 2) Left cluster: icon mark + wordmark + menu labels.
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(bar)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.add_space(8.0);
            let (mark, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
            icon::paint_mark(ui.painter(), mark);
            ui.add_space(6.0);
            ui.label("FERROLITE");
            ui.add_space(12.0);
            for m in ["File", "Edit", "Photo", "View", "Help"] {
                ui.colored_label(theme::TEXT_DIM, m);
            }
        },
    );

    // 3) Center cluster: module tabs, horizontally centered in the bar.
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(bar)
            .layout(Layout::top_down(Align::Center)),
        |ui| {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(module.is_library(), "Library")
                    .clicked()
                {
                    *module = Module::Library;
                }
                if ui
                    .selectable_label(!module.is_library(), "Develop")
                    .clicked()
                {
                    *module = Module::Develop;
                }
            });
        },
    );

    // 4) Right cluster: window controls (rightmost) then version.
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(bar)
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            if let Some(action) = window_controls::controls_ui(ui) {
                let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                ctx.send_viewport_cmd(window_controls::command(action, max));
            }
            ui.add_space(8.0);
            ui.monospace(version);
        },
    );
}
```
Note: `UiBuilder` + `Ui::allocate_new_ui` are egui 0.29 (they replace the deprecated `allocate_ui_at_rect`). The three clusters intentionally share `bar` as their `max_rect`; egui resolves overlapping widget interaction by draw order. `ctx.input(|i| i.viewport().maximized)` is `Option<bool>`. If any signature differs on the resolved 0.29 patch, adjust minimally and note it.

- [ ] **Step 2: Verify build + existing tests**

Run: `cargo build -p ferrolite-app` (compiles; `title_bar` dead until Task 5 — expected) and `cargo test -p ferrolite-app` (existing tests still pass).

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/chrome/mod.rs
git commit -m "feat(app): assemble borderless title bar (drag, centered tabs, controls)"
```

---

### Task 4: Borderless window + app icon (main.rs)

**Files:**
- Modify: `ferrolite-app/src/main.rs`

**Interfaces:**
- Consumes: `chrome::icon::icon_rgba`, `eframe::NativeOptions`, `egui::{ViewportBuilder, IconData}`.
- Produces: a borderless, resizable window with a min size and the Ferrolite icon.

- [ ] **Step 1: Update NativeOptions**

Rewrite the `native_options` in `ferrolite-app/src/main.rs`:
```rust
fn main() -> eframe::Result<()> {
    let icon = egui::IconData {
        rgba: chrome::icon::icon_rgba(256),
        width: 256,
        height: 256,
    };
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 810.0])
            .with_min_inner_size([960.0, 600.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "Ferrolite",
        native_options,
        Box::new(|cc| Ok(Box::new(app::FerroliteApp::new(cc)))),
    )
}
```
Note: `ViewportBuilder::with_icon` takes `impl Into<Arc<IconData>>`; `Arc::new(icon)` satisfies it. `with_decorations(false)`, `with_resizable(true)`, `with_min_inner_size` are egui 0.29.

- [ ] **Step 2: Verify build**

Run: `cargo build -p ferrolite-app`
Expected: compiles. (`title_bar` may still warn dead until Task 5; the app window now has no OS decorations and the Ferrolite icon, but the custom bar isn't wired yet — that's the next task.)

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/main.rs
git commit -m "feat(app): borderless window with min size and Ferrolite icon"
```

---

### Task 5: Integrate the bar into the app, add window border + resize, finalize

**Files:**
- Modify: `ferrolite-app/src/app.rs`, `ferrolite-app/src/theme.rs` (drop now-unneeded `#[allow(dead_code)]`)

**Interfaces:**
- Consumes: `chrome::title_bar`, `theme::BORDER_STRONG`, `egui::{ViewportCommand, ResizeDirection, ...}`.
- Produces: the assembled app with one custom title bar, a 1px window border, and window edge-resize.

- [ ] **Step 1: Replace the inline title bar with `chrome::title_bar`**

In `ferrolite-app/src/app.rs`, replace the entire `egui::TopBottomPanel::top("titlebar")...show(ctx, |ui| { ... });` block with:
```rust
egui::TopBottomPanel::top("titlebar")
    .exact_height(30.0)
    .frame(egui::Frame::none().fill(theme::BG_TITLEBAR))
    .show(ctx, |ui| {
        crate::chrome::title_bar(ctx, ui, &mut self.module, "v0.0.1");
    });
```
Remove the now-unused `use crate::module::Module;`? Keep it — `self.module` field type still references `Module` via the struct definition; the `use` is still needed for the field type. Leave imports that are still used; remove any that clippy flags.

- [ ] **Step 2: Add the 1px window border**

Still in `app.rs`, change the `CentralPanel` frame to add an outer stroke (the central panel fills the remaining area to the window edges, so its stroke draws the window border on the left/right/bottom; the title bar covers the top edge). Replace the central panel block with:
```rust
egui::CentralPanel::default()
    .frame(
        egui::Frame::none()
            .fill(theme::BG_CANVAS)
            .stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG)),
    )
    .show(ctx, |ui| {
        let rect = ui.available_rect_before_wrap();
        canvas::paint(ui, rect);
    });
```
If the resulting inner border double-draws against the side panel, prefer painting a single full-window 1px rect stroke once per frame instead: at the end of `update`, `ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("win_border"))).rect_stroke(ctx.screen_rect().shrink(0.5), 0.0, egui::Stroke::new(1.0, theme::BORDER_STRONG));`. Choose whichever renders a clean single-pixel frame; document which you used.

- [ ] **Step 3: Add window edge-resize (with contingency)**

Add a resize affordance. First try relying on `with_resizable(true)` alone — run the app (Step 6) and check whether dragging the window edges resizes. If it does NOT (undecorated windows often need manual hit-testing), add invisible resize-grips at the end of `update`:
```rust
fn window_resize_grips(ctx: &egui::Context) {
    use egui::{Id, LayerId, Order, Rect, ResizeDirection, Sense, ViewportCommand};
    let r = ctx.screen_rect();
    let m = 6.0; // grip thickness
    let edges = [
        (Rect::from_min_max(r.left_top(), egui::pos2(r.right(), r.top() + m)), ResizeDirection::North, egui::CursorIcon::ResizeVertical),
        (Rect::from_min_max(egui::pos2(r.left(), r.bottom() - m), r.right_bottom()), ResizeDirection::South, egui::CursorIcon::ResizeVertical),
        (Rect::from_min_max(r.left_top(), egui::pos2(r.left() + m, r.bottom())), ResizeDirection::West, egui::CursorIcon::ResizeHorizontal),
        (Rect::from_min_max(egui::pos2(r.right() - m, r.top()), r.right_bottom()), ResizeDirection::East, egui::CursorIcon::ResizeHorizontal),
    ];
    let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("resize_grips")));
    for (i, (rect, dir, cursor)) in edges.into_iter().enumerate() {
        let resp = ctx.interact(LayerId::new(Order::Foreground, Id::new("resize_grips")), Id::new(("grip", i)), rect, Sense::drag());
        // alternative if ctx.interact signature differs: allocate via a transparent Area.
        if resp.hovered() { ctx.set_cursor_icon(cursor); }
        if resp.drag_started() { ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir)); }
        let _ = &painter;
    }
}
```
Call `window_resize_grips(ctx)` at the end of `update` only if needed. Note: the exact `ctx.interact(...)` signature is egui-0.29-specific; if it differs, place four transparent `egui::Area`s with `Sense::drag()` over the edges instead. Record in the report which path was used and whether built-in resize already worked.

- [ ] **Step 4: Drop the now-unnecessary dead_code allows in theme.rs**

`BORDER_STRONG` is now used (border) and `TEXT_DIM` is now used (menu labels + controls). In `ferrolite-app/src/theme.rs`, remove the `#[allow(dead_code)]` attributes from `BORDER_STRONG` and `TEXT_DIM` (and the now-stale palette comment if it no longer applies). If any token remains genuinely unused, keep a single documented allow on just that token.

- [ ] **Step 5: Full gate — fmt, clippy, tests**

Run:
```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```
Expected: fmt clean; clippy ZERO warnings; tests pass (module 2 + slider 6 + theme 3 + icon 5 + window_controls 3 = 19). Fix any real clippy lints; there must be no remaining `dead_code` (all chrome items are now wired).

- [ ] **Step 6: Manual visual verification (record results)**

Run: `cargo run -p ferrolite-app`. Confirm each:
- Only ONE title bar (no OS/winit bar above ours).
- The faceted-F mark + `FERROLITE` show at the left; `Library | Develop` centered; `v0.0.1` + window controls at the right.
- Dragging the empty bar area moves the window; double-clicking it maximizes/restores.
- Minimize, maximize/restore, and close buttons work; close button turns red on hover.
- Window edges resize the window (built-in or via grips).
- A 1px `#2a2a2a` border frames the window; central canvas still shows the steel-blue gradient.
- The taskbar / window icon is the Ferrolite faceted-F (not eframe's default).

Record the outcome (and which resize path was used) in the report. (macOS visual check may be deferred to a mac user; CI still builds all 3 OSes.)

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/app.rs ferrolite-app/src/theme.rs
git commit -m "feat(app): wire custom title bar, window border, and edge-resize — single bar"
```

---

## Self-Review

**Spec coverage (against the design doc):**
- Borderless window / decorations off, min size, resizable → Task 4. ✓
- Single 30px title bar, tabs centered, left logo+menus, right version+controls → Task 3 + Task 5 (wiring). ✓
- Drag-to-move + double-click maximize → Task 3. ✓
- Window controls (min/max/close, close-hover red) + pure tested mapping → Task 2. ✓
- Resize (with contingency grips) → Task 5 Step 3. ✓
- 1px window border → Task 5 Step 2. ✓
- Icon Concept A: exact geometry; procedural egui mark + RGBA for IconData; master SVG committed → Task 1 + Task 4 (with_icon). ✓
- Pure logic unit-tested (command mapping, icon raster, tokens); rendering via visual gate → Tasks 1, 2 tests; Task 5 Step 6. ✓
- No new dependencies; tokens authoritative; conventional commits; final clippy clean → Global Constraints + Task 5. ✓

**Placeholder scan:** No TBD/TODO. The `let _ = ...`/contingency lines in Tasks 2 and 5 carry explicit removal/selection instructions and a stated reason (egui API-drift convenience), mirroring the foundation plan's accepted convention — not logic placeholders. The resize approach has a concrete primary path plus a concrete fallback, both with code.

**Type consistency:** `WindowAction`/`command` defined in Task 2, used in Task 3. `icon::paint_mark`/`icon::icon_rgba` defined in Task 1, used in Tasks 3/4. `theme::ACCENT_BRIGHT` added in Task 1, used in Task 1 (icon) and slider. `chrome::title_bar(ctx, ui, &mut Module, &str)` defined in Task 3, called in Task 5. Consistent. ✓

**Known execution risk:** egui 0.29 immediate-mode layout (`UiBuilder`/`allocate_new_ui`, overlapping cluster UIs for centered tabs), `ViewportCommand::{StartDrag,BeginResize}` behavior on undecorated Windows, and `with_icon` Arc conversion are the API-drift-prone points; each carries an inline "adjust to 0.29 / contingency" note. The rust-build-resolver agent is available if a signature differs.
