# ferrolite — Editable brush / linear / radial mask components (design)

> **Status:** Design — pending user review, then writing-plans.
> **Date:** 2026-07-07
> **Branch:** `fix/brush-mask-perf` (continues the masking work; this is a UX feature, separate from
> the parallel adjustment-path perf investigation).
> **Builds on:** the Components modal (`mask_components_modal.rs`, edit of Luma/Color), the canvas
> affordance routing (`mask_overlay.rs` `show`/`route_brush`), the hover-highlight
> (`highlight_component` → white overlay), and the brush-merge model (strokes append to an active
> Brush component).
> **Goal:** let the user re-edit an existing **Brush**, **Linear gradient**, or **Radial gradient**
> component from the Components modal — not just Luma/Color — by making "Edit" activate that specific
> component for canvas editing, with the edited component highlighted white.

---

## 1. Problem

Today the Components modal's **Edit** button only appears for Luma/Color range components (they have
scalar sliders); `is_editable` is `false` for Brush/Linear/Radial. So:
- A linear/radial gradient can only be edited via canvas handles, and the overlay routing targets the
  **first** matching component for the active tool — so the 2nd+ linear/radial of a mask is
  **uneditable** (a latent bug).
- A brush component, once committed, cannot be resumed — new strokes only append to the *last* brush
  component, so you can't go back and add to an earlier brush layer.

The user wants to edit all three, "like other components can be edited."

## 2. Design — "Edit" makes a component the active canvas-edit target

Clicking **Edit** on any editable component sets `mask.editing_component = Some(i)`, switches the
active tool to that component's tool, and (per §2.4) highlights it white. The canvas affordances then
act on **that specific component**.

### 2.1 `is_editable` covers all authorable component types
Extend `is_editable` to return true for `Brush`, `LinearGradient`, `RadialGradient` (in addition to
`LumaRange`/`ColorRange`). `Imported` stays non-editable (no producer in P1). So the Edit button
shows for every hand-authored component.

### 2.2 Edit activates the matching tool + targets the component
The Edit-button handler (in `mask_components_modal.rs`) already sets `editing_component` +
`overlay_on`. Extend it to also set `mask.tool` to the component's tool via a pure mapping
`tool_for_component(&MaskComponent) -> Option<MaskTool>` (Brush→Brush, LinearGradient→Linear,
RadialGradient→Radial, LumaRange→LumaRange, ColorRange→ColorRange, Imported→None). For Luma/Color it
still also `load_component_into_state` (seed the inline sliders); for Radial it loads feather/invert
(§2.3); for Brush/Linear there is nothing scalar to seed.

### 2.3 Canvas targeting prefers `editing_component`
Both routing paths currently pick a component implicitly; make them prefer the edited one:
- **Linear/Radial handles** (`mask_overlay::show`): the `existing`/`existing_radial` finders select
  the component matching the active tool. Change "first matching" to: **if `editing_component` is
  `Some(i)` and component `i` matches the active tool, target `i`; otherwise** fall back to the first
  match (create-on-drag behavior for a fresh gradient is unchanged). This makes the 2nd+ gradient
  editable and scopes handle edits to the chosen component.
- **Brush** (`route_brush`): when starting a stroke, if `editing_component` is `Some(i)` and component
  `i` is a `Brush`, append into `i` (via `set_brush_with_base` with that comp's stroke count);
  otherwise keep the current "append to `last_brush_index`, else create" behavior. So "Edit a brush
  layer → paint" resumes that specific layer.

### 2.4 Radial inline Feather + Invert
Radial's `center`/`radius`/`rotation` are spatial (canvas handles); `feather` and `invert` are scalar.
Add a `RadialGradient` arm to `edit_component_ui` with a **Feather** `EguiSlider` + an **Invert**
toggle + **Update/Done**. Needs `MaskUiState` fields `radial_feather: f32`, `radial_invert: bool`,
seeded by `load_component_into_state`'s new `RadialGradient` arm. On **Update**, rebuild the radial
**preserving the current `center`/`radius`/`rotation`** (read from the live component) and applying the
new `feather`/`invert` — so an inline feather change and a handle drag never clobber each other's
fields. (The handle-drag path already preserves `feather` when it rewrites the component.)

### 2.5 Brush/Linear inline editor = hint + Done
Brush and Linear have no scalar params, so their `edit_component_ui` arm shows a short hint
("Drag the endpoints on the canvas" / "Paint on the canvas to add to this layer") plus a **Done**
button that clears `editing_component` (and thus the white highlight). Radial's editor also has
**Done**. This gives a consistent way to *exit* edit mode from the modal.

### 2.6 White highlight of the component being edited
Reuse the existing `highlight_component` → white-overlay path. Set the drawn highlight to
`hovered_row.or(editing_component)`: hovering a row highlights that row's component (transient, as
today); when not hovering, the **component currently being edited stays highlighted white** so the
user always sees which one their canvas edits affect. Cleared when editing ends (Done / modal close /
deselect) — the existing reset sites already null `editing_component` and `highlight_component`.

## 3. Data flow (edit a 2nd radial, example)
```
Components modal: click Edit on component #3 (a RadialGradient)
  → editing_component = Some(3); tool = Radial; overlay_on = true;
    load feather/invert into radial_feather/radial_invert; highlight_component→3 (white)
Canvas: drag the radial handles
  → mask_overlay::show targets component 3 (editing_component), not the first radial
  → radial_drag rewrites component 3's center/radius (feather preserved) → EditOutcome (commit on release)
Modal: change Feather slider → Update
  → rebuild component 3 with new feather/invert, preserving its center/radius/rotation → EditOutcome
Modal: Done → editing_component = None → white highlight clears
```

## 4. Error handling
- **`editing_component` out of range / component deleted while editing:** all lookups are
  bounds-checked (`get(i)`); a stale index → no target (canvas falls back to first-match / no-op),
  no panic. Deleting the edited component (or its mask) clears `editing_component` (existing reset
  sites + a bounds check).
- **Tool/type mismatch:** `editing_component` only targets a component whose type matches the active
  tool; if the user switches tools while editing, the target simply doesn't match → fall back
  (documented + tested).
- **Empty brush layer edited:** appending into an empty edited brush works (base_count 0), as already
  covered by the brush-merge helpers.
- Nothing here adds GPU/UI-thread cost beyond the existing overlay (highlight of one cached
  component; CLAUDE.md §1/§2 unaffected).

## 5. Testing (TDD; CLAUDE.md gate, then hold for the author's visual test)
**Pure/CPU logic (unit-tested, egui-free):**
- `tool_for_component` mapping for every `MaskComponent` variant (incl. `Imported` → `None`).
- Editing-target selection: given `editing_component` + active tool, the overlay picks the edited
  component when it matches, else the first match / none. (Extract the selection to a pure helper.)
- `route_brush` target selection: `editing_component` Brush wins over `last_brush_index`; non-brush /
  out-of-range editing_component falls back.
- Radial load/rebuild: `load_component_into_state` seeds `radial_feather`/`radial_invert`; the
  Update-rebuild preserves `center`/`radius`/`rotation` and applies the new feather/invert (pure
  `radial_from_state`-style helper + a preserve-geometry test).
- `highlight_component = hovered.or(editing_component)` selection.

**egui UI** (Edit buttons for the new types, inline radial editor, hints + Done): build + clippy +
the author's hands-on visual test. No golden tests for egui rendering.

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → **hold for Jann's visual test** (edit 1st AND 2nd gradient of a
mask; resume painting an earlier brush layer; radial feather/invert inline; white highlight tracks
the edited component; Done exits).

## 6. Non-goals
- No new component *types*; no changes to compositing/perf (that's the separate adjustment-path
  investigation).
- No per-stroke editing within a brush component (a brush edits as a whole; erase removes coverage).
- No reordering / layers panel (unchanged from the prior decision).
- Linear/Brush get no scalar inline editor (they have no scalar params) — canvas + Done only.

## 7. Decisions recorded (2026-07-07)
| Question | Decision | Rationale |
|---|---|---|
| What "Edit" does for Brush/Linear/Radial | **Makes that component the active canvas-edit target** (tool switch + `editing_component`) | Matches how the app already authors these (handles / painting); unifies "edit = the canvas acts on THIS component"; fixes the 2nd-gradient-uneditable bug. |
| Radial scalars | **Inline Feather slider + Invert toggle** (center/radius/rotation stay on-canvas) | Feather/invert have no spatial handle; editing them inline is natural; geometry stays handle-driven. |
| Brush edit | **Resume painting into that layer** (route_brush targets `editing_component`) | A brush has no scalar params; "edit" = keep adding strokes to the chosen layer. |
| Edit feedback | **White highlight of the edited component** (`highlight_component = hovered.or(editing_component)`) | Reuses the hover-highlight; always shows which component the canvas edits affect. |
| Exit edit mode | **Done button** in the modal (+ existing close/deselect resets) | Consistent, discoverable way to clear `editing_component`. |
