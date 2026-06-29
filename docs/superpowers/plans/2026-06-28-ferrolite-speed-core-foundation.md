# Ferrolite Speed Core — Plan 1: Foundation & Gate 0

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the Cargo workspace, CI on all three OSes, and a themed egui application whose central panel renders a live wgpu canvas, with the reusable `EguiSlider` widget working — i.e. clear **Gate 0** (the architecture map's de-risking gate) and establish the design system.

**Architecture:** A virtual Cargo workspace. One crate this plan: `ferrolite-app` (eframe + egui with the wgpu backend). The app boots an egui shell laid out to the design-system Library skeleton, applies the dark theme + bundled IBM Plex fonts, hosts a custom wgpu render callback in the central canvas region (proving egui↔wgpu texture integration), and provides the `EguiSlider` custom widget whose value math is pure and unit-tested.

**Tech Stack:** Rust (edition 2021), `eframe`/`egui` with `wgpu` feature, `egui_wgpu`, `wgpu`, WGSL, GitHub Actions.

## Global Constraints

- **License:** GPL-3.0-only. Every crate's `Cargo.toml` sets `license = "GPL-3.0-only"`. (Architecture map §2.)
- **Engine-transferable tiers stay permissive-dep:** `ferrolite-image`/`jobs`/`gpu`/`vt` may depend only on permissive crates (`wgpu`, `rayon`, `wide`, `std::simd`). `ferrolite-app` is the GPL binary and may depend on anything. (This plan only creates `ferrolite-app`.)
- **GUI:** pure egui + wgpu canvas. No WebView, no Tauri. (Architecture map §2.)
- **Fonts bundled, never fetched at runtime:** IBM Plex Sans + IBM Plex Mono embedded via `include_bytes!` (offline desktop app). (Design system §3.)
- **Design tokens are authoritative:** colors/typography/metrics come from `docs/design/ferrolite-design-system.md`. Token consolidation is allowed; visual result must match.
- **Files focused:** target 200–400 lines/file, 800 max. (User coding-style rule.)
- **Frequent commits:** one commit per task minimum; conventional-commit messages (`feat:`, `chore:`, `test:`, `ci:`).

---

## Plan sequence for Spec 1 (this is Plan 1 of 5)

Each plan produces a working, testable deliverable. Later plans are written when reached (their GPU/VT code is best authored against the validated foundation).

1. **Foundation & Gate 0 (this plan)** — workspace, CI, themed egui shell, wgpu canvas, `EguiSlider`. Deliverable: app boots on Win/mac/Linux, themed, slider works, canvas renders via wgpu. (= VT rung 1 substrate.)
2. **Decode & Catalog** — `ferrolite-image` vocabulary, `ferrolite-decode` (rawler preview/full/metadata), `ferrolite-catalog` (schema, ingest, `ThumbnailStore` SQLite-blob impl, queries). Deliverable: ingest a folder, generate thumbnails, query them.
3. **Jobs & Library grid** — `ferrolite-jobs` scheduler (priority/cancel/progress), wire ingest+thumbnail as jobs, virtualized Library grid + left panel + live status bar. Deliverable: browse a real folder fast. Benchmark M1/M2.
4. **Viewer & VT ladder** — `ferrolite-gpu` (context + executor skeleton), `ferrolite-vt` rungs 1→4, two-tier preview→full load. Deliverable: smooth zoom/pan. Benchmark M3–M5.
5. **Benchmark harness & milestone** — head-to-head vs RawTherapee, validation-milestone decision.

---

## File structure (this plan)

```
Cargo.toml                                  # virtual workspace manifest
rust-toolchain.toml                         # pinned toolchain
.github/workflows/ci.yml                    # fmt + clippy + build + test on 3 OSes
ferrolite-app/                              # crate lives at the repo root (flat layout, no crates/ dir)
  Cargo.toml
  assets/fonts/                             # IBMPlexSans-Regular/Medium/SemiBold.ttf, IBMPlexMono-Regular/Medium.ttf
  src/
    main.rs                                 # eframe::run_native entrypoint (wgpu backend)
    app.rs                                  # FerroliteApp + eframe::App impl (shell layout, module switch)
    theme.rs                                # color tokens + Visuals override + font install
    module.rs                               # Module enum + pure switch logic (testable)
    widgets/
      mod.rs
      slider.rs                             # EguiSlider widget + pure value-math module (testable)
    canvas/
      mod.rs
      callback.rs                           # wgpu CallbackTrait impl (pipeline + paint)
      shader.wgsl                           # fullscreen gradient (Gate 0 proof)
```

---

### Task 1: Workspace, toolchain, and green CI

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore` (append), `.github/workflows/ci.yml`
- Create: `ferrolite-app/Cargo.toml`, `ferrolite-app/src/main.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: a buildable `ferrolite-app` binary that prints and exits; workspace other plans extend.

- [ ] **Step 1: Create the workspace manifest**

`Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["ferrolite-app"]

[workspace.package]
edition = "2021"
license = "GPL-3.0-only"
rust-version = "1.82"

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }
```

- [ ] **Step 2: Pin the toolchain**

`rust-toolchain.toml`:
```toml
[toolchain]
channel = "1.82.0"
components = ["rustfmt", "clippy"]
```

- [ ] **Step 3: Create the app crate manifest**

`ferrolite-app/Cargo.toml`:
```toml
[package]
name = "ferrolite-app"
version = "0.0.1"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
eframe = { version = "0.29", default-features = false, features = ["wgpu", "default_fonts"] }
egui = "0.29"
egui_wgpu = "0.29"
wgpu = "22"
```

Note: confirm the newest matching versions with `cargo add` during execution; keep `eframe`, `egui`, `egui_wgpu` on the **same** minor version, and `wgpu` matching what that `egui_wgpu` re-exports (check `cargo tree -i wgpu`).

- [ ] **Step 4: Minimal main so the workspace builds**

`ferrolite-app/src/main.rs`:
```rust
fn main() {
    println!("ferrolite-app skeleton");
}
```

- [ ] **Step 5: Append build artifacts to .gitignore**

Append to `.gitignore`:
```
/target
```

- [ ] **Step 6: Create CI workflow**

`.github/workflows/ci.yml`:
```yaml
name: ci
on:
  push:
  pull_request:
jobs:
  check:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - name: Install Linux GUI deps
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libxkbcommon-dev libwayland-dev libxcb1-dev \
            libxkbcommon-x11-dev pkg-config
      - uses: dtolnay/rust-toolchain@1.82.0
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all -- --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo build --all-targets
      - run: cargo test --all
```

- [ ] **Step 7: Verify it builds and is formatted locally**

Run: `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo build`
Expected: all succeed, `ferrolite-app` builds.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore .github ferrolite-app
git commit -m "chore: scaffold cargo workspace, ferrolite-app crate, and CI"
```

---

### Task 2: Module state with pure, testable switch logic

**Files:**
- Create: `ferrolite-app/src/module.rs`
- Modify: `ferrolite-app/src/main.rs` (add `mod module;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `enum Module { Library, Develop }`; `Module::is_library(&self) -> bool`; `Module::default() -> Module` (= `Library`).

- [ ] **Step 1: Write the failing test**

`ferrolite-app/src/module.rs`:
```rust
//! Top-level UI module selection (Library vs Develop).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Module {
    #[default]
    Library,
    Develop,
}

impl Module {
    pub fn is_library(self) -> bool {
        matches!(self, Module::Library)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_library() {
        assert_eq!(Module::default(), Module::Library);
        assert!(Module::default().is_library());
    }

    #[test]
    fn develop_is_not_library() {
        assert!(!Module::Develop.is_library());
    }
}
```

- [ ] **Step 2: Wire the module into the crate**

In `ferrolite-app/src/main.rs`, add at the top:
```rust
mod module;
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-app module::`
Expected: PASS (2 tests).

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/module.rs ferrolite-app/src/main.rs
git commit -m "feat(app): add Module enum with switch logic"
```

---

### Task 3: EguiSlider value math (pure functions, TDD)

**Files:**
- Create: `ferrolite-app/src/widgets/mod.rs`, `ferrolite-app/src/widgets/slider.rs`
- Modify: `ferrolite-app/src/main.rs` (add `mod widgets;`)

**Interfaces:**
- Consumes: nothing.
- Produces (pure math, in `slider::math`):
  - `fraction(value: f32, min: f32, max: f32) -> f32` — clamped 0..=1.
  - `snap(value: f32, step: f32, min: f32, max: f32) -> f32` — round to step, clamp.
  - `value_at(fraction: f32, min: f32, max: f32, step: f32) -> f32` — fraction→snapped value.
  - `fill(frac: f32, min: f32, max: f32, bipolar: bool) -> (f32, f32)` — `(left, width)` in 0..=1.
  - `format(value: f32, decimals: usize, unit: &str, signed: bool) -> String`.

These mirror the imported `EguiSlider.dc.html` behavior (design system §5).

- [ ] **Step 1: Write the failing tests**

`ferrolite-app/src/widgets/slider.rs`:
```rust
//! Lightroom-style horizontal slider. See docs/design/ferrolite-design-system.md §5.

/// Pure value math, independent of egui — unit tested.
pub mod math {
    pub fn fraction(value: f32, min: f32, max: f32) -> f32 {
        if (max - min).abs() < f32::EPSILON {
            return 0.0;
        }
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    }

    pub fn snap(value: f32, step: f32, min: f32, max: f32) -> f32 {
        let snapped = if step > 0.0 {
            (value / step).round() * step
        } else {
            value
        };
        snapped.clamp(min, max)
    }

    pub fn value_at(fraction: f32, min: f32, max: f32, step: f32) -> f32 {
        let raw = min + fraction.clamp(0.0, 1.0) * (max - min);
        snap(raw, step, min, max)
    }

    /// Returns (left, width) in 0..=1 of the filled portion of the track.
    pub fn fill(frac: f32, min: f32, max: f32, bipolar: bool) -> (f32, f32) {
        if bipolar {
            let zero = fraction(0.0, min, max);
            let a = zero.min(frac);
            let b = zero.max(frac);
            (a, b - a)
        } else {
            (0.0, frac)
        }
    }

    pub fn format(value: f32, decimals: usize, unit: &str, signed: bool) -> String {
        let sign = if signed && value > 0.0 { "+" } else { "" };
        format!("{sign}{value:.decimals$}{unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::math::*;

    #[test]
    fn fraction_is_clamped() {
        assert_eq!(fraction(50.0, 0.0, 100.0), 0.5);
        assert_eq!(fraction(-10.0, 0.0, 100.0), 0.0);
        assert_eq!(fraction(200.0, 0.0, 100.0), 1.0);
    }

    #[test]
    fn snap_rounds_to_step_and_clamps() {
        assert_eq!(snap(103.0, 50.0, 50.0, 25600.0), 100.0);
        assert_eq!(snap(40.0, 50.0, 50.0, 25600.0), 50.0); // clamps up to min
    }

    #[test]
    fn value_at_maps_fraction() {
        // ISO slider: min 50, max 25600, step 50, midpoint
        let v = value_at(0.5, 50.0, 25600.0, 50.0);
        assert!((v - 12800.0).abs() <= 50.0);
    }

    #[test]
    fn unipolar_fill_runs_from_left() {
        assert_eq!(fill(0.7, 0.0, 100.0, false), (0.0, 0.7));
    }

    #[test]
    fn bipolar_fill_spans_zero() {
        // min -100, max 100; value -50 -> frac 0.25; zero -> 0.5
        let (left, width) = fill(0.25, -100.0, 100.0, true);
        assert!((left - 0.25).abs() < 1e-6);
        assert!((width - 0.25).abs() < 1e-6);
    }

    #[test]
    fn format_signs_and_units() {
        assert_eq!(format(0.35, 2, " EV", true), "+0.35 EV");
        assert_eq!(format(-46.0, 0, "", true), "-46");
        assert_eq!(format(5450.0, 0, " K", false), "5450 K");
    }
}
```

`ferrolite-app/src/widgets/mod.rs`:
```rust
pub mod slider;
pub use slider::EguiSlider;
```

- [ ] **Step 2: Add a placeholder widget type so `mod.rs` compiles**

Append to `ferrolite-app/src/widgets/slider.rs`:
```rust
/// The widget handle (egui rendering added in Task 4).
pub struct EguiSlider<'a> {
    pub label: &'a str,
    pub value: &'a mut f32,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub step: f32,
    pub decimals: usize,
    pub unit: &'a str,
    pub bipolar: bool,
    pub signed: bool,
}
```

- [ ] **Step 3: Wire widgets into the crate**

In `ferrolite-app/src/main.rs` add:
```rust
mod widgets;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-app slider::`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/widgets ferrolite-app/src/main.rs
git commit -m "feat(app): EguiSlider pure value math with tests"
```

---

### Task 4: EguiSlider egui rendering

**Files:**
- Modify: `ferrolite-app/src/widgets/slider.rs` (add `impl egui::Widget`)

**Interfaces:**
- Consumes: `slider::math::*`; `egui` (`Ui`, `Response`, `Sense`, `Color32`, `Stroke`, `pos2`, `vec2`).
- Produces: `impl<'a> egui::Widget for EguiSlider<'a>` — drag sets value (snapped/clamped), double-click resets to `default`, returns a `Response` whose `.changed()` is true when the value moved. Colors per design system §5.

- [ ] **Step 1: Implement the Widget**

Append to `ferrolite-app/src/widgets/slider.rs`:
```rust
use egui::{pos2, vec2, Color32, Response, Sense, Stroke, Ui, Widget};

// Design-system §5 slider tokens.
const TRACK: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3a);
const FILL_IDLE: Color32 = Color32::from_rgb(0x58, 0x58, 0x58);
const HANDLE_IDLE: Color32 = Color32::from_rgb(0x9a, 0x9a, 0x9a);
const HANDLE_BORDER: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
const ACCENT: Color32 = Color32::from_rgb(0x6d, 0x97, 0xb5);
const ACCENT_BRIGHT: Color32 = Color32::from_rgb(0xa9, 0xc7, 0xdd);
const LABEL: Color32 = Color32::from_rgb(0x8c, 0x8c, 0x8c);
const VALUE_IDLE: Color32 = Color32::from_rgb(0xbd, 0xbd, 0xbd);

const LABEL_W: f32 = 74.0;
const VALUE_W: f32 = 48.0;
const ROW_H: f32 = 22.0;

impl<'a> Widget for EguiSlider<'a> {
    fn ui(self, ui: &mut Ui) -> Response {
        let full = ui.available_width();
        let (rect, mut response) =
            ui.allocate_exact_size(vec2(full, ROW_H), Sense::click_and_drag());

        let track_left = rect.left() + LABEL_W + 8.0;
        let track_right = rect.right() - VALUE_W - 8.0;
        let track_w = (track_right - track_left).max(1.0);
        let mid_y = rect.center().y;

        let mut value = *self.value;
        if response.double_clicked() {
            value = self.default;
            response.mark_changed();
        }
        if let Some(p) = response.interact_pointer_pos() {
            if response.dragged() || response.clicked() {
                let frac = ((p.x - track_left) / track_w).clamp(0.0, 1.0);
                let new = math::value_at(frac, self.min, self.max, self.step);
                if (new - value).abs() > f32::EPSILON {
                    value = new;
                    response.mark_changed();
                }
            }
        }
        *self.value = value;

        let active = response.dragged();
        let frac = math::fraction(value, self.min, self.max);
        let (fill_left, fill_w) = math::fill(frac, self.min, self.max, self.bipolar);

        let painter = ui.painter();
        // label
        painter.text(
            pos2(rect.left() + 4.0, mid_y),
            egui::Align2::LEFT_CENTER,
            self.label,
            egui::FontId::proportional(11.0),
            LABEL,
        );
        // base track line
        painter.line_segment(
            [pos2(track_left, mid_y), pos2(track_right, mid_y)],
            Stroke::new(2.0, TRACK),
        );
        // fill
        let fill_color = if active { ACCENT } else { FILL_IDLE };
        painter.line_segment(
            [
                pos2(track_left + fill_left * track_w, mid_y),
                pos2(track_left + (fill_left + fill_w) * track_w, mid_y),
            ],
            Stroke::new(2.0, fill_color),
        );
        // handle
        let hx = track_left + frac * track_w;
        let handle_color = if active { ACCENT_BRIGHT } else { HANDLE_IDLE };
        painter.circle(pos2(hx, mid_y), 5.5, handle_color, Stroke::new(1.0, HANDLE_BORDER));
        // value text
        let value_color = if active { ACCENT } else { VALUE_IDLE };
        painter.text(
            pos2(rect.right() - 4.0, mid_y),
            egui::Align2::RIGHT_CENTER,
            math::format(value, self.decimals, self.unit, self.signed),
            egui::FontId::monospace(11.0),
            value_color,
        );

        response
    }
}
```

- [ ] **Step 2: Verify it compiles and clippy is clean**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/widgets/slider.rs
git commit -m "feat(app): render EguiSlider in egui with drag + reset"
```

---

### Task 5: Theme — tokens, Visuals override, bundled fonts

**Files:**
- Create: `ferrolite-app/src/theme.rs`
- Add font assets: `ferrolite-app/assets/fonts/*.ttf`
- Modify: `ferrolite-app/src/main.rs` (add `mod theme;`)

**Interfaces:**
- Consumes: `egui::Context`.
- Produces: `theme::install(ctx: &egui::Context)` — installs fonts + dark Visuals; `theme::BG_APP`, `theme::ACCENT`, etc. as `pub const Color32` tokens (design system §2).

- [ ] **Step 1: Download the fonts**

Download IBM Plex (SIL OFL 1.1 — compatible with a GPL binary) into `ferrolite-app/assets/fonts/`:
```bash
mkdir -p ferrolite-app/assets/fonts
cd ferrolite-app/assets/fonts
base=https://raw.githubusercontent.com/IBM/plex/master
curl -L -o IBMPlexSans-Regular.ttf   $base/IBM-Plex-Sans/fonts/complete/ttf/IBMPlexSans-Regular.ttf
curl -L -o IBMPlexSans-Medium.ttf    $base/IBM-Plex-Sans/fonts/complete/ttf/IBMPlexSans-Medium.ttf
curl -L -o IBMPlexSans-SemiBold.ttf  $base/IBM-Plex-Sans/fonts/complete/ttf/IBMPlexSans-SemiBold.ttf
curl -L -o IBMPlexMono-Regular.ttf   $base/IBM-Plex-Mono/fonts/complete/ttf/IBMPlexMono-Regular.ttf
curl -L -o IBMPlexMono-Medium.ttf    $base/IBM-Plex-Mono/fonts/complete/ttf/IBMPlexMono-Medium.ttf
cd -
```
Verify all five files exist and are > 50 KB. Add a `LICENSE-fonts.txt` next to them noting OFL 1.1.

- [ ] **Step 2: Write the theme module with a token test**

`ferrolite-app/src/theme.rs`:
```rust
//! Dark theme + bundled fonts. Tokens from docs/design/ferrolite-design-system.md §2/§3.

use egui::{Color32, Context, FontData, FontDefinitions, FontFamily, Visuals};

pub const BG_APP: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
pub const BG_PANEL: Color32 = Color32::from_rgb(0x17, 0x17, 0x17);
pub const BG_TITLEBAR: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
pub const BG_TOOLBAR: Color32 = Color32::from_rgb(0x1d, 0x1d, 0x1d);
pub const BG_BASE: Color32 = Color32::from_rgb(0x14, 0x14, 0x14);
pub const BG_CANVAS: Color32 = Color32::from_rgb(0x0e, 0x0e, 0x0e);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xdc, 0xdc, 0xdc);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x8a);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x6a, 0x6a, 0x6a);
pub const ACCENT: Color32 = Color32::from_rgb(0x6d, 0x97, 0xb5);
pub const ACCENT_BG_SEL: Color32 = Color32::from_rgb(0x21, 0x2a, 0x30);

pub fn install(ctx: &Context) {
    install_fonts(ctx);
    let mut v = Visuals::dark();
    v.panel_fill = BG_APP;
    v.window_fill = BG_TOOLBAR;
    v.extreme_bg_color = BG_BASE;
    v.override_text_color = Some(TEXT_PRIMARY);
    v.selection.bg_fill = ACCENT_BG_SEL;
    v.selection.stroke.color = ACCENT;
    ctx.set_visuals(v);
}

fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "plex-sans".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")).into(),
    );
    fonts.font_data.insert(
        "plex-mono".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")).into(),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "plex-sans".into());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "plex-mono".into());
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_token_matches_design_system() {
        assert_eq!(ACCENT, Color32::from_rgb(109, 151, 181)); // #6d97b5
    }

    #[test]
    fn app_background_token_is_dark() {
        assert_eq!(BG_APP, Color32::from_rgb(26, 26, 26)); // #1a1a1a
    }
}
```

Note: `FontData::from_static(...).into()` targets egui 0.29's `Arc<FontData>` map. If `cargo build` reports a type mismatch on your pinned egui version, drop the `.into()`.

- [ ] **Step 3: Wire theme into the crate**

In `ferrolite-app/src/main.rs` add:
```rust
mod theme;
```

- [ ] **Step 4: Run the token tests**

Run: `cargo test -p ferrolite-app theme::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/theme.rs ferrolite-app/src/main.rs ferrolite-app/assets
git commit -m "feat(app): dark theme tokens, Visuals override, bundled IBM Plex fonts"
```

---

### Task 6: wgpu canvas callback (Gate 0 proof)

**Files:**
- Create: `ferrolite-app/src/canvas/mod.rs`, `ferrolite-app/src/canvas/callback.rs`, `ferrolite-app/src/canvas/shader.wgsl`
- Modify: `ferrolite-app/src/main.rs` (add `mod canvas;`)

**Interfaces:**
- Consumes: `egui_wgpu::{CallbackTrait, RenderState, ScreenDescriptor}`, `wgpu`.
- Produces:
  - `canvas::CanvasResources` — holds the `wgpu::RenderPipeline`; built once from a `&RenderState` via `CanvasResources::new(rs)`, inserted into `rs.renderer.write().callback_resources`.
  - `canvas::paint(ui: &mut egui::Ui, rect: egui::Rect)` — adds the paint callback that draws the gradient into `rect`.

- [ ] **Step 1: Write the WGSL shader**

`ferrolite-app/src/canvas/shader.wgsl`:
```wgsl
// Fullscreen gradient — proves egui hosts a live wgpu render pass (Gate 0).
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // Fullscreen triangle.
    let x = f32(i32(i) / 2) * 4.0 - 1.0;
    let y = f32(i32(i) & 1) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    // Steel-blue accent gradient (#6d97b5 family) so a glance confirms it rendered.
    let uv = frag.xy / 720.0;
    return vec4<f32>(0.10 + 0.30 * uv.x, 0.20 + 0.35 * uv.y, 0.45, 1.0);
}
```

- [ ] **Step 2: Implement the callback**

`ferrolite-app/src/canvas/callback.rs`:
```rust
use egui_wgpu::{CallbackTrait, RenderState};
use wgpu::util::DeviceExt as _;

pub struct CanvasResources {
    pipeline: wgpu::RenderPipeline,
}

impl CanvasResources {
    pub fn new(rs: &RenderState) -> Self {
        let device = &rs.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("canvas-layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("canvas-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(rs.target_format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        // Touch DeviceExt so the import is used once vertex buffers arrive (Plan 4); harmless now.
        let _ = std::marker::PhantomData::<&dyn Fn() -> wgpu::Buffer>;
        let _ = device.create_buffer_init;
        Self { pipeline }
    }
}

pub struct CanvasCallback;

impl CallbackTrait for CanvasCallback {
    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(res) = resources.get::<CanvasResources>() {
            pass.set_pipeline(&res.pipeline);
            pass.draw(0..3, 0..1);
        }
    }
}
```

Note: the two `let _ =` lines exist only to keep the `DeviceExt` import warning-free until Plan 4 adds vertex buffers — delete them and the `use ... DeviceExt` line if clippy prefers, this is a placeholder-free convenience, not a behavior.

`ferrolite-app/src/canvas/mod.rs`:
```rust
mod callback;
pub use callback::CanvasResources;

use callback::CanvasCallback;

/// Add the wgpu paint callback that fills `rect` with the gradient.
pub fn paint(ui: &mut egui::Ui, rect: egui::Rect) {
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        CanvasCallback,
    ));
}
```

- [ ] **Step 3: Wire canvas into the crate**

In `ferrolite-app/src/main.rs` add:
```rust
mod canvas;
```

- [ ] **Step 4: Verify it compiles and clippy is clean**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: no warnings. (If the `DeviceExt` placeholder lines trip clippy, remove them and the import as noted.)

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/canvas ferrolite-app/src/main.rs
git commit -m "feat(app): wgpu canvas render pipeline + paint callback"
```

---

### Task 7: Assemble the shell, boot the app, clear Gate 0

**Files:**
- Create: `ferrolite-app/src/app.rs`
- Rewrite: `ferrolite-app/src/main.rs` (real eframe entrypoint)

**Interfaces:**
- Consumes: `module::Module`, `theme`, `canvas`, `widgets::EguiSlider`, `eframe`.
- Produces: `app::FerroliteApp` implementing `eframe::App`; `app::FerroliteApp::new(cc: &eframe::CreationContext) -> Self`.

- [ ] **Step 1: Write the app shell**

`ferrolite-app/src/app.rs`:
```rust
use crate::canvas::{self, CanvasResources};
use crate::module::Module;
use crate::theme;
use crate::widgets::EguiSlider;

pub struct FerroliteApp {
    module: Module,
    thumb_size: f32,
}

impl FerroliteApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            let res = CanvasResources::new(rs);
            rs.renderer.write().callback_resources.insert(res);
        }
        Self {
            module: Module::default(),
            thumb_size: 46.0,
        }
    }
}

impl eframe::App for FerroliteApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("titlebar")
            .exact_height(30.0)
            .frame(egui::Frame::none().fill(theme::BG_TITLEBAR))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.colored_label(theme::ACCENT, "■");
                    ui.label("FERROLITE");
                    ui.add_space(12.0);
                    for m in ["File", "Edit", "Photo", "View", "Help"] {
                        ui.label(m);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.monospace("v0.0.1");
                        ui.add_space(12.0);
                        if ui
                            .selectable_label(!self.module.is_library(), "Develop")
                            .clicked()
                        {
                            self.module = Module::Develop;
                        }
                        if ui
                            .selectable_label(self.module.is_library(), "Library")
                            .clicked()
                        {
                            self.module = Module::Library;
                        }
                    });
                });
            });

        egui::SidePanel::left("left")
            .exact_width(236.0)
            .frame(egui::Frame::none().fill(theme::BG_PANEL))
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.colored_label(theme::TEXT_FAINT, "CATALOG");
                ui.label("All Photographs");
                ui.add_space(12.0);
                ui.colored_label(theme::TEXT_FAINT, "THUMBNAIL SIZE");
                ui.add(EguiSlider {
                    label: "Size",
                    value: &mut self.thumb_size,
                    min: 0.0,
                    max: 100.0,
                    default: 46.0,
                    step: 1.0,
                    decimals: 0,
                    unit: "",
                    bipolar: false,
                    signed: false,
                });
            });

        egui::TopBottomPanel::bottom("status")
            .exact_height(24.0)
            .frame(egui::Frame::none().fill(theme::BG_TITLEBAR))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.monospace("NEF · 8256×5504 · ISO 100 · 14mm · f/8 · 1/250s");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.monospace("GPU: idle");
                        ui.monospace("·");
                        ui.monospace("0 indexed");
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG_CANVAS))
            .show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                canvas::paint(ui, rect);
            });
    }
}
```

- [ ] **Step 2: Real entrypoint**

Rewrite `ferrolite-app/src/main.rs` (keep the `mod` lines from earlier tasks):
```rust
mod app;
mod canvas;
mod module;
mod theme;
mod widgets;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default().with_inner_size([1440.0, 810.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Ferrolite",
        native_options,
        Box::new(|cc| Ok(Box::new(app::FerroliteApp::new(cc)))),
    )
}
```

- [ ] **Step 3: Build, lint, test**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all pass (module + slider + theme tests = 10 total).

- [ ] **Step 4: Manual Gate 0 verification (visual — record the result)**

Run: `cargo run -p ferrolite-app`
Expected, confirm each:
- Window opens at ~1440×810, dark theme, IBM Plex fonts visible.
- Title bar shows FERROLITE + menus + Library/Develop tabs (Library selected) + version.
- Clicking Develop/Library toggles the highlighted tab.
- Left panel shows the `Size` slider; dragging it moves the handle and updates the number; double-click resets to 46.
- **Central panel shows the steel-blue wgpu gradient** (this is the Gate 0 proof: egui is hosting a live wgpu render pass).

Repeat on macOS and Windows (or rely on CI building + a teammate's visual check). Note any per-OS issue in the commit body.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/app.rs ferrolite-app/src/main.rs
git commit -m "feat(app): assemble themed shell with wgpu canvas — clears Gate 0"
```

---

## Self-Review

**Spec coverage (against Spec 1 design doc):**
- §3 architecture / workspace + crate tiers → Task 1 (workspace; `ferrolite-app` only, other crates deferred to Plans 2–4 per the plan sequence). ✓
- §8 UI shell (title bar, Library/Develop tabs, left panel, status bar, canvas) → Tasks 6–7. ✓
- §8 theme + bundled fonts → Task 5. ✓
- §8 `EguiSlider` (built in Spec 1) → Tasks 3–4. ✓
- §12 build order step 1 (workspace + CI on 3 OSes) → Task 1. ✓
- §12 build order step 2 / **Gate 0** (egui renders a wgpu texture on all 3 OSes; theme + slider) → Tasks 5–7. ✓ (= VT rung 1 substrate.)
- §10 testing (pure-logic unit tests; visual gate for rendering) → slider math, theme tokens, module logic tested; canvas/shell verified via manual Gate-0 gate (rendering is inherently visual — golden-image GPU diffs arrive with the VT work in Plan 4). ✓
- **Deferred to later plans (correctly out of this plan):** decode, catalog, jobs, VT rungs 2–4, two-tier load, benchmark harness. Mapped in "Plan sequence". ✓

**Placeholder scan:** No "TBD/TODO/handle later". The two `let _ =` lines in Task 6 are explicitly explained convenience-to-avoid-unused-import, with removal instructions — not a logic placeholder. Version numbers carry an explicit "confirm with `cargo add`/`cargo tree`" instruction because crate APIs drift; the pinned values are a known-good starting point, not a guess to be left unverified.

**Type consistency:** `Module::is_library` used identically in Tasks 2 and 7. `slider::math::{fraction,snap,value_at,fill,format}` defined in Task 3, consumed in Task 4. `CanvasResources::new(&RenderState)` defined in Task 6, called in Task 7. `theme::install(&Context)` + tokens defined in Task 5, used in Tasks 4/7. `FerroliteApp::new(&CreationContext)` defined in Task 7, called in `main`. Consistent. ✓

**Known execution risk:** exact egui/eframe/wgpu API surface (font map `.into()`, `entry_point: Some(...)`, `compilation_options`, `cache` fields) tracks the pinned 0.29/wgpu-22 line; on a different pinned version the implementer adjusts these per the inline notes. The rust-build-resolver agent is available if a version bump shifts signatures.
