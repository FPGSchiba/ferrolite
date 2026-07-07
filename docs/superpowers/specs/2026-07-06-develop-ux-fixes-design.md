# ferrolite — Develop UX fixes: icon library, keybinds, mask overlay toggle, color-picker fix, component editing (design)

> **Status:** Design — approved by user (2026-07-06); pending writing-plans.
> **Date:** 2026-07-06
> **Branch:** `feat/develop-tool-registry` (these are **visual-test-feedback fixes** for the just-landed Develop tool-registry refactor — addressed before that branch is finished, per CLAUDE.md "address any issues found before completing the branch"). Not a new branch.
> **Context:** the Develop tool registry / floating palette / tabbed panel refactor
> (`2026-07-06-develop-tool-registry-design.md`) and its code (`ferrolite-app/src/develop/`:
> `tool.rs`, `tool_palette.rs`, `tool_panel.rs`, `tools/*`, `base_tabs.rs`, `mask_panel.rs`,
> `mask_overlay.rs`, `mask_ui.rs`, `mask_edit.rs`, `mask_affordance.rs`); the font/theme setup
> (`theme.rs`), the existing vector-icon precedent (`library/icons.rs`, `widgets/mod.rs`), the
> keymap system (`settings/keymap.rs`, `settings/ui/keyboard.rs`, dispatch in `app.rs`), and the P1
> masking specs.
> **Proves:** the author's hands-on test of the tool-registry refactor surfaced concrete gaps — the
> new tool/sub-tool/undo-redo icons don't render, the mask color-picker doesn't work, there's no way
> to toggle the red mask overlay or to edit/remove individual Luma/Color mask components, and there
> are no tool/brush keybinds. This spec fixes all of that and, for icons, installs a **permanent icon
> library** (an icon-font crate) so icons never silently break again.

---

## 1. Goal & validation

Fix the issues found testing the Develop tool-registry refactor, and put icons on a durable footing:

> In the running Develop module: **every tool/sub-tool/undo-redo/eyedropper/edit/delete icon renders**
> (no tofu); **keybinds** switch tools (A/C/M), change brush radius ([ / ]), and toggle the mask
> overlay (O), all rebindable in Settings; the **mask overlay** (red tint) can be toggled on/off
> persistently so the real adjusted image is visible at rest; the **color-range eyedropper** shows its
> picker cursor + zoom loupe and samples pixels on click; and a selected mask's **individual Luma/Color
> components can be edited and removed** from a per-component list. Adding a new icon anywhere in the
> app is a one-line reference to the icon library.

**Success = the running app** demonstrates each of the above, and a developer can add a new icon by
referencing the `icons` module (or the icon crate's catalog) — no font-glyph guesswork. Automated gate
green, then the author's hands-on visual test (CLAUDE.md).

---

## 2. Scope

**In:**
- **Icon library (permanent fix):** add the **`egui-phosphor`** crate (MIT; Phosphor icons MIT); install
  its font once in `theme::install_fonts`; add a thin `ferrolite-app` `icons` module of **semantic
  aliases** over the crate's constants; render icons in the crate's font family. Add a **CLAUDE.md rule**
  codifying the icon convention.
- **App-wide icon audit + migration (MUST — a guarantee, not a nice-to-have):** every icon in the app is
  sourced from the `icons` library so nothing can silently render as tofu again. This covers the broken
  Develop tool/sub-tool/undo-redo/eyedropper/edit/delete icons AND every other icon/glyph in the app:
  the hand-drawn vector icons in `library/icons.rs` (rating stars, flags, chevrons) and the per-control
  reset glyph `draw_reset_arrow`, plus any raw emoji/symbol characters found anywhere else. The plan
  begins with an audit that enumerates every icon/glyph/hand-drawn-icon call site; all are migrated. The
  only permitted exception is a genuinely bespoke shape with no adequate Phosphor equivalent, which must
  be explicitly named and justified in the plan (default = migrate).
- **Keybinds:** new Develop-gated `Action`s — tool switch (Adjust/Crop/Mask), brush-radius decrease/
  increase (key-repeat), toggle-mask-overlay — with default chords, conflict resolution, the Settings
  rebind UI, the in-app help cheat sheet, and automatic persistence.
- **Mask overlay toggle:** a UI toggle (mask panel) + the keybind, both flipping the persistent
  `MaskUiState.overlay_on`; keep the existing drag-time auto-hide.
- **Color-picker fix:** root-cause + fix so the armed color eyedropper reliably shows its cursor/loupe
  and samples on click.
- **Mask component edit/remove:** a pure `mask_edit::remove_component`; a `MaskUiState.editing_component`
  state; a per-component list UI in the selected mask (delete any; edit Luma/Color back through the
  sliders via `set_component`).

**Out (non-goals / later):**
- **Brush-mask performance** (the lag) → a separate brainstorm → spec (diagnostics first, then fix).
- No new adjustment ops, mask component types, or pipeline/OpStack/persistence changes.
- No re-theming beyond the icon-font addition and the small controls this adds.
- (Icon migration is comprehensive and in-scope — see the In list. The only carve-out is a bespoke shape
  with no Phosphor equivalent, which the plan must name + justify; everything else migrates.)

---

## 3. Architecture of the slice

```
ferrolite-app
  Cargo.toml            + egui-phosphor (version matching egui 0.29)                    [NEW dep]
  src/theme.rs          install_fonts(): egui_phosphor::add_to_fonts(&mut fonts, ...)   [MODIFY]
  src/icons.rs          semantic aliases over egui_phosphor::regular::* (CROP, MASK, STAR,
                        FLAG, CARET_DOWN, RESET, …)                                     [NEW]
  src/widgets/tool_button.rs   render icon in the phosphor font family                  [MODIFY]
  src/widgets/mod.rs    draw_reset_arrow -> render icons::RESET in the reset column     [MODIFY]
  src/library/icons.rs  stars/flags/chevrons -> icons::* (bespoke drawers removed)      [MODIFY]
  (+ audit: any other raw emoji/symbol glyph call site app-wide -> icons::*)            [MODIFY]
  src/develop/
    tools/{adjust,crop,mask,heal}.rs   icon() -> icons::* constant                      [MODIFY]
    tool_palette.rs     undo/redo icons -> icons::UNDO / icons::REDO                     [MODIFY]
    mask_panel.rs        sub-tool strip icons -> icons::*; overlay toggle;
                         "Manage components" button opening the modal                  [MODIFY]
    mask_components_modal.rs  the component-management modal (list + delete + edit)     [NEW]
    mask_overlay.rs      color-eyedropper routing fix (relax selection gate);
                         Ctrl+scroll brush-radius gesture (gated + consumes scroll)     [MODIFY]
    mask_ui.rs           + components_modal_open: bool, editing_component: Option<usize> [MODIFY]
    mask_edit.rs         + remove_component(stack, mask_idx, comp_idx)                  [MODIFY]
  src/settings/keymap.rs        + Action variants + ALL + label() + defaults()          [MODIFY]
  src/settings/ui/keyboard.rs   + new actions in the Develop GROUPS entry               [MODIFY]
  src/help.rs                   + new actions in the cheat-sheet table                  [MODIFY]
  src/app.rs                    Develop-gated key dispatch for the new actions          [MODIFY]
  CLAUDE.md                     + icon convention rule                                  [MODIFY]
```

No pipeline/OpStack/persistence/`EditOutcome`/`apply_edit` change. The icon crate installs a font
family; everything else is UI wiring + one pure helper + one keymap extension.

---

## 4. Icon library (permanent)

**Root cause (from investigation):** `theme::install_fonts` installs only IBM Plex Sans/Mono; the new
tool icons used emoji/symbol chars (`⌗ ◯ 🎚 🩹 🖌 ▤ ◎ ◐ 🎨 ↶ ↷`) via `painter.text(...)`, and no
installed font (Plex + egui 0.29's small bundled emoji subset) covers them → tofu. The codebase already
solved this once for ratings/flags with hand-drawn vector shapes (`library/icons.rs`,
`widgets::draw_reset_arrow`) — but that convention wasn't followed for the tool icons, and hand-drawing
every future icon doesn't scale. The durable fix is an icon-font library.

### 4.1 The crate
- Add **`egui-phosphor`** to `ferrolite-app/Cargo.toml`. Phosphor is a ~1.5k-icon set across weights;
  we use the **Regular** weight. License MIT.
- **Version:** pick the `egui-phosphor` release whose `egui` dependency matches the workspace's egui
  `0.29` (the plan's first task pins the exact version). **Fallback:** if no compatible published
  version exists, vendor that crate's Regular `.ttf` into `assets/fonts/` and register it directly (the
  crate is just a font + generated constants; the fallback yields the same result).
- **Dependency-fetch note:** adding a crate triggers a crates.io fetch; in this environment that may
  require the one-time author-authorized `CARGO_HTTP_CHECK_REVOKE=false` fetch, then an offline build
  (see the repo's known TLS-revocation issue). The plan flags this; it is not done unprompted.

### 4.2 Font install
In `theme::install_fonts` (after inserting Plex Sans/Mono), call
`egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular)` **before** `ctx.set_fonts(fonts)`.
This registers the Phosphor font in the fallback chain (and/or a named family per the crate's API — the
plan uses whatever the crate documents). IBM Plex remains the primary text font; Phosphor supplies the
icon glyphs.

### 4.3 The `icons` module (semantic aliases)
`ferrolite-app/src/icons.rs` — a thin, stable indirection so call sites never hard-code crate paths:

```rust
//! Semantic icon aliases over the bundled icon font (egui-phosphor). UI code uses
//! these constants (e.g. `icons::CROP`) rendered in the icon font family — never raw
//! emoji/symbol glyphs, which the text fonts (IBM Plex) + egui's emoji subset don't
//! cover. Add a new icon = add one alias here (or reference the crate's catalog).
use egui_phosphor::regular as p;
pub const ADJUST: &str = p::SLIDERS_HORIZONTAL; // exact constant names verified in the plan
pub const CROP: &str = p::CROP;
pub const MASK: &str = p::CIRCLE;               // or CIRCLE_HALF / MASK-like glyph
pub const HEAL: &str = p::BANDAIDS;             // first-aid / bandage
pub const BRUSH: &str = p::PAINT_BRUSH;
pub const LINEAR_GRADIENT: &str = p::/* linear-gradient-like */;
pub const RADIAL_GRADIENT: &str = p::/* radial/circle-gradient-like */;
pub const LUMA: &str = p::CIRCLE_HALF;
pub const COLOR: &str = p::PALETTE;
pub const EYEDROPPER: &str = p::EYEDROPPER;
pub const UNDO: &str = p::ARROW_COUNTER_CLOCKWISE;
pub const REDO: &str = p::ARROW_CLOCKWISE;
pub const DELETE: &str = p::TRASH;
pub const EDIT: &str = p::PENCIL_SIMPLE;
pub const OVERLAY_ON: &str = p::EYE;
pub const OVERLAY_OFF: &str = p::EYE_SLASH;
```
(The exact Phosphor constant for each alias is confirmed against the crate's generated constants during
the plan — the names above are the intended mapping; a couple like the two gradients are picked from the
nearest Phosphor glyph.)

### 4.4 Rendering
- `widgets::tool_button` currently does `painter.text(center, …, icon, FontId::proportional(15.0), fg)`.
  Change it to render the icon in the **icon font family** — `FontId::new(15.0, egui_phosphor's family)`
  (or `FontId::proportional` if the crate adds Phosphor to the Proportional fallback chain such that the
  PUA codepoints resolve — the plan uses whichever the crate's `add_to_fonts` mode dictates). The `icon:
  &str` parameter stays; call sites pass `icons::CROP` etc.
- Update all icon call sites: `tools/{adjust,crop,mask,heal}.rs::icon()`, the `mask_panel.rs` sub-tool
  strip array, and `tool_palette.rs` undo/redo, to the `icons::*` constants.

### 4.5 App-wide audit + migration (MUST)
The library is the single source for **all** icons — a guarantee that nothing renders as tofu. The plan
starts with an **audit task** that enumerates every icon/glyph call site in `ferrolite-app`:
- Raw emoji/symbol characters passed to `painter.text`, `ui.label`, `RichText`, `Button`, `selectable_label`,
  combo/menu text, etc. (grep for non-ASCII glyph literals + the known offenders `⌗ ◯ 🎚 🩹 🖌 ▤ ◎ ◐ 🎨 ↶ ↷`).
- The hand-drawn vector icons in `library/icons.rs` (rating **stars**, **flags**, **chevrons**) →
  Phosphor `STAR`/`STAR_FILL`, `FLAG`/`FLAG_FILL` (+ a reject variant), `CARET_DOWN`/`CARET_UP`, etc.,
  rendered in the icon font. Preserve current sizing/placement/fill-state semantics (e.g. filled vs
  outline star for rating level); verify visual parity in the author's test.
- The per-control reset glyph `widgets::draw_reset_arrow` → the library's reset icon
  (`icons::RESET`, e.g. Phosphor `ARROW_COUNTER_CLOCKWISE`), rendered in the `EguiSlider` reset column
  and any other reset affordance. **Load-bearing:** the per-control reset affordance + its placement
  (CLAUDE.md) are unchanged — only the drawn glyph is swapped for the library icon; verify the reset
  column still looks/behaves right at the author's test.

Every enumerated site is migrated. If the audit finds a shape with no adequate Phosphor equivalent, the
plan names it and either picks the closest Phosphor glyph or justifies keeping a bespoke vector draw —
the default is to migrate. Once migrated, `library/icons.rs`'s bespoke drawers and `draw_reset_arrow` are
removed (or reduced to a thin wrapper over the library) so there is exactly one icon system.

### 4.6 CLAUDE.md rule (new, added by this work)
> **UI icons (load-bearing).** EVERY icon in the app comes from the `icons` module
> (`ferrolite-app/src/icons.rs`), which aliases the bundled icon font (`egui-phosphor`, installed in
> `theme::install_fonts`), rendered in the icon font family (via `tool_button` or a `FontId` with that
> family). This includes rating/flag/chevron icons and the per-control **reset** glyph. NEVER put raw
> emoji/symbol characters in IBM Plex text, and do NOT hand-draw new icons with `Painter` shapes — Plex
> + egui's bundled emoji subset don't cover symbols (tofu), and ad-hoc vector icons fragment the system.
> Add a new icon by adding a semantic alias in `icons.rs` sourced from the Phosphor catalog. (The
> per-control reset affordance + its placement remain load-bearing; only its glyph comes from the
> library.)

---

## 5. Keybinds

Extend the existing keymap (`settings/keymap.rs`): the pattern is add an `Action` variant + append to
`Action::ALL` + a `label()` arm + a default `Chord` in `defaults()`; dispatch is a `keymap.pressed()`/
`held()` check in `app.rs`; the Settings rebind UI auto-lists actions from a `GROUPS` table; persistence
is automatic (`Keymap` is `#[serde(default)]` in `Settings`).

### 5.1 New keymap actions + default chords
| Action | Default | Trigger | Effect |
|---|---|---|---|
| `SwitchToolAdjust` | `A` | `pressed` | select `ToolId::Adjust` |
| `SwitchToolCrop` | `C` | `pressed` | select `ToolId::Crop` |
| `SwitchToolMask` | `M` | `pressed` | select `ToolId::Mask` |
| `ToggleMaskOverlay` | `O` | `pressed` | flip `mask.overlay_on` |

- **Conflict handling:** `O` is `FlagReject`'s default. The plan runs `Keymap::conflict()` for each new
  default; if a default collides with an action reachable in the same (Develop) context, pick a free key
  (documented in the plan). All are rebindable, so a collision is not fatal — but defaults must not
  shadow an existing Develop action.
- **Module gating:** all four are handled only inside the existing `self.module == Module::Develop &&
  self.state.viewer.is_some()` block in `app.rs` (mirroring NextImage/PrevImage), so they never fire in
  Library/Export.
- **Tool switch** reuses the palette's exact enable-check + `ToolState::select_tool(id, enabled, reg)`.

### 5.2 Brush radius = Ctrl + Mouse Scroll (canvas gesture, NOT a keymap chord)
Brush radius is adjusted by **Ctrl + mouse scroll** over the canvas — a pointer gesture, not a key
combination, so it does not go through the `Chord` keymap (which is key+modifier only) and is not a
rebindable `Action`. Handling:
- Read the scroll delta + modifier from egui input (`ctx.input(|i| (i.modifiers.command_or_ctrl, i.raw_scroll_delta.y / i.smooth_scroll_delta.y))` — the plan picks the field that matches the existing canvas-zoom code).
- **Gated context:** only when the **Mask tool is active AND the Brush sub-tool is selected** and the
  pointer is over the image; scroll `up` → larger, `down` → smaller; apply a sensitivity factor;
  **clamp** to the same range as the panel's brush-radius slider. Mutates `v.mask.brush_radius`.
- **Conflict with canvas zoom:** Ctrl+scroll (and/or plain scroll) may already drive canvas zoom
  (`drive_viewer`). When the brush gesture is active (Mask+Brush + Ctrl held over the image), the brush
  handler **consumes** the scroll so zoom does not also fire; otherwise scroll/zoom behaves exactly as
  today. The plan confirms the exact zoom-input path and consumes appropriately (no double-handling).
- Rebinding the modifier/gesture is out of scope (the keymap models key chords, not scroll gestures); a
  future setting could add it.

### 5.3 UI surfaces
- Add the new keymap actions to the `"Develop"` entry of `settings/ui/keyboard.rs`'s `GROUPS`
  (auto-appears in the rebind grid with a reset arrow).
- Add them to `help.rs`'s cheat-sheet table for discoverability, plus a line documenting the
  **Ctrl+scroll = brush size** gesture (even though it isn't a rebindable action).

---

## 6. Mask overlay toggle

- `MaskUiState.overlay_on` is currently hard-coded `true` with no control. Add a **UI toggle** in the
  mask panel (an `icons::OVERLAY_ON`/`OVERLAY_OFF` button or a labeled checkbox, near the masks list or
  the selected-mask header) that flips `mask.overlay_on`, plus the `ToggleMaskOverlay` keybind (§5).
- **Keep** the existing drag-time auto-hide: the overlay fill gate stays `overlay_on && !adjusting`
  (dragging a Light/Color slider still momentarily reveals the real effect). With `overlay_on == false`
  the red tint is off persistently, so the user sees the actual adjusted image at rest — the requested
  capability.
- No pipeline change: this only gates the existing overlay-tint paint in `mask_overlay.rs`.

---

## 7. Color-picker fix

**Symptom:** with the Color sub-tool armed ("Picking… (click image)"), there's no cursor change, no
loupe, and clicking does nothing.

**Root-cause candidates (from investigation), to be confirmed via systematic-debugging in the running
app:**
1. **Selection gate (primary suspect):** `mask_overlay::show` returns early unless `mask.selected`
   is `Some` **and** `mask.active`, *before* `route_color_eyedropper` runs — so arming the picker
   without a mask selected makes the whole overlay routing (including the eyedropper's `ui.interact`) a
   silent no-op, and `drive_viewer`'s canvas interact (registered earlier in the frame) consumes the
   click. The panel still shows "Picking…" because that label is drawn unconditionally.
2. **`preview_source` is `None`:** both the click-sample and the loupe are gated on `preview_source`
   being `Some`; if it isn't populated for the current preview, clicking samples nothing and no loupe
   draws.

**Fix direction:**
- Decouple the color eyedropper from requiring a *selected mask*: color samples are staged in
  `MaskUiState.color_samples` and only committed by "Add Color range" (which does need a selected mask).
  So the armed eyedropper (cursor + loupe + sampling into `color_samples`) should run whenever the Mask
  tool is active and `picking_color` is set, regardless of `mask.selected` — with "Add Color range"
  guarded (disabled + a hint) when no mask is selected. Restructure `mask_overlay::show` so the
  eyedropper routing is reachable in that case (e.g. handle the `ColorRange + picking_color` path before
  the `selected`-gated block, or relax the gate for this tool).
- Ensure the eyedropper's sample source is available (confirm `preview_source` is threaded/populated;
  if it can be legitimately `None`, show the armed cursor but no loupe and don't pretend to sample).
- **Verification is hands-on in the running app** (systematic-debugging: reproduce, confirm which
  candidate is the actual cause, fix, verify the loupe appears and clicking adds a swatch).

The loupe render + `sample_source`/`display_to_source` math (added in the tool-registry work) are reused
unchanged; this is a routing/gating fix, not new sampling logic.

---

## 8. Mask component edit/remove

**Current gap:** a selected mask shows only a "N components" count — no per-component list, no remove,
no way to re-tune an existing Luma/Color component. `mask_edit::set_component(stack, mask_idx, comp_idx,
c)` (edit-in-place) already exists (used by canvas drag handlers) but no `remove_component`, and no UI.

### 8.1 Pure helper
Add `mask_edit::remove_component(stack: &OpStack, mask_idx: usize, comp_idx: usize) -> OpStack` —
bounds-checked `Vec::remove` on `layer.mask.components` (no-op if out of range), mirroring `delete_mask`'s
shape. Unit-tested (removes the right index; out-of-range → unchanged clone; removing the last component
leaves the mask layer present with an empty component list).

### 8.2 UI state
Add to `MaskUiState`:
- `components_modal_open: bool` (default `false`) — whether the component-management modal is open.
- `editing_component: Option<usize>` (default `None`) — which component index the modal is currently
  editing (vs. no component selected for edit).
Both reset on mask switch / undo / redo / mask-tool deselect (mirror the existing transient-state
resets), so the modal never references a stale mask/component.

### 8.3 A dedicated modal (NOT inline in the panel)
The panel is already dense (296px), so component management lives in its **own modal**, not the
selected-mask section:
- **Panel trigger:** in `mask_panel::selected_section`, the "N components" count becomes (or is joined
  by) a **"Manage components"** button (with `icons::EDIT`/a list icon) that sets
  `mask.components_modal_open = true`. That is the panel's only addition — no inline list.
- **The modal** (`mask_components_modal.rs`, an `egui::Window`/modal following the app's existing
  modal pattern — e.g. `show_settings`/`show_help`: `Order::Foreground`, closable, and integrated with
  `modal_active()` so canvas/keybind input is suppressed while open):
  - Header: the selected mask's name.
  - **Component list:** loop `layer.mask.components.iter().enumerate()`; each row = a short type label
    (Brush / Linear / Radial / Luma / Color / Imported) + its `CompositeMode` + a **delete** button
    (`icons::DELETE`) that calls `mask_edit::remove_component(stack, idx, i)` (→ `commit`), and — for
    **Luma/Color** — an **edit** button (`icons::EDIT`) that sets `editing_component = Some(i)`.
  - **Inline param editor (self-contained in the modal):** when `editing_component == Some(i)` and that
    component is Luma/Color, show its parameter sliders (Luma: lo/hi/softness; Color: tolerance/softness
    + its sample swatches) initialised from the component's current values, plus an **"Update"** button
    that calls `mask_edit::set_component(stack, idx, i, new_component)` and clears `editing_component`,
    and a **"Cancel"** that clears it without writing. Reuse the same `EguiSlider`s (per-control reset
    preserved).
  - Brush/gradient/Imported components appear in the list with a **delete** button but **no edit**
    button (their editing is spatial/canvas-based, or N/A for Imported).
  - Deleting the component currently being edited clears `editing_component`. Closing the modal clears
    `editing_component` (and `components_modal_open`).
- **Return path:** the modal is rendered from the same place the mask tool/panel is driven and returns
  its `Option<EditOutcome>` (delete/update) into the existing `apply_edit` path — the exact wiring
  (where the modal is shown + how its outcome reaches `apply_edit`) is a plan detail, mirroring how
  the panel's outcome flows today.
- The **add-new-component** flow (tool picker + "Add Luma/Color range" + their sliders) **stays in the
  panel** as today — the modal is for managing/editing/removing existing components, keeping creation
  and management cleanly separated.

---

## 9. Error handling / edge cases

- **Icon crate version mismatch** → the plan pins the compatible version or vendors the `.ttf`; if the
  font family isn't registered, icons must degrade to *something* legible, not panic (the crate handles
  its own font; our aliases are just `&str`).
- **Overlay toggle with no mask / no selection** → toggling `overlay_on` is always safe (just gates a
  paint); if no overlay texture exists, nothing is painted (as today).
- **Eyedropper with no source** → armed cursor shows; no loupe/sample if `preview_source` is `None`;
  never panics.
- **remove_component / set_component out of range** → no-op clone (bounds-checked), never panics.
- **editing_component / components_modal_open stale** (component removed, mask switched, mask tool
  deselected, or undo/redo) → both cleared; the modal closes / stops editing a wrong index; never edits
  or renders a stale mask/component.
- **Ctrl+scroll brush gesture** → only fires in the Mask+Brush context over the image; clamps
  `brush_radius` to the slider range (no runaway); consumes the scroll so canvas zoom doesn't double-fire;
  outside that context, scroll/zoom is unchanged.
- **Keybind conflicts** → resolved at default-assignment time via `Keymap::conflict()`; user rebinds
  freely.
- **Nothing slow on the UI thread (CLAUDE.md §1):** all of this is plain egui + one pure helper; no new
  per-frame heavy work. (Brush-mask perf is the separate follow-up.)

---

## 10. Testing

**Pure CPU logic (unit-tested, the 80%+ target):**
- `mask_edit::remove_component`: correct index removed; out-of-range → unchanged; last-component removal
  keeps the layer.
- The edit-mode transition helper(s) if any pure logic is extracted (e.g. loading a `MaskComponent`'s
  params into the slider fields, and building the updated component from them) — kept as a pure,
  tested unit where practical.
- `icons` module: a test asserting each alias is non-empty (and, if feasible, distinct) — guards against
  a typo'd/empty constant.
- Keymap: the existing `defaults_bind_every_action` test must still pass with the new variants (ALL
  array + defaults updated); add a test that the new defaults don't conflict within the Develop set
  (`Keymap::conflict()` returns `None` for each new default against the others).

**egui rendering / interaction** (icon rendering, tool_button, sub-tool strip, palette, overlay toggle,
the color-picker loupe, the component list, keybind dispatch): `cargo build` + clippy + the author's
hands-on visual test. No egui golden tests (matches the existing UI-testing discipline).

**Systematic-debugging** for the color picker: reproduce in the running app, confirm the actual root
cause, fix, verify the loupe + sampling work.

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → then STOP and hold for the author's hands-on visual test (CLAUDE.md).

---

## 11. Decomposition into implementation plans

Likely a **single plan** on `feat/develop-tool-registry`, in dependency order:
1. **Icon library + full audit/migration:** add `egui-phosphor` (pin egui-0.29-compatible version),
   install in `theme`, create `icons.rs` aliases, switch `tool_button` to the icon font, add the
   CLAUDE.md rule. **Audit** every icon/glyph/hand-drawn-icon call site in `ferrolite-app` and migrate
   ALL of them: the broken Develop tool/sub-tool/undo-redo icons, `library/icons.rs` (stars/flags/
   chevrons), the `draw_reset_arrow` reset glyph (→ `icons::RESET`, preserving the reset column's
   placement/behavior), and any other raw emoji/symbol usage. Remove the superseded bespoke drawers.
   Verify all icons render + reset-column parity (build + visual). This step may split (crate+install,
   then the migration sweep, then the reset-glyph swap) if large — the writing-plans step decides.
2. **Keybinds + brush gesture:** new keymap `Action`s (tool switch A/C/M + overlay toggle O) + `ALL` +
   `label()` + defaults (+ conflict resolution), Develop-gated dispatch, rebind `GROUPS` + help table;
   AND the **Ctrl+scroll brush-radius** canvas gesture (gated to Mask+Brush, clamped, consumes scroll).
3. **Mask overlay toggle:** UI toggle in the mask panel wired to `overlay_on` (keybind from step 2).
4. **Color-picker fix:** systematic-debugging → relax the eyedropper's selection gate + ensure source;
   verify loupe + sampling.
5. **Component edit/remove (modal):** `remove_component` (pure + tests) + `components_modal_open` /
   `editing_component` state + the `mask_components_modal.rs` modal (list + delete + Luma/Color inline
   edit via `set_component`); panel gets a "Manage components" button; wire the modal's outcome into
   `apply_edit`.
6. Gate green + author visual test.

If the icon step's font-family/rendering details prove fiddly, it may split from the icon call-site
swaps; the writing-plans step decides final task granularity.

---

## 12. Decisions recorded (resolved during brainstorming, 2026-07-06)

| Question | Decision | Rationale |
|---|---|---|
| Icon approach | **Bundle an icon-font crate (`egui-phosphor`) + a thin `icons` alias module + a CLAUDE.md rule** | A reusable icon *library* is a permanent fix — future UI sources any glyph from the catalog or adds a one-line alias; no per-icon vector drawing, no silent font-glyph breakage. User chose a "material-icons-crate-like" library; Phosphor is the reliable egui-0.29 choice (MIT). |
| Icon migration scope | **ALL app icons migrate to the library (MUST), via an audit** — including `library/icons.rs` stars/flags/chevrons and the `draw_reset_arrow` reset glyph; bespoke vector drawers removed | User: "the library is a MUST not a MAY — so we guarantee the icons render." One icon system, single source, nothing can silently break; the working vector icons consolidate onto it too (reset affordance stays load-bearing, only its glyph comes from the library). |
| Icon crate | **`egui-phosphor` (Regular)** | De-facto egui icon crate, tracks egui releases, MIT, covers every needed glyph. Material (`egui_material_icons`) offered but Phosphor is the safer version-compat bet. |
| Keybind defaults | **Mnemonic: A/C/M tools, `[`/`]` brush radius, `O` overlay (conflict-checked)**, all rebindable | Matches Photoshop/Lightroom muscle memory (`[`/`]` universal); mnemonic tool letters; rebindable so defaults aren't binding. |
| Brush-radius input | **Ctrl + mouse scroll over the canvas (gated to Mask+Brush, clamped, consumes the scroll so zoom doesn't also fire)** — NOT a keymap chord | User preference; scroll-to-resize is a natural brush gesture; the `Chord` keymap is key-based so this lives as a canvas gesture, not a rebindable action. |
| Component edit/remove UI | **A dedicated modal (`mask_components_modal.rs`), not inline in the panel**; panel gets a "Manage components" button; add-new stays in the panel | User: the 296px panel would be overloaded by an inline list; a modal keeps creation (panel) and management/edit/remove (modal) cleanly separated. |
| Overlay toggle | **Persistent `overlay_on` toggle (UI + keybind) AND keep the drag-time auto-hide** | User wants to see the real effect at rest (persistent off) without losing the existing drag-reveal. |
| Color picker | **Fix by relaxing the selection gate so the armed eyedropper runs without a selected mask (samples stage in `MaskUiState`); confirm via systematic-debugging** | Samples aren't tied to a mask until "Add Color range"; the gate is the likely no-op cause. |
| Component edit/remove mechanism | **Pure `remove_component` + `editing_component` state; delete any component; edit Luma/Color via the existing `set_component`** | `set_component` already exists; only `remove_component` + UI are missing; brush/gradient stay canvas-edited. (UI = the modal above.) |
| Brush performance | **Deferred to a separate brainstorm → spec** | User asked for it as a follow-up after these fixes; it needs its own diagnostics-first design. |
| Branch | **Continue on `feat/develop-tool-registry`** | These are visual-test-feedback fixes for that unmerged refactor; CLAUDE.md says address issues before finishing the branch. |
