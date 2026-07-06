# Develop UX Round 2 Implementation Plan — Components window + live preview, picker fix, keybind discoverability, exponential brush

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the color eyedropper, unify component create/manage into one non-blocking Components window with a live mask preview, make keybinds discoverable (tooltips + Settings/Help), and make Ctrl+scroll brush sizing exponential.

**Architecture:** The eyedropper fix is a one-line gate on the full-canvas right-click-menu interact. The Components window evolves the round-1 `mask_components_modal` into a non-blocking floating window that both lists/edits/deletes existing components and adds new ones (all types); the mask panel's add-flow moves into it. A live preview composites a *prospective* `MaskDefinition` (existing components + the tentative new one) through the existing overlay compositor while adding a Luma/Color component. Keybind hints come from the live keymap. No pipeline/OpStack/persistence changes.

**Tech Stack:** Rust, egui/eframe 0.29, the existing `ferrolite-app` Develop/masking UI + keymap + overlay compositor + `egui-phosphor` icon library.

## Global Constraints

- **No pipeline/OpStack/persistence change:** reuse `mask_edit::{add_component, set_component, remove_component}`, `EditOutcome{stack,kind,commit}`, `OpKind::LocalAdjustments`, `apply_edit`. Mask commits stay one-op-per-gesture.
- **Icons from the library (load-bearing, existing rule):** any new icon via `crate::icons::*` rendered in the icon font; no raw emoji/symbol glyphs, no new hand-drawn Painter icons.
- **Per-control reset preserved:** every migrated/relocated slider keeps its `EguiSlider` reset column.
- **Non-blocking Components window:** it must NOT suppress canvas input — revert the round-1 `modal_active()` extension for `components_modal_open`. The canvas stays live (live preview + color sampling + brush drawing need it).
- **CLAUDE.md Rule A (new):** a control bound to a keybind MUST show that key in its hover tooltip, sourced from the live `Keymap` (rebind-aware).
- **CLAUDE.md Rule B (new):** every keybind/input gesture MUST be represented in the Settings keyboard tab and/or the Help panel (rebindable action → both; non-rebindable gesture → at least Help + a Settings note).
- **Nothing slow on the UI thread (CLAUDE.md §1):** the live preview adds one bounded prospective composite (same tier/bounds as the current overlay rebuild). Brush-mask **performance is OUT of scope** (separate follow-up).
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean (use `--all-targets`); build/test `--offline` (deps already vendored); no `unwrap()` in non-test code.
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → STOP and hold for Jann's hands-on visual test.
- **Branch:** `feat/develop-tool-registry`.

---

## File Structure

- `ferrolite-app/src/app.rs` — loupe_ctx gate fix (T1); Ctrl+scroll uses the exponential helper (T2); render the Components window non-blocking + feed the prospective def to the overlay rebuild (T5/T6); drop the `modal_active` suppression for the window (T5).
- `ferrolite-app/src/develop/mask_overlay.rs` — `brush_radius_from_scroll` → exponential (T2).
- `ferrolite-app/src/settings/keymap.rs` — a `hint(action) -> String` formatter + test (T3).
- `ferrolite-app/src/develop/tool_palette.rs`, `mask_panel.rs` — keybind-augmented tooltips (T3).
- `ferrolite-app/src/settings/ui/keyboard.rs`, `help.rs` — keybind/gesture discoverability audit (T4).
- `ferrolite-app/src/develop/mask_components_modal.rs` — the unified Components window: existing-list (round 1) + Add-new section (T5); consumes the prospective-def preview signal (T6).
- `ferrolite-app/src/develop/mask_panel.rs` — move the add-flow out; keep masks list + Light/Color + overlay toggle + "Components" button (T5).
- `ferrolite-app/src/develop/mask_ui.rs` — add preview state (`preview_component: Option<(MaskComponent, CompositeMode)>`) (T6).
- `ferrolite-app/src/develop/mask_edit.rs` — reuse `luma_from_state`/`color_from_state` (round 1) + a pure `prospective_def` builder + test (T6). (If those `_from_state` helpers live in `mask_components_modal.rs`, move/reuse them; the plan notes it.)
- `CLAUDE.md` — Rule A (T3) + Rule B (T4).

No pipeline/OpStack/persistence files change.

---

## Task 1: Fix the color eyedropper (gate the right-click-menu interact to the Adjust tool)

**Files:** Modify `ferrolite-app/src/app.rs` (~3882-3900). Test: build + clippy + author visual test.

**Root cause (confirmed):** the full-canvas `ui.interact(ui.min_rect(), "loupe_ctx", Sense::click())` for the right-click image menu is registered AFTER the tool canvas and gated only on `!crop_active`, so during the Mask tool it sits on top of the eyedropper's click-interact and steals the click + hover. Fix: register it only in the no-canvas-tool (Adjust) state.

- [ ] **Step 1: Change the gate**

Current (app.rs ~3882-3887):
```rust
                        let ctx_menu_id = self
                            .state
                            .viewer
                            .as_ref()
                            .filter(|v| !v.crop_active)
                            .map(|v| v.image_id);
```
Replace the `.filter(...)` predicate so the context-menu interact is only registered when the active Develop tool is `Adjust` (no canvas tool owns the pointer):
```rust
                        let ctx_menu_id = self
                            .state
                            .viewer
                            .as_ref()
                            .filter(|v| v.tool_state.active == crate::develop::tool::ToolId::Adjust)
                            .map(|v| v.image_id);
```
(This subsumes the old `!crop_active` — Crop is a canvas tool so `active != Adjust` there too — and additionally excludes Mask/Heal, freeing canvas input for the mask eyedropper/affordances. Verify `crop_active` is still set elsewhere for the overlay/interactive gates; this only changes the context-menu registration.)

- [ ] **Step 2: Build + clippy**

Run: `cargo clippy -p ferrolite-app --all-targets --offline -- -D warnings` — Expected: clean.
Run: `cargo build -p ferrolite-app --offline` — Expected: OK.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/app.rs
git commit -m "fix(develop): register loupe context-menu only in the Adjust tool so the mask eyedropper receives clicks"
```

> **Author visual test:** Mask ▸ Color ▸ Pick color → cursor + zoom loupe follow the pointer, clicking adds a swatch; right-click image menu still works in the Adjust view; still suppressed while cropping.

---

## Task 2: Exponential brush sizing (Ctrl+scroll)

**Files:** Modify `ferrolite-app/src/develop/mask_overlay.rs` (the `brush_radius_from_scroll` helper + its test).

**Interfaces:** `brush_radius_from_scroll(current: f32, scroll_y: f32, min: f32, max: f32) -> f32` (signature unchanged; behavior → multiplicative). Consumed by the Ctrl+scroll gesture in `app.rs` (call site unchanged).

- [ ] **Step 1: Update the test to assert multiplicative behavior**

Replace the existing `scroll_tests` for `brush_radius_from_scroll` with:
```rust
#[cfg(test)]
mod scroll_tests {
    use super::*;
    #[test]
    fn scroll_is_multiplicative_and_clamped() {
        let (min, max) = (0.005f32, 0.5f32);
        // up grows, down shrinks
        assert!(brush_radius_from_scroll(0.1, 120.0, min, max) > 0.1, "up grows");
        assert!(brush_radius_from_scroll(0.1, -120.0, min, max) < 0.1, "down shrinks");
        // clamps
        assert_eq!(brush_radius_from_scroll(0.49, 100_000.0, min, max), max, "clamp hi");
        assert_eq!(brush_radius_from_scroll(0.01, -100_000.0, min, max), min, "clamp lo");
        // exponential: same scroll delta => larger ABSOLUTE change at a larger radius
        let small_delta = brush_radius_from_scroll(0.02, 120.0, min, max) - 0.02;
        let large_delta = brush_radius_from_scroll(0.20, 120.0, min, max) - 0.20;
        assert!(large_delta > small_delta, "bigger absolute step when larger (exponential feel)");
        // and the RATIO is ~constant (multiplicative)
        let r_small = brush_radius_from_scroll(0.02, 120.0, min, max) / 0.02;
        let r_large = brush_radius_from_scroll(0.20, 120.0, min, max) / 0.20;
        assert!((r_small - r_large).abs() < 1e-3, "constant multiplicative ratio");
    }
    #[test]
    fn zero_scroll_is_noop() {
        assert_eq!(brush_radius_from_scroll(0.1, 0.0, 0.005, 0.5), 0.1);
    }
}
```

Run: `cargo test -p ferrolite-app --lib brush_radius_from_scroll --offline` — Expected: FAIL (current linear impl breaks the ratio/absolute-step assertions).

- [ ] **Step 2: Implement the exponential form**

Replace the body of `brush_radius_from_scroll`:
```rust
/// New brush radius from a scroll delta, applied MULTIPLICATIVELY so each scroll
/// tick is a constant percentage change — fine-grained at small radii, coarser at
/// large radii (smooth size ramp). Clamped to [min, max]. `scroll_y == 0` is a no-op.
pub(crate) fn brush_radius_from_scroll(current: f32, scroll_y: f32, min: f32, max: f32) -> f32 {
    // Per-"tick" growth factor; egui scroll deltas are ~pixels, so scale down.
    const PER_UNIT: f32 = 0.0012; // ln-space rate; tuned in the visual test
    (current * (scroll_y * PER_UNIT).exp()).clamp(min, max)
}
```
(`x * e^(k·scroll)` is exact multiplicative growth; `PER_UNIT` sets sensitivity — tune in the visual test. Keep the `#[allow(dead_code)]` that's already on this bin-only helper.)

- [ ] **Step 3: Run the test**

Run: `cargo test -p ferrolite-app --lib brush_radius_from_scroll --offline` — Expected: PASS.
Run: `cargo clippy -p ferrolite-app --all-targets --offline -- -D warnings` — Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/develop/mask_overlay.rs
git commit -m "feat(develop): exponential (multiplicative) Ctrl+scroll brush sizing"
```

---

## Task 3: Keybind hints in tooltips (CLAUDE.md Rule A)

**Files:** Modify `ferrolite-app/src/settings/keymap.rs` (a `hint` formatter + test), `develop/tool_palette.rs` + `develop/mask_panel.rs` (augment tooltips), `CLAUDE.md`.

**Interfaces:** Produces `Keymap::hint(&self, action: Action) -> String` — the action's bound chord as a short display string (e.g. `"C"`, `"Ctrl+Z"`, `"Ctrl+Shift+Z"`, `"T"`). Consumed by tooltip call sites.

- [ ] **Step 1: Write the failing `hint` test**

Add to `keymap.rs` tests:
```rust
    #[test]
    fn hint_formats_chords() {
        let km = Keymap::defaults();
        assert_eq!(km.hint(Action::SwitchToolCrop), "C");
        assert_eq!(km.hint(Action::Undo), "Ctrl+Z");
        assert_eq!(km.hint(Action::Redo), "Ctrl+Shift+Z");
        assert_eq!(km.hint(Action::ToggleMaskOverlay), "T");
    }
```
Run: `cargo test -p ferrolite-app --lib keymap::tests::hint_formats_chords --offline` — Expected: FAIL (no `hint`).

- [ ] **Step 2: Implement `hint`**

Add to `impl Keymap`:
```rust
    /// The bound chord for `action` as a short display string (e.g. "Ctrl+Z", "C").
    /// Modifier order: Ctrl, Shift, Alt, then the key. Used for tooltip hints so a
    /// rebind updates the shown key (CLAUDE.md "UI keybind tooltips" rule).
    pub fn hint(&self, action: Action) -> String {
        let c = self.chord(action);
        let mut s = String::new();
        if c.ctrl { s.push_str("Ctrl+"); }
        if c.shift { s.push_str("Shift+"); }
        if c.alt { s.push_str("Alt+"); }
        s.push_str(key_label(c.key));
        s
    }
```
Add a `fn key_label(key: Key) -> &'static str` (or reuse an existing key-name function if `keymap.rs` already has one for the rebind UI — check `settings/ui/keyboard.rs`/`keymap.rs` for a key-display fn and reuse it rather than duplicating). It maps `Key::A => "A"`, `Key::Z => "Z"`, `Key::Comma => ","`, `Key::ArrowLeft => "←"`, etc. — the same labels the rebind UI shows. If such a fn exists, `hint` calls it; only add `key_label` if none exists.

Run: `cargo test -p ferrolite-app --lib keymap::tests::hint_formats_chords --offline` — Expected: PASS.

- [ ] **Step 3: Augment keybound tooltips**

In `develop/tool_palette.rs`, the palette renders each tool via `tool_button(ui, tool.icon(), tool.label(), ...)` and undo/redo via `tool_button(ui, icons::UNDO, "Undo", ...)` etc. Map each keybound control to its `Action` and append the hint to the tooltip. The tool→action map:
```rust
    fn tool_action(id: crate::develop::tool::ToolId) -> Option<crate::settings::keymap::Action> {
        use crate::develop::tool::ToolId;
        use crate::settings::keymap::Action;
        match id {
            ToolId::Adjust => Some(Action::SwitchToolAdjust),
            ToolId::Crop => Some(Action::SwitchToolCrop),
            ToolId::Mask => Some(Action::SwitchToolMask),
            ToolId::Heal => None,
        }
    }
```
`tool_palette::show` receives the registry + ctx (`DevelopCtx { state }`), so it can read `ctx.state.settings.keymap`. Build the tooltip as `format!("{} ({})", tool.label(), km.hint(action))` when an action exists, else `tool.label().to_string()`. For undo/redo pass `format!("Undo ({})", km.hint(Action::Undo))` / `format!("Redo ({})", km.hint(Action::Redo))`. (The keymap is reachable via the `DevelopCtx.state.settings.keymap` already passed to `tool_palette::show`.)

In `develop/mask_panel.rs`, the overlay toggle button's tooltip becomes `format!("{} ({})", tip, km.hint(Action::ToggleMaskOverlay))`. `mask_panel::show`/`selected_section` don't currently receive the keymap — thread `&Keymap` (or the whole `&AppState`) into the overlay-toggle tooltip; the plan's implementer picks the minimal threading (e.g. pass `keymap: &Keymap` to `show`). If threading is heavy, an acceptable alternative is to read the default hint via a helper — but prefer the live keymap per Rule A; note the choice.

- [ ] **Step 4: Add CLAUDE.md Rule A**

Append to `CLAUDE.md` (near the UI icons rule):
```markdown
## UI keybind tooltips (load-bearing)

Any control bound to a keybind MUST display that key in its hover tooltip, sourced from
the live keymap (`Keymap::hint(action)`), so rebinding updates the shown key. Format the
label as `"<Label> (<Key>)"` (e.g. "Crop (C)", "Undo (Ctrl+Z)"). Non-rebindable input
gestures are documented in Help/Settings instead (see "Keybind discoverability").
```

- [ ] **Step 5: Gate + commit**

Run: `cargo test -p ferrolite-app --lib keymap --offline` + `cargo clippy -p ferrolite-app --all-targets --offline -- -D warnings` — Expected: pass + clean.
```bash
git add ferrolite-app/src/settings/keymap.rs ferrolite-app/src/develop/tool_palette.rs ferrolite-app/src/develop/mask_panel.rs CLAUDE.md
git commit -m "feat(develop): keybind hints in control tooltips (from live keymap) + CLAUDE.md rule"
```

---

## Task 4: Keybind discoverability in Settings + Help (CLAUDE.md Rule B)

**Files:** Modify `ferrolite-app/src/settings/ui/keyboard.rs`, `ferrolite-app/src/help.rs`, `CLAUDE.md`. Test: build + clippy + a coverage assertion.

- [ ] **Step 1: Audit that every Action is in the Settings `GROUPS` + Help**

Read `settings/ui/keyboard.rs`'s `GROUPS` and `help.rs`'s shortcut table. Confirm the round-1 additions (`SwitchToolAdjust`, `SwitchToolCrop`, `SwitchToolMask`, `ToggleMaskOverlay`) are each present in a Settings `GROUPS` entry AND the Help table; add any missing. Add a test in `keyboard.rs` that every `Action::ALL` variant appears in exactly one `GROUPS` entry (so a future new action can't silently be undiscoverable):
```rust
    #[test]
    fn every_action_is_in_a_settings_group() {
        use crate::settings::keymap::Action;
        for a in Action::ALL {
            let count = GROUPS.iter().filter(|(_, acts)| acts.contains(&a)).count();
            assert_eq!(count, 1, "{a:?} must be in exactly one Settings group");
        }
    }
```
Run it: `cargo test -p ferrolite-app --lib every_action_is_in_a_settings_group --offline` — fix `GROUPS` until it passes (this is the RED→GREEN for the audit).

- [ ] **Step 2: Add the non-rebindable Ctrl+scroll gesture to Help + a Settings note**

In `help.rs`'s shortcut table, add a row: **"Ctrl + scroll — Brush size (Mask ▸ Brush)"** under the Develop/masking group (if not already present from round 1, ensure it is). In `settings/ui/keyboard.rs`, add a small read-only line below the rebind grid noting non-rebindable gestures, e.g.:
```rust
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Gestures: Ctrl + scroll over the image resizes the brush (Mask ▸ Brush).")
            .size(11.0)
            .color(crate::theme::TEXT_DIM),
    );
```
(Match the surrounding style; place it after the groups grid.)

- [ ] **Step 3: Add CLAUDE.md Rule B**

Append to `CLAUDE.md`:
```markdown
## Keybind discoverability (load-bearing)

Every keybind or input gesture MUST be represented so the user can discover it: a
rebindable `Action` appears in BOTH the Settings keyboard tab (add it to a `GROUPS`
entry — enforced by `every_action_is_in_a_settings_group`) AND the Help panel's shortcut
list. A non-rebindable input gesture (e.g. Ctrl+scroll = brush size) appears at least in
the Help panel and is noted in the Settings keyboard tab's gestures line.
```

- [ ] **Step 4: Gate + commit**

Run: `cargo test -p ferrolite-app --lib --offline` + `cargo clippy -p ferrolite-app --all-targets --offline -- -D warnings` — pass + clean.
```bash
git add ferrolite-app/src/settings/ui/keyboard.rs ferrolite-app/src/help.rs CLAUDE.md
git commit -m "feat(develop): keybind/gesture discoverability in Settings + Help + coverage test + CLAUDE.md rule"
```

---

## Task 5: Unified Components window (merge add-flow; non-blocking; slim the panel)

**Files:** Modify `ferrolite-app/src/develop/mask_components_modal.rs` (the window: existing list + Add-new section), `mask_panel.rs` (move add-flow out; keep list + Light/Color + overlay toggle + "Components" button), `app.rs` (drop the `modal_active` suppression for the window; window already rendered from round 1). Test: build + clippy + author visual test.

**Interfaces:** `mask_components_modal::show(ctx, stack, &mut mask) -> Option<EditOutcome>` (signature unchanged; body gains the Add-new section). Reuses `MaskUiState` add fields (`tool`, `next_mode`, `range_lo/hi/softness`, `color_tolerance/softness/samples`, `brush_*`, `picking_color`) + `mask_edit::add_component`.

- [ ] **Step 1: Make the window non-blocking (revert round-1 modal suppression)**

In `app.rs`, remove `components_modal_open` from `modal_active()` (added in round 1) so the window doesn't suppress canvas/keybind input — the canvas must stay live for the live preview, color sampling, and brush drawing. (Find the `modal_active()` fn; drop the `|| ...components_modal_open` arm.) Confirm the window is still rendered each frame (round-1 wiring) and its `EditOutcome` still routes to `apply_edit`.

- [ ] **Step 2: Add the "Add new component" section to the window**

In `mask_components_modal::show`, below the existing-components list, add an **"Add new component"** section (read the current file first to match its layout/helpers):
- A type picker row (Brush / Linear / Radial / Luma / Color) using `tool_button` with the icon-library glyphs (`icons::BRUSH/LINEAR_GRADIENT/RADIAL_GRADIENT/LUMA/COLOR`), setting `mask.tool` on click (this is the relocated sub-tool strip).
- A composite-mode selector (Add/Subtract/Intersect) setting `mask.next_mode`.
- Then, by `mask.tool`:
  - **`LumaRange`:** the lo/hi/softness `EguiSlider`s (from the panel), + an **"Add Luma range"** button → `out = Some(commit(mask_edit::add_component(stack, idx, MaskComponent::LumaRange { lo: mask.range_lo, hi: mask.range_hi, softness: mask.range_softness }, mask.next_mode)))`.
  - **`ColorRange`:** the tolerance/softness `EguiSlider`s + sample swatches + the **"Pick color"** toggle (`mask.picking_color`) + **"Add Color range"** button (guarded to require selection; clears samples + `picking_color` on add) — exactly the panel's current Color block, relocated.
  - **`Brush` / `Linear` / `Radial`:** the brush param sliders (radius/hardness/flow/erase) for Brush; then a short hint ("Draw on the image to add this component") — selecting the type already sets `mask.tool`, so the existing canvas affordances create it. No Add button needed (drawn on canvas); optionally a "close window to draw" affordance.
- Move these blocks OUT of `mask_panel::selected_section` (Step 3). Reuse the exact slider params + commit logic from the panel (don't rewrite the edit logic).

- [ ] **Step 3: Slim the mask panel**

In `mask_panel::selected_section`, REMOVE the relocated add-flow (sub-tool picker, brush param sliders, Add-Luma/Color blocks, Pick-color) — they now live in the window. KEEP: the per-mask **Light + Color adjustment** sliders (with per-control reset), the component count, and the **"Components"** button (rename the round-1 "Manage components" button to "Components"; it still sets `mask.components_modal_open = true`). Keep the overlay on/off toggle in `mask_panel::show`. The masks list (create/visibility/invert/rename/delete/select) is unchanged.

- [ ] **Step 4: Build + clippy**

Run: `cargo clippy -p ferrolite-app --all-targets --offline -- -D warnings` — clean.
Run: `cargo build -p ferrolite-app --offline` — OK. Run `cargo test -p ferrolite-app --offline` — pass (existing tests unaffected).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/develop/mask_components_modal.rs ferrolite-app/src/develop/mask_panel.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): unified Components window (list+edit+delete + add-new all types), non-blocking; slim mask panel"
```

> **Author visual test:** the "Components" button opens the window; the canvas stays fully usable behind it; adding Luma/Color from the window commits a component; picking Brush/Linear/Radial lets you draw it on the canvas; the panel is decluttered (masks list + Light/Color + overlay toggle + Components button).

---

## Task 6: Live preview mask (prospective full mask while adding Luma/Color)

**Files:** Modify `ferrolite-app/src/develop/mask_ui.rs` (`preview_component` state), `mask_edit.rs` (pure `prospective_def` + test; ensure `luma_from_state`/`color_from_state` are reachable), `mask_components_modal.rs` (set the preview from the Add-Luma/Color params), `app.rs` (feed the prospective def to the overlay rebuild while previewing). Test: pure `prospective_def` test + build/clippy + author visual test.

**Interfaces:** Produces `mask_edit::prospective_def(base: &MaskDefinition, tentative: MaskComponent, mode: CompositeMode) -> MaskDefinition` (pure). `MaskUiState.preview_component: Option<(MaskComponent, CompositeMode)>` (the tentative add being previewed; `None` = no preview).

- [ ] **Step 1: Pure `prospective_def` builder + test**

In `mask_edit.rs`:
```rust
/// The mask definition AS IT WOULD BE with `tentative` folded in at `mode` after the
/// existing `base` components — used to preview an in-progress "add component".
pub fn prospective_def(
    base: &ferrolite_mask::MaskDefinition,
    tentative: ferrolite_mask::MaskComponent,
    mode: ferrolite_mask::CompositeMode,
) -> ferrolite_mask::MaskDefinition {
    let mut def = base.clone();
    def.components.push((tentative, mode));
    def
}
```
Test:
```rust
    #[test]
    fn prospective_def_appends_tentative() {
        use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition};
        let base = MaskDefinition {
            components: vec![(MaskComponent::LumaRange { lo: 0.0, hi: 1.0, softness: 0.0 }, CompositeMode::Add)],
            invert: false,
        };
        let t = MaskComponent::LumaRange { lo: 0.2, hi: 0.7, softness: 0.1 };
        let out = prospective_def(&base, t.clone(), CompositeMode::Subtract);
        assert_eq!(out.components.len(), 2);
        assert_eq!(out.components[1], (t, CompositeMode::Subtract));
        assert_eq!(out.components[0], base.components[0], "base preserved");
    }
```
Run: `cargo test -p ferrolite-app --lib prospective_def --offline` — RED then GREEN.

- [ ] **Step 2: Add the preview state**

In `mask_ui.rs`, add to `MaskUiState`:
```rust
    /// While the Components window's Add section is tuning a Luma/Color component,
    /// this holds the tentative (component, mode) so the canvas overlay previews the
    /// prospective full mask. `None` = no add-preview. Reset on add/close/type change.
    pub preview_component: Option<(ferrolite_mask::MaskComponent, ferrolite_mask::CompositeMode)>,
```
Default `None`. Reset it (to `None`) wherever `components_modal_open`/`editing_component` are reset (mask switch, undo/redo, tool deselect, window close) — mirror those sites.

- [ ] **Step 3: Set the preview from the window's Add-Luma/Color section**

In `mask_components_modal::show`, when the Add type is `LumaRange` or `ColorRange`, build the tentative component from the current `mask.*` params (reuse `luma_from_state(mask)` / `color_from_state(mask)` — the round-1 pure helpers; if they're private to this module, keep using them here) and set `mask.preview_component = Some((tentative, mask.next_mode))` each frame the section is shown. Clear it (`None`) when the Add type is Brush/Linear/Radial, on "Add" (after commit), and when the window closes. (For Color with zero samples, the tentative ColorRange has empty samples → preview shows the base mask unchanged, which is fine.)

- [ ] **Step 4: Feed the prospective def to the overlay rebuild**

In `app.rs`, where `rebuild_mask_overlay_if_needed` composites the selected mask's def for the red overlay: when `mask.preview_component` is `Some((c, mode))` and a mask is selected, composite `mask_edit::prospective_def(&selected_mask_def, c, mode)` instead of the committed def. The overlay cache key (`overlay_key`) MUST incorporate the `preview_component` (hash it in) so the overlay rebuilds live as the Add sliders move. Read `rebuild_mask_overlay_if_needed` (app.rs ~1505) first and thread the override minimally (e.g. compute the def-to-composite = prospective if previewing else committed, and fold `preview_component` into the cache key). On commit/close (`preview_component` → `None`), the key changes back and the overlay reflects the committed mask.

- [ ] **Step 5: Build + clippy + tests**

Run: `cargo test -p ferrolite-app --lib --offline` (incl. `prospective_def`) — pass.
Run: `cargo clippy -p ferrolite-app --all-targets --offline -- -D warnings` — clean. `cargo build --offline` OK.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/develop/mask_ui.rs ferrolite-app/src/develop/mask_edit.rs ferrolite-app/src/develop/mask_components_modal.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): live prospective-mask preview while adding a Luma/Color component"
```

> **Author visual test:** open Components → Add ▸ Luma (or Color), drag the params → the red overlay on the canvas updates live to show the prospective selection (existing + the new component at its mode); "Add" commits it (overlay now reflects the real mask); closing/cancelling reverts the overlay.

---

## Task 7: Workspace gate + author visual-test hand-off

**Files:** none (verification only).

- [ ] **Step 1: Remove any remaining plan-added scaffolding allows** (grep touched files; the bin-only `brush_radius_from_scroll` allow stays, documented). Confirm clippy clean.

- [ ] **Step 2: Full gate**

Run: `cargo fmt --all --check` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo test --workspace`
Expected: all green. (Note: `state::tests::cancel_pending_jobs_drains_thumb_handles` is a PRE-EXISTING flaky concurrency test unrelated to this work — if it fails under full-suite parallelism, re-run it in isolation to confirm it passes; not a regression.)

- [ ] **Step 3: STOP — hand over the visual test plan**

Checklist for the author:
1. **Color picker works:** Mask ▸ Color ▸ Pick color → cursor + zoom loupe follow the pointer; click adds a swatch. Right-click image menu still works in the Adjust view; still suppressed while cropping.
2. **Components window:** the "Components" button opens a floating window; the canvas stays fully usable behind it. Existing components list edits/deletes as before. Add ▸ Luma/Color commits; Add ▸ Brush/Linear/Radial lets you draw on the canvas. The mask panel is decluttered.
3. **Live preview:** while adding Luma/Color, dragging params updates the red overlay live to the prospective selection; Add commits it; close/cancel reverts.
4. **Keybind tooltips:** hovering the palette tools shows "Adjust (A)/Crop (C)/Mask (M)", undo/redo show their chords, the overlay toggle shows "(T)"; a rebind in Settings updates the tooltip.
5. **Discoverability:** every keybind is in Settings ▸ Keyboard and the Help panel; the Ctrl+scroll brush gesture is listed in Help + noted in Settings.
6. **Exponential brush:** Ctrl+scroll over the image resizes smoothly — fine steps when small, larger steps when big; clamped; normal scroll still zooms.

Address findings, then finish per CLAUDE.md (do not merge/PR on your own).

---

## Self-Review

**1. Spec coverage:** §4 picker fix → T1; §9 exponential brush → T2; §7 tooltips + Rule A → T3; §8 Settings/Help + Rule B → T4; §5 unified Components window (non-blocking, add-flow moved, panel slimmed) → T5; §6 live preview → T6; gate + hold → T7. All spec sections mapped.

**2. Placeholder scan:** The `PER_UNIT`/`k` growth factor and the exact tooltip-threading + overlay-rebuild-override are flagged "tune in visual test" / "read the fn first + thread minimally" with the concrete mechanism (multiplicative formula; `preview_component` state + prospective_def + cache-key fold) given — appropriate for egui wiring against existing code, not hand-waving. Pure helpers (`brush_radius_from_scroll`, `prospective_def`, `hint`) have complete code + tests.

**3. Type consistency:** `brush_radius_from_scroll(f32,f32,f32,f32)->f32` unchanged; `Keymap::hint(Action)->String`; `prospective_def(&MaskDefinition, MaskComponent, CompositeMode)->MaskDefinition`; `MaskUiState.preview_component: Option<(MaskComponent, CompositeMode)>` used consistently in T6; `mask_edit::add_component`/`luma_from_state`/`color_from_state` reused as named; `ToolId::Adjust` gate in T1. `EditOutcome`/`OpKind::LocalAdjustments` reused.

**Open items the implementer confirms against live code (flagged in-task):** the exact `modal_active()` arm to drop; whether a key-label fn already exists in keymap/keyboard.rs (reuse vs add `key_label`); the `rebuild_mask_overlay_if_needed` structure for the prospective-def override + cache key; whether `luma_from_state`/`color_from_state` are module-private (reuse in place).
