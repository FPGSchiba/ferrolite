# Handoff: Ferrolite Desktop UI (Library + Develop + Export)

## Overview
Desktop UI design for **Ferrolite**, an open-source RAW photo editor and library manager built in Rust with the **egui** immediate-mode GUI framework. This package covers three screens: **Library** (catalog/browser), **Develop** (the editor, with 4 tool modes: Adjust, Crop, Heal, Mask), and **Export**. The aesthetic is a "digital darkroom" — near-black neutral grays, flat egui-native widgets (no gradients/blur/heavy shadows), one restrained steel-blue accent color.

## About the Design Files
The files in this bundle (`Ferrolite.dc.html`, `EguiSlider.dc.html`) are **HTML design references** built to preview the intended look, layout, and interaction states — they are NOT production code and must not be copied into the Rust/egui codebase as-is. The task is to **recreate this design natively in egui** (Rust), mapping each HTML element to the equivalent egui primitive (see "egui mapping" below). Every visual in these files was deliberately constrained to only use effects egui can actually render — flat panels, 1px separators, circular slider handles, collapsing headers — specifically so the translation to egui is direct.

## Fidelity
**High-fidelity.** Exact colors, spacing, type sizes, and copy are given below and should be matched as closely as egui's styling API allows. Interaction affordances (hover/active states, tab switching, slider drag) are demonstrated in the HTML and should be recreated with egui's native equivalents, not reimplemented as literal HTML/CSS behavior.

## egui mapping — read this first
| HTML element in the mockup | egui equivalent |
|---|---|
| Custom slider (`EguiSlider.dc.html`) | `egui::Slider` with custom width, no text field spinner, drag-to-reset-on-double-click via `.on_hover_...` or custom widget |
| Tab row (Light/Color/Effects) | `egui::TopBottomPanel` row of `SelectableLabel`, or manual `Button`-per-tab with fill color = accent when active |
| Collapsible nested sections ("Tone Curve", "Region Tones", "Color Grading", "Optics") | `egui::CollapsingHeader` |
| Icon buttons (toolbar cluster, tool switcher) | `egui::Button` with icon font glyph, `fill` color set when selected |
| Color-label dots on thumbnails | small filled `egui::Painter::circle_filled` |
| Star ratings | row of clickable glyph `Label`/`Button` widgets |
| Combo boxes (Sort, Aspect, Working space, Format) | `egui::ComboBox` |
| Checkboxes (enable-distortion, Copy EXIF, etc.) | `egui::Checkbox` |
| Grid of thumbnails | `egui::Grid` or manual layout in a `ScrollArea` |
| Color-grading wheels | custom `egui::Widget` — a `Painter`-drawn conic gradient circle + draggable dot (no native primitive; build as custom paint + interact) |
| Histogram / curve SVGs | custom `Painter` paths (`Shape::line`/`Shape::path`) drawn each frame from the actual histogram/curve data |
| Floating panels over the canvas (tool cluster, histogram, EXIF chip) | `egui::Area` or `egui::Window` with `.title_bar(false)` positioned via `.fixed_pos()` |

## Screens / Views

### 1. Library
**Purpose:** Fast, dense photo browsing/culling — the catalog view.

**Layout:**
- Top menu/title bar: 30px tall, `#111111` bg, 1px `#262626` bottom border. Logo mark (14×14 accent-filled square, "F") + app name "FERROLITE" (11px, 600 weight, 1.5px letter-spacing, `#dcdcdc`) on the left; File/Edit/Photo/View/Help menu labels (11.5px, `#9a9a9a`); center-right: Library/Develop/Export nav (11.5px, active = `#eaf1f6` text + 2px accent bottom border, inactive = `#9a9a9a` + transparent border); far right: version string "v0.1.2" (`IBM Plex Mono`, 10.5px, `#6a6a6a`) + window controls (−, □, ✕). The active tab's 2px accent underline is painted fully inside the 30px titlebar and never covered by anything painted after it — it used to be maskable by the panel separator directly below the titlebar, which is now fixed.
- Toolbar: 38px tall, `#1a1a1a` bg, 1px `#262626` bottom border. Left to right: search field (210px, `#101010` bg, 1px `#2c2c2c` border, placeholder "Search filename or tag…"), Sort combo ("Capture Time"), ascending toggle, `>=` rating-operator button, 5 outline star-rating filter icons, green flag (pick) icon, red circle-slash (reject) icon, "Tags (0)" combo, "Metadata" filter dropdown button (opens a popup panel — see Metadata Filters below), a reset-all-filters button (counter-clockwise arrow icon) ending the filter cluster — greyed out once every filter/sort/search field is already at default; right-aligned: "Size" label + custom slider (110px track) + numeric readout + reset icon. The "Subfolders" toggle is not part of this cluster — it scopes folder selection, not filtering, and lives in the FOLDERS section header instead (see below).
- Left panel: 216px fixed width, `#171717` bg, 1px `#262626` right border.
  - CATALOG section (10px letter-spaced label, `#6a6a6a`): "All Photographs" (selected row, `#242424` bg, `#e0e0e0` text), "Recently Added".
  - "Open folder…" button (full width, `#1e1e1e` bg, 1px `#2c2c2c` border).
  - FOLDERS section: flat filesystem tree, e.g. "▸ 100MSDCF 3333" (count right-aligned, monospace, `#666`); a "Subfolders" toggle sits right-aligned in the section header, scoping whether the current folder selection includes its subfolders.
  - COLLECTIONS section (with a "+" add button): a tree nesting to **arbitrary depth**, built entirely via drag-and-drop — drag a collection row onto another collection row (any depth) to nest it underneath; drag onto the COLLECTIONS root header to un-nest it back to the top level. A drop that would create a cycle is rejected with no state change and flashes the target row red instead. "Add Sub-collection" is available at every depth, not just the top level. Counts right-aligned.
  - TAGS section (with a "+" add button): flat list of colored tags, e.g. "Cool" (red swatch), "Hammer" (green swatch) — 12×12px rounded-square color swatch + name + count.
- Grid (center, `#131313` bg): responsive `auto-fill` grid, cell min-width driven by the Size slider (`118 + sizePct*1.7`px). Each cell: 3:2 thumbnail image, 1px border (accent + 2px outline glow when selected), a star-rating glyph overlay bottom-left and a color-label dot bottom-right on the thumbnail itself, filename below (`IBM Plex Mono`, 10px) + capture date/time below that (9px, `#5c5c5c`).
- Status bar: 24px, `#141414` bg, 1px `#232323` top border — current filename + dimensions on the left (monospace, 10px, `#6e6e6e`), "Idle · N / 3333 indexed · GPU: idle" on the right.

**Metadata Filters popup** (anchored under the "Metadata" button, right-aligned to viewport): `#1d1d1d` bg, 1px `#353535` border, 300px wide. Header "METADATA FILTERS" + "Reset" link. Combo rows: Camera, Lens (active filters get accent-tinted border/text) — there is **no rating combo here**; rating filtering lives only in the main toolbar's `>=` rating control, so it isn't duplicated in the popup. "FILE TYPE": a multi-select chip row (RAW/JPEG/PNG/TIFF, any combination selectable, empty selection = all types) with its own per-section reset arrow. "EXPOSURE RANGE": ISO, Aperture, and Focal are each a dual-handle min–max **range slider** on a log track, not a single value — ISO in full-stops from 50 to 102400; Aperture in third-stops from f/0.7 to f/32 with an "f/" value prefix; Focal from 8mm to 1200mm with detents that widen with range (1mm below 50mm, 5mm below 200mm, 10mm below 600mm, 50mm above) since a constant step is hard to aim across that whole span. Double-clicking a range's readout opens manual text entry — "100-400" sets both handles, a single number sets an exact-match filter. A handle pair spanning the slider's full min–max reads as "inactive" for that field, and each range slider carries its own per-control reset. Footer: "Apply Filters" (accent-filled) / "Close" buttons. Lens/aperture/focal metadata is written at ingest time (catalog schema v7), with a background EXIF backfill job populating it for photos ingested before v7, so these filters work against existing libraries too.

### 2. Develop
**Purpose:** The RAW editor. Large canvas, tool-driven right panel.

**Layout:**
- Shared filter/toggle toolbar (38px, same style as Library's toolbar): "Before/After" toggle button on the left (icon ◐ + label, highlights when active), then the same Sort/rating/flag/tag filter cluster as Library (used to filter the filmstrip).
- Top filmstrip: 96px tall horizontal scroll strip of 120px-wide thumbnails, `#171717` bg. Selected thumbnail gets a 2px accent outline. The strip free-scrolls under manual drag/wheel input; it auto-centers on the selected thumbnail only in response to navigation (arrow keys, a click selecting a photo, or a programmatic open) — it does not fight a manual scroll back to center every frame.
- Center canvas: fills remaining width, `#0d0d0d` bg, image centered/contained (max 94% of available space).
  - **Floating tool cluster** (top-left over canvas): 4 icon buttons in a dark rounded rect (`#1c1c1cee` bg, 1px `#2f2f2f` border) — **Adjust** (⚙), **Crop** (⌗), **Heal** (✛), **Mask** (◐) — the active tool gets a `#2a3438` fill + `#bcd6e4` icon color. A vertical divider, then Undo (↺) / Redo (↻) icons (not tool-linked).
  - **Floating histogram** (top-right over canvas): 280px wide dark panel with an RGB overlaid histogram (red/green/blue filled paths with light strokes) drawn from the actual pixel data.
  - **Floating EXIF chip** (bottom-left over canvas): stacked monospace lines — focal length, aperture, shutter, ISO, zoom%. Sizes itself to its own content (an explicit width from the widest line, growing upward as rows are added/removed) rather than a fixed box.
  - **Info toggle button** (bottom-left over canvas, stacked ABOVE the EXIF chip so they never overlap): "ℹ Info" pill button, highlights when the Info panel (see below) is open. Docks to the canvas's bottom-left corner margin when the EXIF chip is hidden, and sits above the chip's real painted extent (not a fixed offset) whenever the chip is shown, so it tracks the chip's content-driven height instead of assuming a fixed one.
  - **Crop mode overlay** (only when Crop tool active): rule-of-thirds grid lines + 8 corner/edge handle squares drawn directly on the canvas. Shipped correctness: the locked aspect ratio holds exactly while dragging a handle at the image edge; the apply-crop edge-smear artifact is fixed (the sampling matrix now derives from the rounded output dimensions, and sampling clamps to the crop rect rather than the full frame); handle-drag releases commit properly.
  - **Mask mode overlay** (only when Mask tool active): full-canvas semi-transparent red tint (`#c02a2a` at ~42% opacity, multiply blend) to visualize the active mask region.
- **Left Info panel** (300px, mirrors the right panel's chrome — `#1a1a1a` bg, 1px `#262626` border, toggled via the bottom-left "Info" button, hidden by default): read-only rows — Camera, Lens, Focal, Aperture, Shutter, ISO, Captured (timestamp), Size, Zoom. Label (11px, `#7a7a7a`, fixed 66px column) + value (11.5px, `#d0d0d0`). **No reset button** — this panel is informational only.
- **Right panel** (300px, `#1a1a1a` bg, 1px `#262626` left border): header always shows camera name + edit status ("No edits"/"Saved") + a "Working space" combo (e.g. "Rec2020"). Below that, content swaps entirely based on the active tool. The panel's scrollbar hugs the panel's own edge (a single ~8px inset from the border), not floated inboard of the slider content:

  **Tool: Adjust** (default) and **Tool: Mask** *share the exact same "options library"* — same 3 tabs, same nested collapsible sections, same slider set — the only differences are (a) Mask's panel is prefixed with a mask-management header block, and (b) Mask's slider values start at 0 (untouched) since they're scoped per-mask rather than global.
  - Tab row: **Light | Color | Effects** — all three tabs sit on a single row, same visual level (no secondary row). Active tab = `#eaf1f6` text + 2px accent underline.
  - **Light tab**: Exposure (EV, bipolar), Contrast, Highlights, Shadows, Whites, Blacks (all -100..100 bipolar), Temp (2000–12000K), Tint (bipolar) — then a collapsible **"Tone Curve"** section (▸/▾ disclosure) containing: Master/R/G/B channel selector, a point-curve chart (diagonal line + 4 draggable node dots, hint text "Drag to adjust · double/right-click or Delete to remove a point"), Reset/Linear/Smooth button row. Directly below Tone Curve sits its own collapsible **"Region Tones"** section — Highlights/Lights/Darks/Shadows region sliders + Shadow split/Midtone split/Highlight split (0–1 range sliders) — with a one-line subheader ("Region-based curve tones — complements the Basic sliders, which weight by pixel brightness") distinguishing it from the brightness-weighted Basic sliders above. Footer: Reset + Reset all buttons.
  - **Color tab**: 8 hue swatches (24×24px rounded squares — red/orange/yellow/green/cyan/blue/purple/pink) representing the HSL color-range selector, then Hue/Sat/Lum sliders for the selected swatch, a divider, then global Vibrance/Saturation sliders — then a collapsible **"Color Grading"** section containing 4 color wheels (Shadows/Midtones/Highlights/Global — each an 88px circular conic-gradient with a white center dot + a Lum slider under it) plus Blending and Balance sliders. Footer: Reset all.
  - **Effects tab**: a "SHARPENING" section label, then Amount/Radius/Detail/**Masking** sliders — Masking suppresses sharpening in flat areas so it does not re-amplify the noise NR removed. Amount and Radius are per-mask as before; **Detail and Masking are global-only** (the per-mask apply shader does not read them yet) and ship greyed in Mask scope with an honest reason, the same precedent `dehaze_radius` set — this is a deliberate asymmetry within one section, not full Amount/Radius/Detail/Masking parity across scopes. A divider; a "NOISE REDUCTION" section label (with a small "AI" chip hinting at a future AI-denoise option) containing Luminance, Detail, Color, and **Color Detail** (chroma noise reduction) sliders, wired and live in Adjust scope — **noise reduction is global-only**: it runs upstream of where masks are composited, so all four sliders ship greyed in Mask scope with an honest reason ("Noise reduction runs before the tone and color stages so its strength stays independent of your other edits — global only"). A one-line subheader under the NOISE REDUCTION label reads "Judge noise reduction and sharpening at 1:1" — both operate on real pixel-scale detail, so a fit-view/coarse-LOD preview under-represents their effect. Then a collapsible **"Optics"** section containing lens name + "Choose lens…" button, Distortion (with an enable checkbox + "Needs a matched lens" note when no profile matched), and Vignette (manual). Footer: Reset all.

  **Tool: Crop** — tabs disappear entirely, replaced by a dedicated panel. **Shipped as designed**: a collapsible **"CROP & TRANSFORM"** section — Angle slider (own per-control reset); Aspect combo ("Original") kept in sync with a wrapping row of compact direct-format chips (Original/1:1/4:3/3:2/16:9/5:4/Custom, selected chip accent-tinted, "Custom" doubling as the free-ratio state indicator); a "Reset crop" button — and a collapsible **"GEOMETRY"** section — Keystone V / Keystone H sliders (manual homography warp at a constant strength K=0.35, each with its own per-control reset and a live preview while dragging); "Auto Perspective" / "Guided Upright" buttons ship **disabled**, with a "coming with automatic perspective analysis" hover — the design intent for these two is unchanged, they're just waiting on the CV work. Both sections persist their own disclosure state, same as every other Develop section (see State Management).

  *Known/intended behavior:* because keystone correction warps sampling via a homography, the widened sampling quad can extend past the crop rect on its far edge on some corners; those corners clamp to the crop edge rather than auto-cropping inward. The user re-crops manually if they want the frame tightened — there's no auto-crop compensation, by design, for now.

  **Tool: Heal** — "HEAL / CLONE / REMOVE" label, 3-way mode segmented control (Heal/Clone/Remove (AI)), Brush Size/Feather/Opacity sliders, helper text, spot count.

  **Tool: Mask** — header block ABOVE the shared options library (see above): "Create New Mask" button + an "AI" auto-mask chip + an eye/visibility toggle; below that an Inv checkbox + mask name ("Mask 1") + delete icon row; below that a "N components" counter + "New Brush Layer (B)" button. Then a label "Editing: Mask 1 — adjustments below apply only inside this mask" (accent-colored) directly above the shared Light/Color/Effects tabs.

- Bottom status bar (30px, `#141414` bg, 1px `#262626` top border): star rating + flag + reject + download icons, "Tags" label, divider, current filename+dimensions, right-aligned "Idle · N / 3333 indexed · GPU: idle".

### 3. Export
**Purpose:** Batch export queue + output settings.

**Layout:**
- Top bar (36px): "Export queue — N image(s)" + "Clear queue" link.
- Main area: empty-state centered text ("Queue is empty. Add images from Library or Develop.") when nothing queued; otherwise a grid of queued thumbnails (reuse Library's grid cell style).
- Right panel (300px): "EXPORT SETTINGS" label, then rows where the **control sits left, its label sits right** (reverse of the Develop panel's slider convention) — Format combo (JPEG), Output color space combo (Srgb), Bit depth 2-way segmented (8-bit/16-bit), Quality slider + "Quality" label, Effort 3-way segmented (Fast/Balanced/Best), Resize combo (None), **"Sharpen for" combo (None/Screen/Glossy paper/Matte paper — default None)**, **"Sharpen amount" combo (Low/Standard/High — default Standard, disabled when "Sharpen for" is None)**. Divider, then 3 checkboxes: Copy EXIF metadata (checked), Embed ICC profile (checked), Strip metadata (unchecked).
- Bottom bar (52px): "Destination folder…" button + "(no folder chosen)" text, "Filename" label + `{name}` template field + a "?" help icon circle, right-aligned "Start export" button (disabled style until a folder is chosen).
- Status bar: same pattern as Library/Develop.

## Interactions & Behavior
- **Tab switching** (Library/Develop/Export nav, and the Light/Color/Effects tabs): click to switch, instant — no transition/animation.
- **Tool switching** (Adjust/Crop/Heal/Mask floating cluster): click to switch; changes the entire right-panel content and adds mode-specific canvas overlays (crop grid, mask tint). Only one tool active at a time. Switching away from Crop never commits a crop by itself — only an explicit action (drag release, Reset) changes crop state.
- **Crop handle drag safety**: Escape cancels an in-progress handle drag, taking precedence over close-viewer only while a drag is in progress; otherwise close-viewer behaves as elsewhere.
- **Collapsible sections** ("Tone Curve", "Region Tones", "Color Grading", "Optics" inside each tab): click the row to toggle open/closed; chevron flips ▸ → ▾. Only relevant when their parent tab is active. Open/closed state persists reliably per section, tracked independently for the Adjust scope and the Mask scope (see State Management).
- **Sliders**: click-drag anywhere on the track to set value (not just the handle) — this matches egui's `Slider` drag-anywhere behavior. Double-click resets to default. Numeric value is right-aligned, monospace, turns accent-colored while being dragged.
- **Metadata Filters popup**: click "Metadata" button to open, click "Close" or "Metadata" again to dismiss; anchored to stay on-screen (opens leftward from the right edge).
- **Info panel toggle**: click the bottom-left "ℹ Info" pill to show/hide the left Info panel; button highlights (accent tint) while the panel is open.
- **Grid selection**: single click selects a thumbnail (accent 2px outline + lighter border); double-click a thumbnail in Library jumps to Develop with that photo loaded.
- **Before/After toggle** (Develop toolbar): splits/restores the canvas view; when split, a labeled "BEFORE"/"AFTER" chip appears on each half.
- **Settings > Keyboard tab**: every group's keybind labels align to one global column shared across the whole tab, not a column re-measured per group — so labels of different lengths in different groups still line up.
- **Collection drag-and-drop** (Library COLLECTIONS tree): whole collection rows are drag sources; dropping one onto another collection row nests it underneath (any depth), dropping onto the COLLECTIONS root header un-nests it back to the top level. A drop that would create a cycle makes no state change and flashes the target row red instead of committing. Dragging images from the grid onto a collection row still adds them to that collection, as before.
- No animation or transition effects were designed beyond the drag-and-drop above — everything else is an instant state change, consistent with egui's immediate-mode redraw model.

## State Management
Minimal state needed to reproduce this design in a real app:
- `active_module`: Library | Develop | Export
- `active_tool` (Develop only): Adjust | Crop | Heal | Mask
- `active_adjust_tab` / `active_mask_tab` (independent per tool): Light | Color | Effects
- Per-section collapsible-open booleans (curve/region-tones/grade/optics, one per section in every tab) — tracked **separately** for the Adjust tool vs. the Mask tool, since a mask's disclosure state shouldn't affect the global Adjust panel's, and persisted reliably across sessions for every section in both scopes. The Crop tool's own two sections (Crop & Transform, Geometry) follow the same persisted-disclosure pattern.
- `show_info_panel: bool`
- `before_after: bool`
- `filters_open: bool` (Metadata popup)
- `selected_photo_index`
- `grid_cell_size` (driven by the Size slider)
- Per-photo edit values (all the slider values) — in the real app these back actual pipeline parameters (exposure, contrast, HSL, curve points, mask list, etc.)
- Masks are a **list** per photo, each with: name, invert flag, visibility flag, component count, and its own full set of Light/Color/Effects values (scoped to that mask only)

## Design Tokens

**Colors**
| Token | Hex | Usage |
|---|---|---|
| bg-base | `#161616` | app root background |
| bg-titlebar | `#111111` | top menu bar |
| bg-toolbar | `#1a1a1a` | toolbars, right/left panels |
| bg-panel-alt | `#171717` | left nav panel, filmstrip |
| bg-canvas | `#0d0d0d` / `#131313` | image canvas / grid background |
| bg-input | `#141414` / `#101010` | combo boxes, text fields |
| border-default | `#262626` / `#2c2c2c` | 1px separators everywhere |
| border-subtle | `#232323` / `#242424` | status bar / row separators |
| text-primary | `#c8c8c8` / `#d0d0d0` / `#eaf1f6` (active) | body text |
| text-dim | `#7a7a7a` / `#8a8a8a` / `#6a6a6a` | secondary/label text |
| text-faint | `#5a5a5a` / `#666` | tertiary/meta text |
| **accent (steel/iron)** | `#6d97b5` | active tab underline, selected outline, focus, slider drag |
| accent-fill | `#232b30` | active/selected control background |
| accent-border | `#34464f` | active/selected control border |
| accent-text | `#cfe0ec` | text on accent-fill backgrounds |
| rating-star | `#c9a23a` / `#d8c458` | star glyphs |
| label-red | `#c75450` / `#c0504a` | color-label / tag swatch |
| label-green | `#5aa06a` | color-label / tag swatch / pick flag |
| label-yellow | `#c9a23a` | color-label |
| label-purple | `#8a6ab0` | color-label |
| reject-red | `#a05a5a` | reject flag |
| mask-tint | `#c02a2a` @ 42% multiply | mask visualization overlay |

**Typography**
- UI labels/body: **IBM Plex Sans** (400/500/600), 9.5–12px
- Numeric values, filenames, EXIF, code-like data: **IBM Plex Mono** (400/500), 9–11px
- Section labels (small caps style): 10px, 1px letter-spacing, 600 weight, `#6a6a6a`

**Spacing / sizing**
- Toolbars: 30px (titlebar), 38–40px (filter toolbars), 36px (Export top bar)
- Panels: 216–300px fixed widths
- Standard row height for combos/inputs: 22–24px
- Slider row height: 22px (label 74px fixed + track flex + value 48px fixed, per `EguiSlider.dc.html`)
- Border radius: 3px on controls/chips, 50% on color wheels/swatch dots — kept minimal/flat, no heavy rounding
- 1px borders throughout; no drop shadows, no blur, no gradients on chrome (gradients are used ONLY for the color-grading wheels and the RGB histogram fill, which are literal data visualizations, not decoration)

## Assets
- Photo thumbnails/canvas images: placeholder photography from `picsum.photos` (seeded, e.g. `picsum.photos/seed/ice04/1400/933`) — **replace with real thumbnail/preview rendering from the RAW decode pipeline**.
- No custom icon font was used — the mockup uses Unicode glyphs (⚙ ⌗ ✛ ◐ ☆ ★ ⚑ ⊘ ▾ ▸ ↺ ↻ ℹ etc.) as stand-ins for what should become a proper icon set in the egui build (egui supports embedding an icon font via `FontFamily`).

## Files
- `Ferrolite.dc.html` — the full three-screen design (Library, Develop with all 4 tool modes, Export). Open directly in a browser to explore.
- `EguiSlider.dc.html` — the reusable slider component used throughout Develop/Export, demonstrating the drag-anywhere-on-track + double-click-reset interaction and the label/track/value layout.
