# ferrolite — Design System (derived from the Claude Design mockup)

> **Source:** claude.ai/design project "Ferrolite" (`1513fd43-811e-4a92-9dd7-51c6928125fa`),
> files `Ferrolite.dc.html` + `EguiSlider.dc.html`, imported 2026-06-28 (archived alongside
> this file in `docs/design/`).
> **Purpose:** Translate the HTML mockup into an implementable spec for the **egui** UI.
> The app is pure-Rust egui with a wgpu canvas (settled — no WebView/HTML UI), so the
> mockup is a **visual target**, not code to ship. This file is the canonical theme +
> widget + layout reference every UI task across all specs builds against.

---

## 1. Visual direction

A professional, **dark, Lightroom/Capture-One-class** desktop tool. Restrained, dense,
information-first. Muted steel-blue accent, neutral greys, monospaced numerics. No
gradients except subtle thumbnail overlays; no rounded-everything; tight 3px radii.

Two top-level modules switched by a segmented control in the title bar:
- **Library** — catalog browse, folders/collections, thumbnail grid. → **Spec 1**.
- **Develop** — single-image edit: navigator, filmstrip, canvas, adjustment panel. → **Spec 2/3**.

---

## 2. Color tokens

Define these as a `Theme` struct in `ferrolite-app` (egui `Visuals` override). Hex as-observed.

### Surfaces (darkest → lightest)
| Token | Hex | Use |
|---|---|---|
| `bg.canvas` | `#0d0d0d` / `#0e0e0e` | image canvas, thumbnail wells, histogram/curve plot bg |
| `bg.base` | `#141414` | grid area, inset input fields |
| `bg.titlebar` | `#161616` | title/menu bar, status bar |
| `bg.panel` | `#171717` | left panels (catalog/folders, navigator/filmstrip) |
| `bg.app` | `#1a1a1a` | app background, right adjustment panel |
| `bg.toolbar` | `#1d1d1d` | top toolbars, popovers |
| `bg.inputDim` | `#121212` | search field |

### Lines / borders
| Token | Hex | Use |
|---|---|---|
| `border.strong` | `#2a2a2a` | panel separators, main dividers |
| `border.control` | `#303030` | input/control outlines |
| `border.subtle` | `#242424` / `#232323` / `#262626` | section rows, plot frames |
| `border.popover` | `#353535` | popover edge |

### Text
| Token | Hex | Use |
|---|---|---|
| `text.primary` | `#dcdcdc` / `#d0d0d0` | titles, active labels |
| `text.secondary` | `#c0c0c0` / `#b5b5b5` / `#b0b0b0` | body, control text |
| `text.dim` | `#8a8a8a` / `#7a7a7a` | secondary labels, slider labels (`#8c8c8c`) |
| `text.faint` | `#6a6a6a` / `#5e5e5e` | section headers, placeholders, counts |
| `text.onAccent` | `#eaf1f6` / `#e0e8ee` | text on accent selection |

### Accent (steel blue)
| Token | Hex | Use |
|---|---|---|
| `accent` | `#6d97b5` | primary accent: active tab, selection ring, logo, active slider fill, focus |
| `accent.bright` | `#a9c7dd` | active slider handle, hover highlight |
| `accent.text` | `#cfe0ec` | text on accent-tinted buttons |
| `accent.bgSel` | `#212a30` / `#232b30` | selected row / pressed button background |
| `accent.border` | `#34464f` / `#3f5f73` | accent control border, active-tab background |

### Semantic — photo color labels & collection dots
red `#c75450` · amber `#c9a23a` · green `#5aa06a` · blue `#6d97b5` (=accent) · purple `#8a6ab0` · teal `#4aa6a0` · print-amber `#c98a3a` · reject-red `#9a5a5a`

### Histogram channels (Spec 3)
R stroke `#cf6a6a` / fill `#b54949`@50% · G `#6cc077` / `#4f9e56`@50% · B `#6d97d5` / `#4a73b5`@50%

### Slider (see §5)
track `#3a3a3a` · fill idle `#585858` / active `accent` · handle idle `#9a9a9a` / active `accent.bright` · handle border `#161616` · value idle `#bdbdbd` / active `accent`

---

## 3. Typography

Two families (matches the `--max-two-font-families` rule):
- **IBM Plex Sans** (400/500/600) — all UI text.
- **IBM Plex Mono** (400/500) — every numeric readout: counts, EXIF, version, slider values, breadcrumb totals, zoom.

Bundle both as static fonts in the binary (egui `FontDefinitions`) — do **not** fetch from Google Fonts at runtime (offline desktop app).

| Role | Size | Weight | Notes |
|---|---|---|---|
| Section header | 10px | 600 | UPPERCASE, letter-spacing ~1px, `text.faint` |
| Logo wordmark | 11px | 600 | letter-spacing 1.5px |
| Control / body | 11–11.5px | 400–500 | |
| Menu items | 11.5px | 400 | |
| Mono readouts | 9–11px | 400–500 | IBM Plex Mono |
| Base | 12px | 400 | |

---

## 4. Layout & metrics

| Region | Size |
|---|---|
| Title/menu bar height | 30px |
| Toolbar height (both modules) | 40px |
| Grid breadcrumb bar | 28px |
| Status bar | 24px |
| Library left panel width | 236px |
| Develop left (navigator/filmstrip) | 160px |
| Develop right (adjustment panel) | 296px |
| Control height | 22–24px |
| Slider row height | 22px |
| Radius — controls | 3px |
| Radius — groups/popovers | 4px |
| Radius — logo mark | 2px |
| Thumbnail aspect | 3:2 |
| Grid gap | 10px |
| Grid column | `auto-fill, minmax(118 + sizeSlider*1.7 px, 1fr)` |

**App shell:** `title bar` → `module body`. Each module body = `toolbar` → `content row`.
- Library content row: `left panel (236)` | `grid column (breadcrumb → scroll grid → status bar)`.
- Develop content row: `left (160: navigator + filmstrip)` | `center canvas (flex)` | `right (296: adjustment sections)`.

Custom scrollbars: 10px, track `#141414`, thumb `#383838` (hover `#484848`), 5px radius.

---

## 5. EguiSlider — widget spec (the core reusable control)

Horizontal row, height 22px: **`[label 74px] [track flex] [value 48px right]`**.

- **Label** (left, 74px, `text.dim`, ellipsis).
- **Track** (flex, 18px hit area, `ew-resize`): full-width 2px base line `#3a3a3a`; a fill bar
  and an 11px circular handle (1px `#161616` border).
- **Value** (right, 48px, mono, right-aligned).

**Fill modes:**
- *Unipolar:* fill from left edge to handle.
- *Bipolar* (`bipolar=true`): fill spans from the **zero position** to the handle (so negative
  values fill leftward from center). Zero fraction = `(0-min)/(max-min)`.

**Interaction:**
- Drag anywhere on track → set value from x; **snap to `step`**; clamp `[min,max]`.
- **Double-click → reset to `default`.**
- While dragging (`active`): fill → `accent`, handle → `accent.bright`, value text → `accent`.
- Idle: fill `#585858`, handle `#9a9a9a`, value `#bdbdbd`.

**Formatting:** `value.toFixed(decimals) + unit`; if `signed` and value > 0, prefix `+`.

**Parameters:** `label, value, min, max, default, step, decimals, unit, bipolar, signed`.

**egui implementation note:** a custom `Widget` — `ui.allocate_response(size, Sense::click_and_drag())`,
paint via `ui.painter()`, detect double-click via `response.double_clicked()`. Returns the new
value + a `changed` flag so the caller can mark the edit-DAG node dirty (Spec 2). For Spec 1 it
drives non-edit values (thumbnail size, metadata-filter ranges).

---

## 6. Component inventory → egui mapping → owning spec

| Mockup component | egui realization | Spec |
|---|---|---|
| Title bar + menus | custom top panel (`TopBottomPanel`) | 1 |
| Library/Develop segmented tabs | `SelectableLabel` pair, accent bg | 1 |
| Version readout | mono label | 1 |
| Search field | `TextEdit` + leading glyph | 1 |
| Sort combo / WB preset / Aspect combo | `ComboBox` (restyled) | 1 / 2 |
| Star-rating filter row | custom star widget | 1 |
| Color-label filter dots | custom dot row | 1 |
| Metadata-filter popover | `Area` + `Frame` popup; contains `EguiSlider`s | 1 |
| Thumbnail-size slider | bare `EguiSlider` track variant | 1 |
| Left catalog/folders/collections tree | custom tree rows (indented, counts) | 1 |
| Thumbnail grid | virtualized grid in `ScrollArea` (manual layout, lazy thumbs) | 1 |
| Breadcrumb bar | label row | 1 |
| Status bar (EXIF · indexed · GPU) | mono panel; **"GPU: idle/busy" + "N indexed"** bind to job system + catalog | 1 |
| Image canvas | wgpu paint callback region (preview→VT) | 1 |
| Develop toolbar (Crop/Heal/Mask/Grad, undo/redo, Before/After, zoom) | button row | 2 |
| Navigator thumbnail + viewport rect | small canvas + overlay rect | 2 |
| Filmstrip | vertical `ScrollArea` of thumbs | 2 |
| Collapsible adjustment sections (▸/▾) | restyled `CollapsingHeader` | 2 |
| Adjustment sliders (Exposure…Crop) | `EguiSlider` bound to edit-DAG nodes | 2 |
| Histogram | custom painted widget (RGB paths) | 3 |
| Tone-curve editor | custom painted interactive widget | 2 |
| HSL swatch selector | swatch row + `EguiSlider`s | 2 |
| Before/After split | dual canvas region | 2/3 |

---

## 7. Binding to the architecture

- The **Library module is the Spec 1 UI target.** Its status bar is a live readout of Spec 1
  subsystems: `"N indexed"` ← catalog row count; `"GPU: idle/busy"` ← job/VT activity; the
  EXIF line ← `ferrolite-decode` metadata. The grid is the virtualized thumbnail surface (G1);
  selecting/opening a photo triggers the two-tier load (G2).
- The **Develop module is the Spec 2/3 UI target.** Every adjustment `EguiSlider` is a control
  surface over a node in the `ferrolite-pipeline` retained edit DAG; the histogram + before/after
  are Spec 3 (color) surfaces.
- **`EguiSlider` is built in Spec 1** (needed by the thumbnail-size control and metadata-filter
  ranges) and reused heavily in Spec 2 — so it lands early as a shared `ferrolite-app` widget.
- The theme (`§2`/`§3`) is established in Spec 1's Phase-0 shell and reused unchanged thereafter.

---

## 8. Fidelity notes / deviations allowed

- Picsum/`picsum.photos` images and placeholder data in the mockup are illustrative only.
- Exact hex values may be consolidated into the smallest consistent token set during
  implementation (e.g. collapse near-identical greys) as long as the visual result matches.
- Glyph icons in the mockup (`⌕ ▦ ◷ ⚑ ⛭ ◐`) are placeholders; the implementation may substitute
  a proper icon set, keeping size/placement.
