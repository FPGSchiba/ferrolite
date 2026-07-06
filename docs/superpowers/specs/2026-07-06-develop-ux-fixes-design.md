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
  aliases** over the crate's constants; render icons in the crate's font family everywhere (starting
  with `widgets::tool_button` + the mask sub-tool strip + palette undo/redo). Replace all broken emoji
  glyphs. Add a **CLAUDE.md rule** codifying the icon convention.
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
- Not migrating every existing glyph in the app to the icon library in this pass — only the Develop
  tool-registry icons that are broken + any new icons this spec adds (rating/flag icons in
  `library/icons.rs` already work via vector drawing and are left as-is; they MAY move to the library
  later, out of scope here).

---

## 3. Architecture of the slice

```
ferrolite-app
  Cargo.toml            + egui-phosphor (version matching egui 0.29)                    [NEW dep]
  src/theme.rs          install_fonts(): egui_phosphor::add_to_fonts(&mut fonts, ...)   [MODIFY]
  src/icons.rs          semantic aliases over egui_phosphor::regular::* (CROP, MASK,…)  [NEW]
  src/widgets/tool_button.rs   render icon in the phosphor font family                  [MODIFY]
  src/develop/
    tools/{adjust,crop,mask,heal}.rs   icon() -> icons::* constant                      [MODIFY]
    tool_palette.rs     undo/redo icons -> icons::UNDO / icons::REDO                     [MODIFY]
    mask_panel.rs        sub-tool strip icons -> icons::*; overlay toggle; per-component
                         list (delete/edit); "Pick color" edit-mode wiring              [MODIFY]
    mask_overlay.rs      color-eyedropper routing fix (relax selection gate)            [MODIFY]
    mask_ui.rs           + editing_component: Option<usize>                             [MODIFY]
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

### 4.5 CLAUDE.md rule (new, added by this work)
> **UI icons (load-bearing).** All Develop/UI icons come from the `icons` module
> (`ferrolite-app/src/icons.rs`), which aliases the bundled icon font (`egui-phosphor`, installed in
> `theme::install_fonts`). Render them in the icon font family (via `tool_button` or `FontId` with that
> family). NEVER put raw emoji/symbol characters in IBM Plex text — Plex and egui's bundled emoji subset
> don't cover them and they render as tofu. Add a new icon by adding a semantic alias in `icons.rs`
> (sourced from the crate's catalog). Pre-existing vector icons (`library/icons.rs`,
> `draw_reset_arrow`) remain valid for bespoke shapes.

---

## 5. Keybinds

Extend the existing keymap (`settings/keymap.rs`): the pattern is add an `Action` variant + append to
`Action::ALL` + a `label()` arm + a default `Chord` in `defaults()`; dispatch is a `keymap.pressed()`/
`held()` check in `app.rs`; the Settings rebind UI auto-lists actions from a `GROUPS` table; persistence
is automatic (`Keymap` is `#[serde(default)]` in `Settings`).

### 5.1 New actions + default chords
| Action | Default | Trigger | Effect |
|---|---|---|---|
| `SwitchToolAdjust` | `A` | `pressed` | select `ToolId::Adjust` |
| `SwitchToolCrop` | `C` | `pressed` | select `ToolId::Crop` |
| `SwitchToolMask` | `M` | `pressed` | select `ToolId::Mask` |
| `BrushRadiusDecrease` | `[` (`OpenBracket`) | `held` | brush radius −= rate·dt, clamped |
| `BrushRadiusIncrease` | `]` (`CloseBracket`) | `held` | brush radius += rate·dt, clamped |
| `ToggleMaskOverlay` | `O` | `pressed` | flip `mask.overlay_on` |

- **Conflict handling:** `O` is `FlagReject`'s default. The plan runs `Keymap::conflict()` for each new
  default; if a default collides with an action reachable in the same (Develop) context, pick a free key
  (documented in the plan). All are rebindable, so a collision is not fatal — but defaults must not
  shadow an existing Develop action.
- **Module gating:** all six are handled only inside the existing `self.module == Module::Develop &&
  self.state.viewer.is_some()` block in `app.rs` (mirroring NextImage/PrevImage), so they never fire in
  Library/Export. Brush-radius keys additionally only act when the Mask tool + Brush sub-tool are the
  active context (else no-op) — the plan decides the exact guard; at minimum they mutate
  `v.mask.brush_radius` clamped to the panel slider's range.
- **Tool switch** reuses the palette's exact enable-check + `ToolState::select_tool(id, enabled, reg)`.
- **Held-key repeat** uses `Keymap::held()` (egui `key_down`) with a per-frame delta, the pattern
  `HoldBeforePeek` already establishes for held keys.

### 5.2 UI surfaces
- Add the new actions to the `"Develop"` entry of `settings/ui/keyboard.rs`'s `GROUPS` (auto-appears in
  the rebind grid with a reset arrow).
- Add them to `help.rs`'s cheat-sheet table for discoverability.

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
Add `MaskUiState.editing_component: Option<usize>` (default `None`) — which component index the
Luma/Color sliders are currently editing (vs. adding a new one). Reset on mask switch/undo/redo (mirror
the existing transient-state resets).

### 8.3 Panel UI (`mask_panel::selected_section`)
- Render a **per-component list**: loop `layer.mask.components.iter().enumerate()`, each row showing a
  short type label (Brush / Linear / Radial / Luma / Color / Imported) + its `CompositeMode`, a
  **delete** button (`icons::DELETE`) for any component (→ `remove_component`, `commit`), and — for
  **Luma/Color** — an **edit** button (`icons::EDIT`) that sets `editing_component = Some(i)` and loads
  the component's params back into `mask.range_lo/hi/softness` (Luma) or `mask.color_samples/tolerance/
  softness` (Color).
- When `editing_component == Some(i)`, the Luma/Color param block's action button reads **"Update"** and
  calls `mask_edit::set_component(stack, idx, i, new_component)` instead of `add_component`; committing
  clears `editing_component`. When `None`, it stays **"Add Luma range"/"Add Color range"** →
  `add_component` (today's behavior).
- Deleting the component currently being edited clears `editing_component`.
- Brush/gradient components remain edited on the canvas (handles/strokes); they appear in the list with
  a delete button but no panel edit button (their edit is spatial).

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
- **editing_component stale** (component removed or mask switched) → cleared; the action button reverts
  to "Add"; never edits a wrong/absent index.
- **Keybind conflicts** → resolved at default-assignment time via `Keymap::conflict()`; user rebinds
  freely; brush-radius held keys clamp to the slider range (no runaway values).
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
1. **Icon library:** add `egui-phosphor` (pin egui-0.29-compatible version), install in `theme`, create
   `icons.rs` aliases, switch `tool_button` to the icon font, replace all tool/sub-tool/undo-redo icon
   literals, add the CLAUDE.md rule. Verify icons render (build + visual).
2. **Keybinds:** new `Action`s + `ALL` + `label()` + defaults (+ conflict resolution), Develop-gated
   dispatch (tool switch / overlay toggle / brush-radius held), rebind `GROUPS` + help table.
3. **Mask overlay toggle:** UI toggle in the mask panel wired to `overlay_on` (keybind from step 2).
4. **Color-picker fix:** systematic-debugging → relax the eyedropper's selection gate + ensure source;
   verify loupe + sampling.
5. **Component edit/remove:** `remove_component` (pure + tests) + `editing_component` state + the
   per-component list UI (delete/edit; Add↔Update).
6. Gate green + author visual test.

If the icon step's font-family/rendering details prove fiddly, it may split from the icon call-site
swaps; the writing-plans step decides final task granularity.

---

## 12. Decisions recorded (resolved during brainstorming, 2026-07-06)

| Question | Decision | Rationale |
|---|---|---|
| Icon approach | **Bundle an icon-font crate (`egui-phosphor`) + a thin `icons` alias module + a CLAUDE.md rule** | A reusable icon *library* is a permanent fix — future UI sources any glyph from the catalog or adds a one-line alias; no per-icon vector drawing, no silent font-glyph breakage. User chose a "material-icons-crate-like" library; Phosphor is the reliable egui-0.29 choice (MIT). |
| Icon crate | **`egui-phosphor` (Regular)** | De-facto egui icon crate, tracks egui releases, MIT, covers every needed glyph. Material (`egui_material_icons`) offered but Phosphor is the safer version-compat bet. |
| Keybind defaults | **Mnemonic: A/C/M tools, `[`/`]` brush radius, `O` overlay (conflict-checked)**, all rebindable | Matches Photoshop/Lightroom muscle memory (`[`/`]` universal); mnemonic tool letters; rebindable so defaults aren't binding. |
| Brush-radius repeat | **`held()` key with per-frame delta, clamped** | egui `key_down` gives smooth repeat; reuses the `HoldBeforePeek` held-key pattern. |
| Overlay toggle | **Persistent `overlay_on` toggle (UI + keybind) AND keep the drag-time auto-hide** | User wants to see the real effect at rest (persistent off) without losing the existing drag-reveal. |
| Color picker | **Fix by relaxing the selection gate so the armed eyedropper runs without a selected mask (samples stage in `MaskUiState`); confirm via systematic-debugging** | Samples aren't tied to a mask until "Add Color range"; the gate is the likely no-op cause. |
| Component edit/remove | **Pure `remove_component` + `editing_component` state + per-component list (delete any; edit Luma/Color via `set_component`)** | `set_component` already exists; only remove + UI are missing; brush/gradient stay canvas-edited. |
| Brush performance | **Deferred to a separate brainstorm → spec** | User asked for it as a follow-up after these fixes; it needs its own diagnostics-first design. |
| Branch | **Continue on `feat/develop-tool-registry`** | These are visual-test-feedback fixes for that unmerged refactor; CLAUDE.md says address issues before finishing the branch. |
