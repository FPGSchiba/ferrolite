# Systematic UI fixes — Round 4 (design)

**Date:** 2026-07-29 · **Branch:** `feat/ui-v2-rewrite` · **Author-approved:** yes (walkthrough
annotations + two AskUserQuestion decisions + design-list approval, this session)

## Context

Workstream 2 of the V2 effort: the author annotated the full UX walkthrough
(`docs/superpowers/2026-07-29-v2-ux-walkthrough.md`). Bug-class findings were root-caused and
fixed separately (commits `cf8a440`, `44af74d`, `2531dfb`, `32f3fc8`). This spec covers the
**UX-change items**; the **Crop tool overhaul** (walkthrough 4.2) is deliberately excluded —
it gets its own spec ("a single major pass", per the author).

Accepted changes flow back into `docs/design/V2/README.md` (the living design frame;
`.dc.html` mockups are never regenerated) at the end of the round.

## Author decisions folded in

- **H/S/W/B (2.1+2.2):** parametric H/S/W/B moves OUT of TONE CURVE into its own titled
  collapsible section, with a subheader explaining the difference vs. the Basic sliders.
- **Subfolders (1.4):** moves to the Folders tree section header — it scopes the folder
  selection; it is not a filter.

## Items

### D1 — REGION TONES section (Develop · Light tab)

Move the parametric H/S/W/B sliders (`curve_widget_parametric`) out of the TONE CURVE section
into a new collapsible section **REGION TONES**, directly below TONE CURVE. One faint one-line
subheader under the section title: *"Region-based curve tones — complements the Basic sliders,
which weight by pixel brightness."* Keeps: per-control resets, scoped (`ScopedEdit`) behavior in
Adjust and Mask scope, existing slider layout. Gets: its own disclosure memory entry (same
mechanism as the other sections) and inclusion in the section-header persistence test.

### D2 — Right gutter / scrollbar offset (Develop · panel chrome)

The adjustments panel shows ~15px of dead space right of the scrollbar (walkthrough 2.6).
Root-cause the layout (double inner margin / width reservation) and make the scrollbar hug the
panel's right edge. Pure layout fix; no behavior change.

### D3 — Info button docks bottom-left when overlay hidden (Develop · canvas)

When the floating info overlay is hidden, its toggle button currently floats where the overlay
used to be. Change: overlay hidden ⇒ the toggle button anchors to the canvas **bottom-left**
corner (standard margin); overlay shown ⇒ button stays with the overlay as today.

### L1 — Subfolders toggle moves to the Folders tree header (Library)

Remove the Subfolders control from the toolbar filter cluster; render it in the left panel's
**Folders** section header row (right-aligned in the header). Same setting/state field, same
query semantics — placement only. Tooltip explains scope: *"Include images in subfolders"*.

### L2 — Reset-all-filters (Library · toolbar)

One icon button (Phosphor counter-clockwise arrow via `icons.rs`, rendered like the other
tool buttons) at the end of the filter cluster. Click resets rating, flag, metadata
(camera/lens/ISO/aperture/focal), file-type, and search to defaults in one step. Disabled
(greyed, with hover reason "All filters are at default") when nothing deviates — driven by a
pure `filters_are_default(&FilterState) -> bool` predicate with unit tests.

### L3 — Range sliders for ISO / aperture / focal (Library · Metadata popup)

Replace the current single-value controls with **dual-handle min/max range sliders** on fixed
bounds:

| Filter | Bounds | Stepping |
|---|---|---|
| ISO | 50 – 102 400 | full stops (50, 100, 200, … 102 400), log-scaled track |
| Aperture | f/0.7 – f/32 | third-stops from the standard aperture series, log-scaled track |
| Focal | 8 – 1200 mm | 1 mm (linear track) |

Each has a per-control reset (both handles back to full range = filter inactive). A range at
full bounds means "not filtering". Handle-clamping logic (`lo <= hi`, snap-to-step) is a pure,
unit-tested helper. The widget is a new shared `widgets::range_slider` (reused by all three,
built to `EguiSlider`'s visual language).

### L4 — Multi-select, resettable file-type filter (Library)

The file-type filter becomes multi-select chips (RAW / JPEG / PNG / TIFF / …, sourced from the
kinds the catalog actually knows). Empty selection = "all types" (the reset state, also what
reset-all restores). Query treats selected set as an OR-filter. Chip visuals reuse the existing
segmented-control styling.

### L5 — Collection hierarchy drag-and-drop (Library · left panel)

- Drag a collection row onto another collection ⇒ becomes its child.
- Drag a collection onto the Collections root header ⇒ un-parents (top level).
- Cycle prevention: dropping a collection onto its own descendant is rejected — no state
  change, brief red-flash affordance on the invalid target. `would_create_cycle` is a pure,
  unit-tested helper over the parent map.
- Drop-target highlight reuses the existing photo-into-collection drop affordance.
- Persisted through the existing catalog collection API (parent relationship).

### N1 — Filmstrip free-scroll (Develop · filmstrip)

User wheel/drag scrolling of the filmstrip is never snapped back. Auto-centering on the
selected image happens **only** on a navigation event: arrow-key nav, filmstrip click, or
programmatic open. Encoded as a pure snap-policy decision (`scroll_target(event, …)`) with
unit tests: user-scroll ⇒ `None`, navigation ⇒ `Some(centered)`.

### N2 — Titlebar active-module underline (Chrome)

The active module button (Library / Develop / Export) gets the V2-design underline accent
(short accent-colored rule under the label), in addition to the current active styling.

### N3 — Global keybind column alignment (Settings · Keyboard tab)

The keybind label column aligns to ONE global width across all sections (max over all rows),
not per-section. Rebind flow, grouping, and the `every_action_is_in_a_settings_group`
invariant are untouched.

## Non-goals

- Crop tool overhaul (own spec, next).
- NR / clarity / texture implementations (future effort; stay greyed).
- Status/feedback work (author: "come back once we start work on V3").
- Regenerating `.dc.html` mockups.

## Conventions binding every item

- Icons only via `icons.rs` aliases (Phosphor); no raw glyphs, no hand-drawn `Painter` icons.
- Any keybound control shows its key via `Keymap::hint` in the tooltip.
- New editable/filter controls get a per-control reset affordance.
- No UI-thread blocking work; catalog writes go through the existing job/event paths.
- Tests hermetic: reset `Settings::default()` after `AppState::new()` in app-state tests.
- Scoped gate per task (`ferrolite-app`, plus `ferrolite-catalog` where touched); repo gate
  once at round end.

## Testing

Pure-logic helpers (`filters_are_default`, range clamp/snap, `would_create_cycle`, filmstrip
snap policy) get unit tests. Section/registry invariants extend the existing header-persistence
and settings-group tests. Visual correctness lands in the author's hands-on checklist at the
end of the round.
