# ferrolite — Develop UX round 2: mask Components window + live preview, color-picker fix, keybind discoverability, exponential brush (design)

> **Status:** Design — approved by user (2026-07-06); pending writing-plans.
> **Date:** 2026-07-06
> **Branch:** `feat/develop-tool-registry` (continues the visual-test-feedback work for the Develop
> tool-registry + UX-fixes; not a new branch).
> **Context / builds on:** the Develop tool-registry refactor + the round-1 UX fixes
> (`2026-07-06-develop-tool-registry-design.md`, `2026-07-06-develop-ux-fixes-design.md`) and their
> code (`ferrolite-app/src/develop/`: `mask_panel.rs`, `mask_overlay.rs`, `mask_components_modal.rs`,
> `mask_ui.rs`, `mask_edit.rs`, `tool_palette.rs`, `widgets/tool_button.rs`; `settings/keymap.rs`,
> `settings/ui/keyboard.rs`, `help.rs`, `theme.rs`, `icons.rs`, `app.rs`).
> **Proves:** a second round of author-hands-on feedback — the color eyedropper still doesn't work
> (root cause now found), component creation should move into one unified Components window with a
> live mask preview, keybinds should be discoverable (tooltips + settings/help), and brush sizing
> should feel smooth (exponential).

---

## 1. Goal & validation

Fix the remaining Develop masking friction from the author's second test pass, and make keybinds
discoverable:

> The color eyedropper actually samples (cursor + zoom loupe appear, click adds a swatch). Component
> creation + management live in ONE **Components window** (floating; the canvas stays live) with an
> **Add new component** section whose Luma/Color params drive a **live preview** of the resulting mask
> on the canvas before you commit; Brush/Linear/Radial are armed from the window and drawn on the
> canvas. Every keybound control shows its key in its **tooltip**, and every keybind/gesture is
> represented in **Settings and/or Help**. Ctrl+scroll brush sizing feels smooth (exponential — fine
> when small, coarser when large).

**Success = the running app** demonstrates each; automated gate green, then the author's hands-on
visual test (CLAUDE.md).

---

## 2. Scope

**In:**
- **Color-picker fix:** stop the full-canvas right-click-menu interact from stealing the eyedropper's
  click/hover.
- **Unified Components window:** evolve the round-1 `mask_components_modal` into one floating
  (non-blocking) window that both manages existing components (list + edit/delete) and **adds new
  ones** (all types); move the panel's add-flow into it.
- **Live preview mask:** while adding a Luma/Color component, the canvas overlay shows the prospective
  full mask (existing components + the tentative new one, at its composite mode).
- **Keybinds in tooltips** (+ CLAUDE.md Rule A).
- **Keybind discoverability in Settings + Help** (+ CLAUDE.md Rule B); audit + fill gaps incl. the
  Ctrl+scroll gesture.
- **Exponential brush sizing** for the Ctrl+scroll gesture.

**Out (non-goals / later):**
- **Brush-mask performance** → the separate follow-up (unchanged).
- No new adjustment ops, mask component types, or pipeline/OpStack/persistence changes.
- No re-theming beyond what these controls add.

---

## 3. Architecture of the slice

```
ferrolite-app/src/
  app.rs
    - loupe_ctx interact (~3882): register ONLY when active tool == Adjust (was: !crop_active)  [FIX]
    - Components window: render the floating window; feed the PROSPECTIVE mask to the overlay
      rebuild while an Add-Luma/Color preview is active; drop the modal_active() suppression      [MODIFY]
    - Ctrl+scroll gesture: call the (now exponential) brush_radius_from_scroll                    [MODIFY]
  develop/
    mask_components_modal.rs  -> the unified Components window (list+edit+delete + Add-new section) [MODIFY]
    mask_panel.rs             move the add-flow out; keep masks list + Light/Color + overlay toggle
                              + "Components" button                                                [MODIFY]
    mask_overlay.rs           eyedropper unchanged (fixed by the app.rs loupe_ctx gate); brush
                              radius helper -> exponential                                         [MODIFY]
    mask_ui.rs                add-section state (add_type, add_mode, preview flag) as needed        [MODIFY]
    mask_edit.rs              reuse add_component/set_component/remove_component (no new op)         [reuse]
  widgets/
    tool_button.rs / a tooltip helper: append the bound key to tooltips                            [MODIFY]
    keybind_hint helper (keymap chord -> display string)                                           [NEW small]
  settings/keymap.rs, settings/ui/keyboard.rs, help.rs   keybind discoverability + gesture entries  [MODIFY]
  CLAUDE.md                  Rule A (tooltip keybinds) + Rule B (settings/help representation)      [MODIFY]
```

No pipeline/OpStack/persistence/`EditOutcome`/`apply_edit` change. Reuses the overlay compositor for
the live preview and the existing pure `mask_edit` helpers for commits.

---

## 4. Color-picker fix (root cause found)

**Root cause (confirmed by reading `app.rs`):** after the active tool's `canvas()` runs (which, for
the Mask tool, registers the eyedropper's `ui.interact(image_rect, "mask_overlay_affordance",
Sense::click())`), `app.rs:3882-3900` registers a **full-canvas** `ui.interact(ui.min_rect(),
"loupe_ctx", Sense::click())` for the right-click image context menu, gated only on `!crop_active`.
Because it is registered LAST and covers the whole canvas, egui treats it as the top widget for the
pointer, so it **captures the primary click and hover**: the eyedropper's `resp.clicked()` never fires
(no sample) and `resp.hover_pos()` returns `None` (no loupe). The brush still works because it uses
`Sense::click_and_drag()` and the drag falls through to it (`loupe_ctx` senses only `click`). Crop was
already exempted for the same reason; the Mask tool was not.

**Fix:** register the `loupe_ctx` context-menu interact **only in the no-canvas-tool state** — i.e.
when `tool_state.active == ToolId::Adjust` (which subsumes the old `!crop_active` and also excludes
Mask/Heal). The right-click image menu remains available in the default Adjust view; while a canvas
tool (Crop/Mask) is active, the tool owns canvas input. No change to `route_color_eyedropper` itself —
it was always correct; it just never received the events. Verified by the author's hands-on test
(cursor + loupe appear, click samples).

---

## 5. Unified Components window

### 5.1 One window, non-blocking
The round-1 `mask_components_modal` (a blocking modal: list + edit/delete of existing components)
becomes **one floating `egui::Window`** ("Components — <mask name>") that is **non-blocking**: the
canvas stays visible and interactive behind/beside it. **The round-1 `modal_active()` extension for
`components_modal_open` is reverted** — this window must NOT suppress canvas input, because the live
preview, color sampling, and brush drawing all need the canvas live (it behaves like the tool palette,
not like Settings/Help). Opened by a **"Components"** button in the mask panel.

### 5.2 Two sections
1. **Existing components** (as round 1): a row per component — type label + composite mode + delete
   (`icons::DELETE` → `remove_component`) + edit for Luma/Color (`icons::EDIT` → inline params +
   Update via `set_component` / Cancel). Unchanged behavior, now inside the unified window.
2. **Add new component:** a type picker (Brush · Linear · Radial · Luma · Color) + a composite-mode
   selector (Add/Subtract/Intersect), then:
   - **Luma / Color:** param sliders (Luma: lo/hi/softness; Color: tolerance/softness + the sample
     swatches + a **Pick color** eyedropper toggle) and an **Add** button that commits via
     `mask_edit::add_component(stack, idx, component, mode)`. While this section is active it drives
     the **live preview** (§6). Requires a selected mask (guard + hint, as round 1).
   - **Brush / Linear / Radial:** an **Add** button that sets `mask.tool` to that type (and the
     composite mode into `mask.next_mode`) and closes/steps the window aside, so the user draws the
     component on the canvas via the existing overlay affordances (unchanged). No param preview (these
     are drawn interactively).

### 5.3 Panel slimming
`mask_panel::selected_section` **moves its component-creation UI into the window**: the sub-tool
picker, Add-Luma/Color buttons, brush/range param sliders, and the Pick-color button relocate to the
window's Add section. The panel keeps: the masks list (create/visibility/invert/rename/delete/select),
the per-mask **Light + Color adjustment** sliders (with per-control reset), the overlay on/off toggle,
and the new **"Components"** button (replacing the round-1 "Manage components" button). This declutters
the 296px panel, the author's stated motivation.

### 5.4 Eyedropper with the window open
Because the window is non-blocking and the loupe_ctx fix (§4) frees canvas input while the Mask tool is
active, arming **Pick color** in the window's Add-Color section sets `mask.tool = ColorRange` +
`mask.picking_color = true`, and clicking the canvas samples via the existing `route_color_eyedropper`
(cursor + loupe + swatch). Committing "Add" builds the ColorRange component from the collected samples.

---

## 6. Live preview mask

- While the window's **Add** section has a **Luma or Color** type selected with params, the canvas
  overlay renders the **prospective full mask**: the selected mask's *existing* components **plus** a
  *tentative* new component built from the Add-section params (via the pure `luma_from_state` /
  `color_from_state` helpers) folded in at the chosen composite mode.
- Implemented by feeding a **prospective `MaskDefinition`** to the existing overlay build
  (`rebuild_mask_overlay_if_needed` / the `MaskCompositor`) instead of the committed mask, whenever the
  Add-preview is active. The overlay's existing red-tint render then shows the live selection. On
  **Add** (commit), close the Add-preview and the overlay reflects the now-real mask; on window close /
  type change / cancel, revert to the committed mask's overlay.
- Reuses the overlay compositor + coverage→RGBA path; no new GPU work beyond an extra composite of the
  prospective def while previewing (bounded like the current overlay rebuild; still on the preview
  tier — brush-perf work stays out of scope). The prospective-def build is a small pure unit (existing
  components + tentative component) that is unit-tested.

---

## 7. Keybinds in control tooltips (CLAUDE.md Rule A)

- A small helper (e.g. `keymap.rs::hint(action) -> String` or a `keybind_hint` widget helper) formats
  an `Action`'s bound `Chord` into a display string (e.g. `"C"`, `"Ctrl+Z"`, `"T"`). Tooltips for
  keybound controls append it: palette tools show `"Crop (C)"` / `"Adjust (A)"` / `"Mask (M)"`, the
  overlay toggle shows `"Toggle mask overlay (T)"`, palette undo/redo show `"Undo (Ctrl+Z)"` /
  `"Redo (Ctrl+Shift+Z)"`, etc.
- `tool_button`'s `tooltip: &str` callers pass the key-augmented string (or a variant of `tool_button`
  takes an optional `Action` and appends the hint). The exact mechanism is a plan detail; the contract
  is: any control bound to a keybind shows that key in its hover tooltip, sourced from the live keymap
  (so a rebind updates the tooltip).
- **CLAUDE.md Rule A:** *A control bound to a keybind MUST display that key in its hover tooltip,
  sourced from the live keymap (`Keymap`), so rebinding updates it.*

---

## 8. Keybind discoverability in Settings + Help (CLAUDE.md Rule B)

- **Every keymap `Action`** already auto-lists in the Settings keyboard rebind grid (`GROUPS`) and
  should also appear in the Help cheat sheet — **audit** both and fill any gaps (esp. the round-1
  additions SwitchToolAdjust/Crop/Mask + ToggleMaskOverlay: confirm each is in a Settings `GROUPS`
  entry AND the Help table).
- **Non-rebindable input gestures** (the **Ctrl+scroll = brush size** gesture — not a `Chord`) get an
  explicit **Help** entry (a labeled row) and a noted line in the **Settings** keyboard tab (e.g. a
  read-only "Gestures" note) so they're discoverable even though not rebindable.
- **CLAUDE.md Rule B:** *Every keybind or input gesture MUST be represented in the Settings keyboard
  tab and/or the Help panel — a rebindable `Action` appears in both (Settings grid + Help); a
  non-rebindable gesture appears at least in Help (and is noted in Settings).*

---

## 9. Exponential brush sizing

- Change the Ctrl+scroll brush-radius gesture from **linear** (`radius += scroll·SENS`) to
  **multiplicative/exponential**: `radius = (radius * k.powf(scroll_ticks)).clamp(min, max)` where `k`
  is a per-tick growth factor slightly > 1 (tuned in the visual test). This makes each scroll tick a
  constant *percentage* change, so the brush is fine-grained at small radii and proportionally coarser
  at large radii — a smooth size ramp.
- Update the pure `brush_radius_from_scroll(current, scroll_y, min, max) -> f32` helper to the
  multiplicative form and update its test (scroll up grows multiplicatively, down shrinks, clamps; a
  small radius changes by a small absolute amount, a large radius by a larger absolute amount for the
  same scroll delta). Same clamp bounds (`BRUSH_RADIUS_MIN/MAX`).

---

## 10. Error handling / edge cases

- **Picker with a canvas tool active:** the loupe_ctx menu is simply not registered (no right-click
  image menu while Crop/Mask active) — matches the crop precedent; no panic.
- **Components window with no mask selected:** the Add section's commit ("Add" for Luma/Color) is
  guarded to require a selected mask (hint otherwise); sampling/preview degrade gracefully.
- **Live preview revert:** closing the window / changing the Add type / cancelling reverts the overlay
  to the committed mask; the tentative component is never written unless "Add" is pressed.
- **Non-blocking window + keybinds:** with `modal_active()` no longer suppressing for this window,
  Develop keybinds stay live while it's open (intended — it's a tool window). Confirm no keybind
  becomes destructive in that state (there is no mask-delete keybind).
- **Exponential clamp:** the multiplicative update always clamps to `[MIN, MAX]`; `scroll_y == 0` is a
  no-op; never produces 0 or negative radius.
- **Nothing slow on the UI thread (CLAUDE.md §1):** the live preview adds one bounded prospective
  composite while previewing (same tier/bounds as the existing overlay rebuild); brush-perf is the
  separate follow-up.

---

## 11. Testing

**Pure CPU logic (unit-tested):**
- `brush_radius_from_scroll` exponential: grow/shrink are multiplicative, clamp at bounds, small-radius
  delta < large-radius delta for the same scroll.
- The prospective-`MaskDefinition` builder (existing components + tentative Luma/Color at a mode) — a
  pure unit; reuses `luma_from_state`/`color_from_state`.
- `keymap` hint formatter: an `Action`'s `Chord` → the expected display string (plain key, Ctrl+, etc.).
- (Existing `remove_component`/`set_component`/round-trip tests remain.)

**egui rendering / interaction** (the Components window, live preview, tooltips, picker, settings/help
entries): `cargo build` + clippy + the author's hands-on visual test. No egui golden tests.

**Regression guard:** the round-1 masking + adjustments behavior is preserved (the add-flow moved, not
changed; commits still go through `add_component`/`apply_edit` as `OpKind::LocalAdjustments`;
per-control reset intact). The picker fix must not break the right-click image menu in the Adjust view
or crop/brush input.

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → then STOP and hold for the author's hands-on visual test (CLAUDE.md).

---

## 12. Decomposition into implementation plans

Single plan on `feat/develop-tool-registry`, in dependency order:
1. **Color-picker fix:** gate the `loupe_ctx` interact to `active == Adjust`. (Small; author verifies.)
2. **Exponential brush sizing:** update `brush_radius_from_scroll` + its test.
3. **Keybind hint helper + tooltips (Rule A):** the `hint` formatter (+ test) + augment keybound
   control tooltips; add the CLAUDE.md rule.
4. **Settings + Help keybind audit (Rule B):** ensure every Action is in Settings `GROUPS` + Help;
   add the Ctrl+scroll gesture entry; add the CLAUDE.md rule.
5. **Unified Components window:** merge add-flow into `mask_components_modal` (non-blocking; revert
   `modal_active` suppression); slim `mask_panel`; the Add section (all types) with commit/arm logic.
6. **Live preview mask:** prospective-def builder (+ test) + feed it to the overlay rebuild while an
   Add-Luma/Color preview is active; revert on commit/close/change.
7. Gate green + author visual test.

Steps 1-4 are independent + small; 5-6 are the feature core (5 before 6). The writing-plans step sets
final task granularity.

---

## 13. Decisions recorded (resolved during brainstorming, 2026-07-06)

| Question | Decision | Rationale |
|---|---|---|
| Color-picker root cause | **The full-canvas `loupe_ctx` right-click-menu interact (registered after the tool canvas, gated only `!crop_active`) steals the eyedropper's click+hover** | Found by reading `app.rs`; brush survives because it uses drag, eyedropper uses click; crop was already exempted. |
| Picker fix | **Register `loupe_ctx` only when `active == Adjust`** | Matches the crop precedent; the canvas belongs to the active tool; the right-click menu stays in the default view. |
| Add-component scope | **All component types in the window** (Luma/Color: params+preview+Add; Brush/Linear/Radial: arm the canvas tool) | User choice; unifies component creation; canvas-drawn types can't be param-previewed so they arm + draw. |
| Modal shape | **One unified Components window** (existing list + Add-new) | User choice; single place for all component management; declutters the panel. |
| Window blocking | **Non-blocking floating window; revert round-1 `modal_active()` suppression** | Live preview + color sampling + brush drawing need the canvas live; behaves like the tool palette. |
| Live preview | **Prospective full mask (existing + tentative new component at its mode)** | User choice; shows the real resulting selection, not just the new component in isolation. |
| Keybind tooltips | **Every keybound control shows its key in its tooltip, from the live keymap** (CLAUDE.md Rule A) | Discoverability; rebind-aware. |
| Keybind discoverability | **Every keybind/gesture represented in Settings and/or Help** (CLAUDE.md Rule B); gestures at least in Help | User ask; completes discoverability across surfaces. |
| Brush sizing | **Exponential (multiplicative per scroll tick)** | User ask; smooth feel — fine when small, coarser when large. |
| Brush performance | **Deferred to the separate follow-up** | Unchanged; its own diagnostics-first design. |
