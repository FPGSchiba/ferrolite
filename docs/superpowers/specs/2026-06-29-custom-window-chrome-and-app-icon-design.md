# Ferrolite — Custom Window Chrome & App Icon (Design)

> **Status:** design, approved in brainstorm 2026-06-29. Follows Spec 1 Plan 1 (foundation & Gate 0), which shipped the themed egui/wgpu shell.
> **Design system:** `docs/design/ferrolite-design-system.md` (§1 visual direction, §2 tokens, §4 metrics).

## Problem

The app currently runs in an OS-decorated (winit) window **and** draws its own `TopBottomPanel::top("titlebar")` underneath it — so the user sees **two stacked title bars**. The design system specifies a single, custom, Lightroom/Capture-One-style title+menu bar (§1, §4: "Title/menu bar height 30px"). Additionally, the window shows eframe's default icon in the title bar / taskbar instead of a Ferrolite mark.

## Goal

Replace the OS title bar with our own: turn off native decorations and make the existing 30px bar the real window title bar (drag, window controls, resize), with a designed Ferrolite icon used both as the title-bar mark and the OS window/taskbar icon.

## Non-goals (unchanged from Plan 1 scope)

- Real dropdown menus (File/Edit/… stay as labels for now — a later spec wires them).
- Status-bar metadata wiring (still the Plan 2 catalog work).
- OS-specific window snapping / Aero-snap affordances beyond what winit gives for free.
- macOS traffic-light styling — decided: a single consistent right-side control layout on all three OSes.

## Approach

Standard eframe **custom window frame**: `ViewportBuilder::with_decorations(false)`, and our title bar renders the window controls + handles drag/maximize/resize via `egui::ViewportCommand`. Chosen over keeping OS decorations (which can't deliver the single-bar design) and over a third-party frame crate (unnecessary dependency).

## Window setup (`main.rs`)

`ViewportBuilder::default()`:
- `.with_inner_size([1440.0, 810.0])` (unchanged)
- `.with_decorations(false)` — borderless; our bar becomes the title bar
- `.with_resizable(true)`
- `.with_min_inner_size([960.0, 600.0])` — prevent collapsing the dense layout
- `.with_icon(<IconData>)` — the Ferrolite app icon (see Icon section)

## Title bar (`chrome` module)

A single `TopBottomPanel::top("titlebar")`, `exact_height(30.0)`, fill `BG_TITLEBAR` (#161616). Layout left → right with **tabs centered**:

- **Left:** faceted-F icon mark (~18px, painted — see Icon) + `FERROLITE` wordmark, then `File Edit Photo View Help` as labels (`TEXT_DIM`/`TEXT_PRIMARY`).
- **Center:** `Library | Develop` segmented switcher, horizontally centered in the bar. Drives `Module` using the existing one-bool selectable-label logic (exactly one active; clicking sets `Module::Library`/`Develop`).
- **Right:** `v0.0.1` (IBM Plex Mono, `TEXT_DIM`), then the three window-control buttons.

**Drag region:** the bar's non-interactive area is sensed with `Sense::click_and_drag()`; on `drag_started()` send `ViewportCommand::StartDrag`; on `double_clicked()` toggle maximize (see controls). Interactive widgets (logo is non-interactive; menu labels, tabs, version, buttons) must not also trigger window drag — allocate the drag interaction over the residual space only.

### Window controls (`chrome/window_controls.rs`)

Three themed buttons at the right edge, ~30×30 hit area, drawn with the painter (glyphs: `–` minimize, `▢`/`❐` maximize-restore, `✕` close). Hover background `BG_TOOLBAR` (#1d1d1d); **close** hover background uses the design-system semantic red `#c75450`. The mapping from a button to its effect is **pure and unit-tested**:

```
enum WindowAction { Minimize, ToggleMaximize, Close }

fn command(action: WindowAction, is_maximized: bool) -> egui::ViewportCommand
// Minimize       -> ViewportCommand::Minimized(true)
// ToggleMaximize -> ViewportCommand::Maximized(!is_maximized)
// Close          -> ViewportCommand::Close
```

`is_maximized` is read from `ctx.input(|i| i.viewport().maximized)`. The egui painting/hit-testing is not unit-tested (visual check), but the `command` mapping and the maximize toggle are.

### Resize

Keep `with_resizable(true)`. egui 0.29 supports edge resizing of undecorated windows via `ViewportCommand::BeginResize(ResizeDirection)`. **Verify during implementation** that Windows gives edge-resize for the borderless window; **only if it does not**, add thin (~6px) invisible resize-grips along the edges/corners that issue `BeginResize`. This contingency is explicitly allowed by the plan and gated on the observed behavior.

### Window border

A borderless dark window blends into the desktop, so draw a 1px outer `BORDER_STRONG` (#2a2a2a) frame around the whole window. Implement via the outermost `CentralPanel`/frame stroke (or a full-window `Frame` with a stroke). Square corners (matches the Lightroom/mockup aesthetic; design-system radii apply to inner controls, not the window).

## App icon — Concept A "Faceted F monogram"

A two-tone steel "F" — a direct evolution of the current accent ■ mark; reads at 16px. **Master geometry** (SVG `viewBox 0 0 64 64`):

- **F body** polygon, fill `ACCENT` #6d97b5:
  `20,16  46,16  46,25  29,25  29,31  42,31  42,40  29,40  29,49  20,49`
- **Light facet** polygon over the top arm + upper stem, fill `ACCENT_BRIGHT` #a9c7dd at 0.6 opacity:
  `20,16  46,16  46,25  29,25  29,31  20,31`
- **OS-icon tile** (icon only, not the title-bar mark): rounded rect `x2 y2 w60 h60 rx13`, fill `#161a1f`, 1px stroke `#2a2a2a`, with the F centered on it.

### Two renderings from one master

1. **Title-bar mark** — painted procedurally in egui (`Painter` + two `Shape::convex_polygon` from the points above, scaled to ~18px, transparent background, no tile). No asset, no decode dependency; scales crisply.
2. **OS window/taskbar icon** — the F **on the rounded tile**, supplied to `with_icon` as `IconData` (RGBA). A master PNG (e.g. 256×256, plus 64/32 if useful) is generated **offline** from the SVG during implementation (resvg/rsvg/inkscape or equivalent) and committed under `ferrolite-app/assets/icon/`. At startup it is decoded to `IconData`.
   - Preferred loader: `eframe::icon_data::from_png_bytes(include_bytes!(...))` if available under our `eframe` features; otherwise add the `png` crate (decode-only) or `image` with just the `png` feature. Exact mechanism is a verify-during-impl detail — keep new dependencies minimal and permissive-licensed.

The procedural mark and the rasterized tile derive from the same coordinates, so they stay visually consistent.

## File structure (additions/changes)

```
ferrolite-app/
  assets/icon/
    ferrolite.svg            # master (Concept A) — source of truth
    ferrolite-256.png        # generated; window IconData (sizes TBD during impl)
  src/
    chrome/
      mod.rs                 # title_bar(ctx, ui, module, version) — assembles the bar
      icon.rs                # paint_mark(painter, rect) — procedural faceted F; F polygon consts
      window_controls.rs     # WindowAction + pure command() mapping (+ tests); button rendering
    app.rs                   # replace inline titlebar body with chrome::title_bar(...)
    main.rs                  # decorations(false), resizable, min size, with_icon(...)
```

`chrome` keeps the window-chrome concern out of `app.rs` (which stays the panel-layout orchestrator), honoring the "many small files / one responsibility" rule.

## Testing

- **Unit (pure logic):**
  - `window_controls::command(action, is_maximized)` returns the correct `ViewportCommand` for each action, and `ToggleMaximize` flips on both `is_maximized` states.
  - Module-switch invariant (extends existing `module` tests): exactly one of Library/Develop active.
  - If `icon.rs` exposes the F geometry as constants, a trivial test that the F polygon has the expected vertex count / closed shape (guards accidental edits).
- **Visual / manual (same gate style as Gate 0):** run `cargo run -p ferrolite-app` and confirm: only one title bar (no OS bar); drag moves the window; double-click maximizes/restores; minimize/maximize/close work; close-hover turns red; window edges resize; 1px border visible; the faceted-F mark shows in the bar; the taskbar/window icon is the Ferrolite F (not eframe's default).
- **Gates:** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` (zero warnings), `cargo test --all` stay green. CI (3-OS) unchanged.

## Risks / verify-during-implementation

- **Borderless edge-resize on Windows** — primary unknown; contingency (invisible resize-grips) specified above.
- **Drag vs. widget interaction** — ensure the drag sense covers only residual bar space so menu/tab/button clicks aren't swallowed.
- **Icon loader feature availability** — confirm `eframe::icon_data::from_png_bytes` under our features; else minimal decode dep.
- **macOS borderless behavior** — undecorated windows on macOS lose traffic lights (accepted) and may need `titlebar`-related viewport flags; verify the window is movable/closable there (CI builds; visual check by a mac user later).
