# Develop UX Fixes — Icon Library, Keybinds, Overlay Toggle, Picker Fix, Component Modal — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the issues found testing the Develop tool-registry refactor — put all icons on a durable icon-font library (`egui-phosphor`) so nothing renders as tofu, add tool/overlay keybinds + a Ctrl+scroll brush-size gesture, make the mask red-overlay toggleable, fix the broken color-range eyedropper, and add a modal to edit/remove individual mask components.

**Architecture:** Add the `egui-phosphor` crate; install its font once in `theme::install_fonts`; expose a thin `ferrolite-app::icons` module of semantic aliases over the crate's constants; migrate **every** icon/glyph in the app (the broken Develop icons, `library/icons.rs` stars/flags/chevrons, and the `draw_reset_arrow` reset glyph) to render from that font — one guaranteed icon system. Extend the existing `Action`/`Keymap` system with Develop-gated tool-switch + overlay-toggle actions; add Ctrl+scroll brush sizing as a gated canvas gesture (not a keymap chord). Add a `mask.overlay_on` toggle. Relax the color-eyedropper's routing gate so it works without a pre-selected mask. Add a pure `mask_edit::remove_component` + a dedicated `mask_components_modal.rs` for editing/removing components. No pipeline/OpStack/persistence/`EditOutcome`/`apply_edit` changes.

**Tech Stack:** Rust, egui/eframe **0.29**, `egui-phosphor`, the existing `ferrolite-app` Develop/masking UI + keymap + theme.

## Global Constraints

- **Icons are load-bearing (new CLAUDE.md rule, added in Task 1):** EVERY icon comes from the `icons` module (aliasing `egui-phosphor`, installed in `theme::install_fonts`), rendered in the icon font family. NEVER raw emoji/symbol chars in IBM Plex text; do NOT hand-draw new `Painter` icons. The per-control reset affordance + its placement stay load-bearing — only its glyph comes from the library.
- **No behavior loss / no pipeline change:** reuse `EditOutcome`/`OpKind`/`apply_edit`/`OpStack` verbatim; migrations preserve edit logic, per-control reset (`EguiSlider` reset column), overlays, and rating/flag visual semantics (filled vs outline). No new ops, mask component types, persistence, or render-pipeline changes. Brush **performance** is explicitly out of scope (separate follow-up).
- **egui 0.29:** the `egui-phosphor` version MUST match egui 0.29 (Task 1 pins it). Do not bump egui.
- **Dependency fetch:** adding `egui-phosphor` triggers a crates.io fetch. In this environment that may fail schannel TLS revocation; the fix is a one-time **author-authorized** `CARGO_HTTP_CHECK_REVOKE=false` fetch, then offline build (see repo memory). An implementer that hits a fetch failure reports **BLOCKED**; the controller runs the authorized fetch and continues. Do NOT set that env var unprompted.
- **Nothing slow on the UI thread (CLAUDE.md §1):** all of this is plain egui + one pure helper + a keymap/gesture handler; no new per-frame heavy work.
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean (use `--all-targets`, not `--lib`); no `unwrap()` in non-test code.
- **Scaffolding hygiene:** any not-yet-wired new symbol carries a scoped `#[allow(dead_code)]` removed in its consumer task; the final task leaves no plan-added allow.
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → STOP and hold for Jann's hands-on visual test (CLAUDE.md).
- **Branch:** `feat/develop-tool-registry` (these are visual-test-feedback fixes for that unmerged refactor).

---

## File Structure

**New:**
- `ferrolite-app/src/icons.rs` — semantic aliases over `egui_phosphor::regular::*` (`CROP`, `MASK`, `ADJUST`, `HEAL`, `BRUSH`, `LINEAR_GRADIENT`, `RADIAL_GRADIENT`, `LUMA`, `COLOR`, `EYEDROPPER`, `UNDO`, `REDO`, `DELETE`, `EDIT`, `RESET`, `STAR`, `STAR_FILL`, `FLAG`, `FLAG_REJECT`, `CARET_DOWN`, `CARET_UP`, `OVERLAY_ON`, `OVERLAY_OFF`, …) + the icon `FontFamily` handle. (Task 1)
- `ferrolite-app/src/develop/mask_components_modal.rs` — the component-management modal. (Task 10)

**Modified:**
- `ferrolite-app/Cargo.toml` (+`egui-phosphor`), `ferrolite-app/src/lib.rs`/`main.rs` module decl (`mod icons;`), `src/theme.rs` (font install), `CLAUDE.md` (icon rule). (Task 1)
- `src/widgets/tool_button.rs` (render in icon family); `develop/tools/{adjust,crop,mask,heal}.rs`, `develop/tool_palette.rs`, `develop/mask_panel.rs` (icon literals → `icons::*`). (Task 2)
- `src/library/icons.rs` (stars/flags/chevrons → `icons::*`; bespoke drawers removed), `src/widgets/mod.rs` (`draw_reset_arrow` → render `icons::RESET`). (Task 3)
- `src/settings/keymap.rs`, `src/settings/ui/keyboard.rs`, `src/help.rs`, `src/app.rs` (new actions + dispatch). (Task 4)
- `src/develop/mask_overlay.rs` (Ctrl+scroll brush gesture; color-eyedropper gate fix). (Tasks 5, 7)
- `src/develop/mask_panel.rs` (overlay toggle button; "Manage components" button). (Tasks 6, 10)
- `src/develop/mask_ui.rs` (`components_modal_open`, `editing_component`). (Task 10)
- `src/develop/mask_edit.rs` (`remove_component`). (Task 8)

**Untouched:** pipeline/OpStack/persistence, `mask_affordance.rs` (sample math reused), the brush rasterizer/perf path.

---

## Task 1: Add the icon library (`egui-phosphor`) + install + `icons` module + CLAUDE.md rule

**Files:**
- Modify: `ferrolite-app/Cargo.toml`, `ferrolite-app/src/theme.rs`, `ferrolite-app/src/main.rs` (or `lib.rs` — wherever modules are declared), `CLAUDE.md`
- Create: `ferrolite-app/src/icons.rs`
- Test: `#[cfg(test)] mod tests` in `icons.rs`

**Interfaces:**
- Produces: `ferrolite_app::icons` with `pub const <NAME>: &str` aliases + a way to get the icon `FontId` (e.g. `pub fn font(size: f32) -> egui::FontId`). Consumed by Tasks 2, 3, 6, 10.

- [ ] **Step 1: Add the dependency (correct version for egui 0.29)**

Determine the `egui-phosphor` release whose `egui` dependency is `0.29` (check the crate's metadata/changelog; egui-phosphor tags releases per egui version). Add to `ferrolite-app/Cargo.toml` `[dependencies]` (mirror the existing dependency style, near `egui`):

```toml
egui-phosphor = "<version matching egui 0.29>"
```

Run `cargo build -p ferrolite-app`. **If the crates.io fetch fails with a TLS/schannel revocation error**, STOP and report **BLOCKED** (the controller will re-run the fetch with the author-authorized `CARGO_HTTP_CHECK_REVOKE=false`, then you continue offline). If the chosen version's `egui` req is not `0.29`, pick the correct one. If NO published version targets egui 0.29, report BLOCKED with findings (fallback: vendor the crate's `Regular` `.ttf` into `assets/fonts/` + register directly — controller decides).

- [ ] **Step 2: Install the font in `theme::install_fonts`**

In `ferrolite-app/src/theme.rs` `install_fonts` (currently inserts Plex Sans/Mono then `ctx.set_fonts(fonts)` at ~theme.rs:36-57), add the Phosphor font BEFORE `ctx.set_fonts(fonts)`:

```rust
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
```

(Verify the exact API against the pinned version — `add_to_fonts(&mut FontDefinitions, Variant)` is the standard signature; the crate adds its font to the family fallback chain so its PUA codepoints resolve in `Proportional`. If the version exposes a named family instead, note it and expose that family from `icons::font`.)

- [ ] **Step 3: Write the failing test for `icons`**

Create `ferrolite-app/src/icons.rs`:

```rust
//! Semantic icon aliases over the bundled icon font (egui-phosphor, installed in
//! `theme::install_fonts`). UI code uses these constants (e.g. `icons::CROP`) rendered
//! in the icon font family — never raw emoji/symbol glyphs (IBM Plex + egui's emoji
//! subset don't cover them → tofu) and never new hand-drawn Painter icons. Add a new
//! icon = add one alias here, sourced from the Phosphor catalog
//! (`egui_phosphor::regular::*`).

use egui_phosphor::regular as p;

pub const ADJUST: &str = p::SLIDERS_HORIZONTAL;
pub const CROP: &str = p::CROP;
pub const MASK: &str = p::CIRCLE_HALF_TILT;
pub const HEAL: &str = p::BANDAIDS;
pub const BRUSH: &str = p::PAINT_BRUSH;
pub const LINEAR_GRADIENT: &str = p::ROWS;
pub const RADIAL_GRADIENT: &str = p::CIRCLE;
pub const LUMA: &str = p::CIRCLE_HALF;
pub const COLOR: &str = p::PALETTE;
pub const EYEDROPPER: &str = p::EYEDROPPER;
pub const UNDO: &str = p::ARROW_COUNTER_CLOCKWISE;
pub const REDO: &str = p::ARROW_CLOCKWISE;
pub const DELETE: &str = p::TRASH;
pub const EDIT: &str = p::PENCIL_SIMPLE;
pub const RESET: &str = p::ARROW_COUNTER_CLOCKWISE;
pub const STAR: &str = p::STAR;
pub const STAR_FILL: &str = p::STAR_FILL;
pub const FLAG: &str = p::FLAG;
pub const FLAG_REJECT: &str = p::FLAG_BANNER_FOLD; // or PROHIBIT / X — pick the closest reject glyph
pub const CARET_DOWN: &str = p::CARET_DOWN;
pub const CARET_UP: &str = p::CARET_UP;
pub const OVERLAY_ON: &str = p::EYE;
pub const OVERLAY_OFF: &str = p::EYE_SLASH;

/// The icon font. If `add_to_fonts` registered Phosphor into the Proportional fallback
/// chain, `FontFamily::Proportional` resolves the PUA codepoints and this is fine; if the
/// crate exposes a named family, return that instead.
pub fn font(size: f32) -> egui::FontId {
    egui::FontId::proportional(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_alias_is_nonempty() {
        for (name, s) in [
            ("ADJUST", ADJUST), ("CROP", CROP), ("MASK", MASK), ("HEAL", HEAL),
            ("BRUSH", BRUSH), ("LINEAR_GRADIENT", LINEAR_GRADIENT),
            ("RADIAL_GRADIENT", RADIAL_GRADIENT), ("LUMA", LUMA), ("COLOR", COLOR),
            ("EYEDROPPER", EYEDROPPER), ("UNDO", UNDO), ("REDO", REDO),
            ("DELETE", DELETE), ("EDIT", EDIT), ("RESET", RESET), ("STAR", STAR),
            ("STAR_FILL", STAR_FILL), ("FLAG", FLAG), ("FLAG_REJECT", FLAG_REJECT),
            ("CARET_DOWN", CARET_DOWN), ("CARET_UP", CARET_UP),
            ("OVERLAY_ON", OVERLAY_ON), ("OVERLAY_OFF", OVERLAY_OFF),
        ] {
            assert!(!s.is_empty(), "icon alias {name} is empty");
        }
    }
}
```

**IMPORTANT — verify every `p::NAME` against the pinned crate's actual constants.** The Phosphor constant names above are the intended mapping but MUST be confirmed to exist in `egui_phosphor::regular` for the pinned version (names occasionally differ, e.g. `CIRCLE_HALF_TILT` vs `CIRCLE_HALF`). If a name doesn't exist, pick the nearest real Phosphor glyph and note the substitution in your report. The build won't compile with a wrong constant, so this is compiler-enforced.

Declare the module: add `mod icons;` (or `pub mod icons;`) to `ferrolite-app/src/main.rs` (or wherever the crate's modules are declared — mirror the existing `mod theme;` etc.).

- [ ] **Step 4: Run the test + build**

Run: `cargo test -p ferrolite-app --lib icons` — Expected: PASS (after fixing any wrong constant names so it compiles).
Run: `cargo build -p ferrolite-app` — Expected: OK (font installs; nothing renders it yet).

- [ ] **Step 5: Add the CLAUDE.md icon rule**

Append a new section to `CLAUDE.md` (project root), after an existing load-bearing section:

```markdown
## UI icons (load-bearing)

EVERY icon in the app comes from the `icons` module (`ferrolite-app/src/icons.rs`), which
aliases the bundled icon font (`egui-phosphor`, installed once in `theme::install_fonts`)
and is rendered in the icon font family (via `widgets::tool_button` or a `FontId` from
`icons::font`). This includes tool/sub-tool icons, undo/redo, the rating **stars**,
**flags**, **chevrons**, and the per-control **reset** glyph. NEVER put raw emoji/symbol
characters in IBM Plex text and do NOT hand-draw new icons with `Painter` shapes — Plex +
egui's bundled emoji subset don't cover symbols (they render as tofu), and ad-hoc vector
icons fragment the system. Add a new icon by adding a semantic alias in `icons.rs` sourced
from the Phosphor catalog. The per-control reset affordance and its placement remain
load-bearing (see "Per-component reset"); only its glyph comes from the library.
```

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/Cargo.toml ferrolite-app/Cargo.lock ferrolite-app/src/icons.rs ferrolite-app/src/theme.rs ferrolite-app/src/main.rs CLAUDE.md
git commit -m "feat(icons): add egui-phosphor icon library + icons module + install font + CLAUDE.md rule"
```

---

## Task 2: Migrate the Develop icons to the library

**Files:**
- Modify: `ferrolite-app/src/widgets/tool_button.rs`, `develop/tools/{adjust,crop,mask,heal}.rs`, `develop/tool_palette.rs`, `develop/mask_panel.rs`
- Test: build + clippy + author visual test

**Interfaces:** Consumes `icons::*` + `icons::font` (Task 1).

- [ ] **Step 1: Render `tool_button`'s icon in the icon font family**

In `widgets/tool_button.rs`, change the `painter.text(...)` call from `egui::FontId::proportional(15.0)` to `crate::icons::font(15.0)` (identical if Phosphor is in the Proportional chain; explicit for clarity + future-proofing). Keep the `icon: &str` parameter and everything else.

- [ ] **Step 2: Point the tool/sub-tool/undo-redo icons at `icons::*`**

Replace the emoji literals:
- `develop/tools/adjust.rs::icon()` → `crate::icons::ADJUST`
- `develop/tools/crop.rs::icon()` → `crate::icons::CROP`
- `develop/tools/mask.rs::icon()` → `crate::icons::MASK`
- `develop/tools/heal.rs::icon()` → `crate::icons::HEAL`
- `develop/tool_palette.rs`: the undo/redo `tool_button(ui, "\u{21b6}", …)` / `"\u{21b7}"` → `crate::icons::UNDO` / `crate::icons::REDO`
- `develop/mask_panel.rs` sub-tool strip array (currently `(MaskTool::Brush, "🖌", …)` …): map each to `crate::icons::BRUSH`, `LINEAR_GRADIENT`, `RADIAL_GRADIENT`, `LUMA`, `COLOR`.

(`icon()` returns `&'static str`; `icons::*` are `&'static str` — types match.)

- [ ] **Step 3: Audit for stray glyph icons in the Develop UI**

Run a grep for other non-ASCII glyph literals used as icons in the develop UI and migrate any found:

Run: `rg -n "[\x{2190}-\x{2BFF}\x{1F000}-\x{1FAFF}]" ferrolite-app/src/develop ferrolite-app/src/widgets` (arrows/symbols/emoji ranges). For each hit that is a UI icon (not e.g. a degree sign in a label), replace with an `icons::*` alias (add the alias to `icons.rs` if missing). Report the full list of hits + how each was handled (migrated / left as non-icon text like `°`).

- [ ] **Step 4: Build + clippy**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean.
Run: `cargo build -p ferrolite-app` — Expected: OK.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/widgets/tool_button.rs ferrolite-app/src/develop
git commit -m "feat(develop): render tool/sub-tool/undo-redo icons from the icon library"
```

---

## Task 3: Migrate `library/icons.rs` (stars/flags/chevrons) + `draw_reset_arrow` to the library

**Files:**
- Modify: `ferrolite-app/src/library/icons.rs`, `ferrolite-app/src/widgets/mod.rs`, any callers of the removed drawers
- Test: build + clippy + author visual test (visual parity is the acceptance criterion)

**Interfaces:** Consumes `icons::*` + `icons::font`.

- [ ] **Step 1: Migrate the rating/flag/chevron drawers**

`library/icons.rs` currently hand-draws `star()` (and flag/chevron helpers) as `Painter` shapes. Convert each to render the corresponding library glyph in the icon font, preserving the existing call signature (so callers don't change) and the current sizing/placement/color/fill-state:
- `star(painter, center/rect, size, color, filled)` → `painter.text(center, egui::Align2::CENTER_CENTER, if filled { icons::STAR_FILL } else { icons::STAR }, icons::font(size), color)` (match the existing size/anchor the vector version used so ratings line up in the grid).
- flag pick/reject → `icons::FLAG` / `icons::FLAG_REJECT` similarly (preserve the filled/colored states the current code uses).
- chevron/caret → `icons::CARET_DOWN` / `CARET_UP`.

Keep each helper's public signature identical; only the body changes from shape-drawing to `painter.text`. If a helper's signature can't express the glyph cleanly (e.g. it took no color), adjust minimally and update its callers, noting it.

- [ ] **Step 2: Migrate `draw_reset_arrow` to the reset glyph**

In `widgets/mod.rs`, change `draw_reset_arrow(painter, center, r, color)` to render `icons::RESET` in the icon font instead of the hand-built arc+arrowhead, sized to `r` (pick a font size that visually matches the old ~`2r` glyph in the `EguiSlider` reset column):

```rust
pub(crate) fn draw_reset_arrow(painter: &egui::Painter, center: egui::Pos2, r: f32, color: egui::Color32) {
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        crate::icons::RESET,
        crate::icons::font(r * 2.2), // tune to match the reset column's prior visual size
        color,
    );
}
```

Keep the function name + signature (it's called by `EguiSlider` and is CLAUDE.md load-bearing) so no caller changes and the reset column keeps its exact placement — only the drawn glyph changes.

- [ ] **Step 3: Remove superseded bespoke drawing**

Delete the now-unused vector-geometry code (the arc/arrowhead body's helpers, the star/flag/chevron polygon builders) so there's one icon system. Keep only what's still referenced. Run clippy to confirm nothing dead remains.

- [ ] **Step 4: Build + clippy**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean (no dead code from removed drawers).
Run: `cargo build -p ferrolite-app` — Expected: OK.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/library/icons.rs ferrolite-app/src/widgets/mod.rs
git commit -m "feat(icons): migrate rating/flag/chevron + reset glyph to the icon library"
```

> **Visual-parity note for the author test:** rating stars (filled/empty across 0–5), pick/reject flags, dropdown carets, and the per-control reset arrow in every slider must look right and sit in the same place as before. This is the task most likely to need a size/anchor tweak.

---

## Task 4: Keybinds — tool switch (A/C/M) + toggle mask overlay (O)

**Files:**
- Modify: `ferrolite-app/src/settings/keymap.rs`, `src/settings/ui/keyboard.rs`, `src/help.rs`, `src/app.rs`
- Test: `#[cfg(test)] mod tests` in `keymap.rs`

**Interfaces:**
- Produces: `Action::{SwitchToolAdjust, SwitchToolCrop, SwitchToolMask, ToggleMaskOverlay}`; Develop-gated dispatch. Consumes `ToolState::select_tool`, `ToolId`, `MaskUiState.overlay_on`.

- [ ] **Step 1: Write the failing keymap tests**

Add to `keymap.rs` tests:

```rust
    #[test]
    fn new_develop_actions_have_defaults_and_no_internal_conflict() {
        let km = Keymap::defaults();
        use Action::*;
        // Every action (incl. the new ones) is bound — the existing exhaustiveness
        // test already covers this via Action::ALL; here assert the new ones resolve.
        for a in [SwitchToolAdjust, SwitchToolCrop, SwitchToolMask, ToggleMaskOverlay] {
            let _ = km.chord(a); // must not panic / must be present
        }
        // The new defaults must not collide with each other.
        let news = [SwitchToolAdjust, SwitchToolCrop, SwitchToolMask, ToggleMaskOverlay];
        for &a in &news {
            if let Some(other) = km.conflict(a, km.chord(a)) {
                assert!(news.contains(&other) == false || other == a,
                    "new action {a:?} conflicts with {other:?}");
            }
        }
    }
```

Run: `cargo test -p ferrolite-app --lib keymap` — Expected: FAIL to compile (new `Action` variants don't exist).

- [ ] **Step 2: Add the `Action` variants + `ALL` + `label()` + defaults**

In `keymap.rs`:
- Add `SwitchToolAdjust, SwitchToolCrop, SwitchToolMask, ToggleMaskOverlay` to the `Action` enum (after `OpenHelp` or grouped logically).
- Add them to `Action::ALL` (bump the array length accordingly — the existing `defaults_bind_every_action` test enforces coverage).
- Add `label()` arms (e.g. `"Tool: Adjust"`, `"Tool: Crop"`, `"Tool: Mask"`, `"Toggle mask overlay"`).
- In `defaults()`, add: `m.insert(SwitchToolAdjust, plain(Key::A)); m.insert(SwitchToolCrop, plain(Key::C)); m.insert(SwitchToolMask, plain(Key::M)); m.insert(ToggleMaskOverlay, plain(Key::O));`
- **Conflict check for `O`:** `FlagReject` defaults to `plain(Key::O)`. Run `Keymap::defaults().conflict(ToggleMaskOverlay, plain(Key::O))` mentally/in a scratch test: if `FlagReject` is reachable in the Develop context (it is a global flag action), `O` collides. **Resolve:** since flags apply broadly, pick a non-colliding default for `ToggleMaskOverlay` — use `plain(Key::T)` (mnemonic "toggle", verify `T` is unbound) or another free key; document the final choice in your report. (A/C/M: verify none of A/C/M is already a default — `SelectAll` is `Ctrl+A` (has Ctrl, so `plain(A)` is free); confirm C/M are free.)

- [ ] **Step 3: Run the keymap tests**

Run: `cargo test -p ferrolite-app --lib keymap` — Expected: PASS (incl. the existing exhaustiveness test with the enlarged `ALL`).

- [ ] **Step 4: Dispatch in `app.rs` (Develop-gated)**

Inside the existing `if self.module == crate::module::Module::Develop && self.state.viewer.is_some()` block (the one at ~app.rs:3329 that also guards NextImage/PrevImage; add near it, respecting `!ctx.wants_keyboard_input()` where text fields might capture), add:

```rust
    let km = &self.state.settings.keymap;
    let tool = if km.pressed(ctx, crate::settings::keymap::Action::SwitchToolAdjust) {
        Some(crate::develop::tool::ToolId::Adjust)
    } else if km.pressed(ctx, crate::settings::keymap::Action::SwitchToolCrop) {
        Some(crate::develop::tool::ToolId::Crop)
    } else if km.pressed(ctx, crate::settings::keymap::Action::SwitchToolMask) {
        Some(crate::develop::tool::ToolId::Mask)
    } else {
        None
    };
    if let Some(id) = tool {
        let enabled = self
            .tool_registry
            .get(id)
            .map(|t| t.enabled(&crate::develop::tool::DevelopCtx { state: &self.state }))
            .unwrap_or(false);
        if let Some(v) = self.state.viewer.as_mut() {
            v.tool_state.select_tool(id, enabled, &self.tool_registry);
        }
    }
    if self.state.settings.keymap.pressed(ctx, crate::settings::keymap::Action::ToggleMaskOverlay) {
        if let Some(v) = self.state.viewer.as_mut() {
            v.mask.overlay_on = !v.mask.overlay_on;
        }
    }
```

(Mirror the borrow discipline of the palette's `SelectTool` handler at ~app.rs:3712-3729 — compute `enabled` via a fresh `DevelopCtx` borrow, then take `&mut self.state.viewer`. Keep this inside the existing modal-suppressed / Develop-gated region.)

- [ ] **Step 5: Rebind UI + help cheat sheet**

- In `settings/ui/keyboard.rs`, add the four new actions to the `"Develop"` entry of `GROUPS` (they auto-render in the rebind grid).
- In `help.rs`, add the four to its shortcut table, plus a line: **"Ctrl + scroll — brush size (Mask ▸ Brush)"** (documents the Task-5 gesture even though it's not a rebindable action).

- [ ] **Step 6: Gate + commit**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` + `cargo test -p ferrolite-app --lib` — Expected: clean + pass.

```bash
git add ferrolite-app/src/settings ferrolite-app/src/help.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): keybinds — switch tool (A/C/M) + toggle mask overlay"
```

---

## Task 5: Ctrl + mouse-scroll brush-size gesture

**Files:**
- Modify: `ferrolite-app/src/develop/mask_overlay.rs` (or `route_brush`), and/or `src/app.rs` where canvas scroll/zoom is handled
- Test: a pure unit for the clamp/delta→radius math + build/clippy + author visual test

**Interfaces:** Mutates `MaskUiState.brush_radius`; must consume the scroll so canvas zoom doesn't double-fire.

- [ ] **Step 1: Confirm the canvas zoom-scroll path**

Find where the Develop canvas handles scroll for zoom (in `drive_viewer` / `app.rs`). Note the exact input field used (`i.raw_scroll_delta` vs `i.smooth_scroll_delta`) and how it detects Ctrl (`i.modifiers.command`/`.ctrl`). The brush gesture must read the same field and, when it acts, prevent the zoom handler from also consuming that scroll (either handle the brush gesture first and zero/consume the delta, or gate the zoom handler to skip when the brush gesture is active). Record the exact mechanism in your report.

- [ ] **Step 2: Write the failing pure test for the radius delta**

Add a small pure helper + test (in `mask_overlay.rs` or a pure module — keep it egui-free):

```rust
/// New brush radius from a scroll delta. `scroll_y > 0` (wheel up) grows the brush.
/// Clamped to the same [min,max] the panel slider uses.
pub(crate) fn brush_radius_from_scroll(current: f32, scroll_y: f32, min: f32, max: f32) -> f32 {
    const SENS: f32 = 0.0015; // radius units per scroll unit; tune in visual test
    (current + scroll_y * SENS).clamp(min, max)
}

#[cfg(test)]
mod scroll_tests {
    use super::*;
    #[test]
    fn scroll_up_grows_scroll_down_shrinks_and_clamps() {
        let (min, max) = (0.005, 0.5);
        assert!(brush_radius_from_scroll(0.1, 100.0, min, max) > 0.1, "up grows");
        assert!(brush_radius_from_scroll(0.1, -100.0, min, max) < 0.1, "down shrinks");
        assert_eq!(brush_radius_from_scroll(0.49, 100_000.0, min, max), max, "clamps hi");
        assert_eq!(brush_radius_from_scroll(0.01, -100_000.0, min, max), min, "clamps lo");
    }
}
```

Use the SAME `min`/`max` the panel brush-radius slider uses (find them in `mask_panel.rs` — the brush radius `EguiSlider`'s `min`/`max`; the plan's Step 1 investigation noted defaults like radius `0.08` — confirm the slider bounds and reuse them as constants).

Run: `cargo test -p ferrolite-app --lib brush_radius_from_scroll` — Expected: FAIL (fn not defined) → then PASS after adding it.

- [ ] **Step 3: Wire the gesture**

In the Mask-tool canvas path (where `route_brush` runs — `mask_overlay.rs`), when the Brush sub-tool is active and the pointer is over `image_rect`: if Ctrl is held and the scroll delta is non-zero, set `mask.brush_radius = brush_radius_from_scroll(mask.brush_radius, scroll_y, MIN, MAX)` and consume the scroll (per Step 1). Only in this context; otherwise leave scroll/zoom untouched.

- [ ] **Step 4: Gate + commit**

Run: `cargo test -p ferrolite-app --lib` + `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: pass + clean.

```bash
git add ferrolite-app/src/develop/mask_overlay.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): Ctrl+scroll brush-size gesture (gated to Mask+Brush, clamped, consumes scroll)"
```

---

## Task 6: Mask overlay (red tint) toggle button

**Files:**
- Modify: `ferrolite-app/src/develop/mask_panel.rs`
- Test: build + clippy + author visual test

- [ ] **Step 1: Add the toggle in the mask panel**

In `mask_panel::show` (near the masks list header, or in `selected_section` near the top), add a toggle button using the icon library that flips `mask.overlay_on`:

```rust
    let (icon, tip) = if mask.overlay_on {
        (crate::icons::OVERLAY_ON, "Hide mask overlay")
    } else {
        (crate::icons::OVERLAY_OFF, "Show mask overlay")
    };
    if crate::widgets::tool_button(ui, icon, tip, mask.overlay_on, true, None).clicked() {
        mask.overlay_on = !mask.overlay_on;
    }
```

Placement: a small row/button near the mask list header so it's reachable regardless of selection. The overlay-fill gate in `mask_overlay.rs` stays `overlay_on && !adjusting` (unchanged) — so toggling off hides the red tint persistently and the real adjusted image shows at rest; the drag-time auto-hide still works.

- [ ] **Step 2: Gate + commit**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean.

```bash
git add ferrolite-app/src/develop/mask_panel.rs
git commit -m "feat(develop): mask overlay on/off toggle button (persistent)"
```

---

## Task 7: Fix the color-range eyedropper (systematic-debugging)

**Files:**
- Modify: `ferrolite-app/src/develop/mask_overlay.rs` (routing), possibly `mask_panel.rs` (guard "Add Color range" when no mask selected)
- Test: code-analysis + build/clippy + **author visual test** (the fix is verified live)

> REQUIRED SUB-SKILL for the implementer: superpowers:systematic-debugging — reproduce/confirm the cause before changing code.

**Root cause (confirm):** `mask_overlay::show` returns early via `let (Some(idx), true) = (mask.selected, mask.active) else { return None; };` (mask_overlay.rs:84) BEFORE `route_color_eyedropper` runs — so arming "Pick color" without a *selected* mask makes the whole overlay routing (the eyedropper's `ui.interact`, cursor, loupe, and sampling) a silent no-op, and `drive_viewer`'s canvas interact (registered earlier in the frame) consumes clicks. The loupe/sample also require `preview_source` to be `Some`.

- [ ] **Step 1: Reproduce + confirm**

Read the routing (`mask_overlay.rs:84-94`) + `route_color_eyedropper` (`mask_overlay.rs:484-599`) + the app dispatch of the Mask canvas + `preview_source` threading (`develop/tools/mask.rs:41-59`). Confirm the two facts: (a) the `ColorRange` eyedropper is unreachable unless a mask is selected; (b) sampling+loupe need `preview_source`. Note in your report which is the operative cause for "nothing happens" (both plausible; the selection gate is primary since the picker button lives in `selected_section`, but arming then switching selection, or a no-component mask, can still dead-end; the loupe needs the source).

- [ ] **Step 2: Relax the eyedropper routing**

Restructure `mask_overlay::show` so the `ColorRange + picking_color` eyedropper path runs whenever the **Mask tool is active** (regardless of `mask.selected`), BEFORE / independent of the `(Some(idx), true)`-gated block that the shape/brush tools need. Concretely: handle the color-eyedropper case up front:

```rust
    // Color eyedropper is armed-mode and stages samples in MaskUiState (not tied to a
    // selected mask until "Add Color range"), so route it before the selection gate.
    if mask.active && mask.tool == MaskTool::ColorRange && mask.picking_color {
        route_color_eyedropper(ui, image_rect, mask, src_dims, stack, preview_source);
        return None;
    }
    let (Some(idx), true) = (mask.selected, mask.active) else { return None; };
    // ... existing brush / linear / radial routing unchanged ...
```

(Keep `overlay_on && !adjusting` overlay-fill painting where it is. Ensure `route_color_eyedropper` still early-returns if `!picking_color` — harmless now that the caller also checks, but keep it robust.)

- [ ] **Step 3: Guard "Add Color range" without a selected mask**

In `mask_panel.rs`, the "Add Color range" button calls `add_component(stack, idx, …)` which needs a selected mask `idx`. Ensure that button is only enabled when a mask is selected (`mask.selected.is_some()`), with a hint (e.g. tooltip "Select or create a mask first") when samples exist but no mask is selected — so picking colors works without a mask, and committing them prompts selecting one. (Sampling into `color_samples` already works without a selection after Step 2.)

- [ ] **Step 4: Ensure the sample source**

Confirm `preview_source` is populated on the happy path (it is set in `apply_preview_ready`; the overlay tint also depends on it). If it can be legitimately `None` (mid-decode), the armed cursor should still show but the loupe/sample simply do nothing that frame — never panic. No code change if already so; note the finding.

- [ ] **Step 5: Gate + commit**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` + `cargo test -p ferrolite-app --lib` — Expected: clean + pass.

```bash
git add ferrolite-app/src/develop/mask_overlay.rs ferrolite-app/src/develop/mask_panel.rs
git commit -m "fix(develop): color eyedropper works without a pre-selected mask (loupe + sampling reachable)"
```

> **Author visual test:** enter Mask tool → Color sub-tool → Pick color → loupe follows the cursor over the image, click adds a swatch; "Add Color range" is disabled until a mask is selected.

---

## Task 8: `mask_edit::remove_component` (pure helper + tests)

**Files:**
- Modify: `ferrolite-app/src/develop/mask_edit.rs`
- Test: `#[cfg(test)] mod tests` in `mask_edit.rs`

**Interfaces:** Produces `pub fn remove_component(stack: &OpStack, mask_idx: usize, comp_idx: usize) -> OpStack`. Consumed by Task 10.

- [ ] **Step 1: Write the failing test**

Add to `mask_edit.rs` tests (mirror the existing test style; use the crate's `MaskComponent`/`CompositeMode` + `create_mask`/`add_component` helpers to build a stack):

```rust
    #[test]
    fn remove_component_removes_the_indexed_component() {
        use ferrolite_mask::{CompositeMode, MaskComponent};
        let luma = |lo| MaskComponent::LumaRange { lo, hi: 1.0, softness: 0.1 };
        let s = create_mask(&OpStack::default(), "m".into());
        let s = add_component(&s, 0, luma(0.1), CompositeMode::Add);
        let s = add_component(&s, 0, luma(0.2), CompositeMode::Add);
        let s = add_component(&s, 0, luma(0.3), CompositeMode::Add);
        let out = remove_component(&s, 0, 1); // remove the middle one
        let comps = &layers(&out).layers[0].mask.components;
        assert_eq!(comps.len(), 2);
        assert_eq!(comps[0].0, luma(0.1));
        assert_eq!(comps[1].0, luma(0.3), "index 2 shifted down to 1");
    }

    #[test]
    fn remove_component_out_of_range_is_noop() {
        use ferrolite_mask::{CompositeMode, MaskComponent};
        let s = create_mask(&OpStack::default(), "m".into());
        let s = add_component(&s, 0, MaskComponent::LumaRange { lo: 0.1, hi: 1.0, softness: 0.1 }, CompositeMode::Add);
        assert_eq!(remove_component(&s, 0, 9), s, "bad comp idx -> unchanged");
        assert_eq!(remove_component(&s, 9, 0), s, "bad mask idx -> unchanged");
    }

    #[test]
    fn remove_last_component_keeps_the_layer() {
        use ferrolite_mask::{CompositeMode, MaskComponent};
        let s = create_mask(&OpStack::default(), "m".into());
        let s = add_component(&s, 0, MaskComponent::LumaRange { lo: 0.1, hi: 1.0, softness: 0.1 }, CompositeMode::Add);
        let out = remove_component(&s, 0, 0);
        assert_eq!(layers(&out).layers.len(), 1, "layer stays");
        assert!(layers(&out).layers[0].mask.components.is_empty());
    }
```

Run: `cargo test -p ferrolite-app --lib mask_edit` — Expected: FAIL (fn not defined).

- [ ] **Step 2: Implement**

Add to `mask_edit.rs` (mirror `delete_mask`'s bounds-checked style + reuse `edit_layer`):

```rust
/// Remove one component (by index) from a mask's definition. No-op if `mask_idx` or
/// `comp_idx` is out of range. The layer itself stays (even if it becomes empty).
pub fn remove_component(stack: &OpStack, mask_idx: usize, comp_idx: usize) -> OpStack {
    let la = layers(stack);
    if mask_idx >= la.layers.len() || comp_idx >= la.layers[mask_idx].mask.components.len() {
        return stack.clone();
    }
    edit_layer(stack, mask_idx, |layer| {
        layer.mask.components.remove(comp_idx);
    })
}
```

(Verify `edit_layer`'s signature — the investigation reported `fn edit_layer(stack, idx, f: impl FnOnce(&mut MaskLayer)) -> OpStack`. If it doesn't fit the empty-components case, follow `delete_mask`'s explicit `layers()`→mutate→`write()` shape instead.)

- [ ] **Step 3: Run tests + commit**

Run: `cargo test -p ferrolite-app --lib mask_edit` — Expected: PASS.

```bash
git add ferrolite-app/src/develop/mask_edit.rs
git commit -m "feat(develop): mask_edit::remove_component pure helper + tests"
```

---

## Task 9: Component-management modal + panel entry point

**Files:**
- Create: `ferrolite-app/src/develop/mask_components_modal.rs`
- Modify: `ferrolite-app/src/develop/mask_ui.rs` (state fields), `develop/mask_panel.rs` ("Manage components" button), `develop/mod.rs` (module decl), `app.rs` (render the modal + route its outcome), and the transient-reset sites (`apply_undo_redo`, mask switch/deselect)
- Test: pure unit for any extracted param↔component logic; build + clippy + author visual test

**Interfaces:** Consumes `mask_edit::remove_component` (Task 8), `mask_edit::set_component`, `icons::{DELETE, EDIT}`. Produces the modal `show(...) -> Option<EditOutcome>`.

- [ ] **Step 1: Add `MaskUiState` fields + resets**

In `mask_ui.rs`, add to `MaskUiState` (after `rename_buf`):

```rust
    /// The component-management modal is open for the selected mask.
    pub components_modal_open: bool,
    /// Which component index the modal is currently editing (Luma/Color), if any.
    pub editing_component: Option<usize>,
```

Defaults: `components_modal_open: false, editing_component: None,`.

Reset both to their defaults wherever transient mask state is reset: on mask switch (when `mask.selected` changes / a mask is selected), on mask-tool deselect, and in `apply_undo_redo` (app.rs, alongside the existing `v.mask.gesture = None; v.mask.overlay_key = None;` resets). Mirror those existing reset sites.

- [ ] **Step 2: Build the modal**

Create `develop/mask_components_modal.rs`. It renders an `egui::Window` (modal, `Order::Foreground`, closable) for the selected mask and returns `Option<EditOutcome>` (a delete or update). Pattern after the app's existing modals (`show_settings`/`show_help`) so it integrates with `modal_active()` (canvas/keybinds suppressed while open).

```rust
//! Modal for managing a selected mask's components: list + delete (any) + edit
//! (Luma/Color via set_component). Keeps the 296px panel uncluttered.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::{mask_edit, mask_ui::MaskUiState};
use crate::widgets::EguiSlider;
use ferrolite_mask::{CompositeMode, MaskComponent};
use ferrolite_pipeline::OpStack;

/// Render the modal if `mask.components_modal_open`. Returns an edit if one was made.
pub fn show(
    ctx: &egui::Context,
    stack: &OpStack,
    mask: &mut MaskUiState,
) -> Option<EditOutcome> {
    if !mask.components_modal_open {
        return None;
    }
    let Some(mask_idx) = mask.selected else {
        mask.components_modal_open = false; // nothing selected -> close
        return None;
    };
    let layers = mask_edit::layers(stack);
    let Some(layer) = layers.layers.get(mask_idx) else {
        mask.components_modal_open = false;
        return None;
    };
    let components = layer.mask.components.clone();

    let mut out: Option<EditOutcome> = None;
    let mut open = true;
    egui::Window::new(format!("Components — {}", layer.name))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            for (i, (comp, mode)) in components.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("{}. {}  [{:?}]", i + 1, component_label(comp), mode));
                    if crate::widgets::tool_button(ui, crate::icons::DELETE, "Remove", false, true, None).clicked() {
                        out = Some(commit_edit(mask_edit::remove_component(stack, mask_idx, i)));
                        if mask.editing_component == Some(i) { mask.editing_component = None; }
                    }
                    if is_editable(comp)
                        && crate::widgets::tool_button(ui, crate::icons::EDIT, "Edit", mask.editing_component == Some(i), true, None).clicked()
                    {
                        mask.editing_component = Some(i);
                        load_component_into_state(comp, mask); // prime the sliders
                    }
                });
            }
            // Inline editor for the component being edited (Luma/Color only).
            if let Some(i) = mask.editing_component {
                if let Some((comp, mode)) = components.get(i) {
                    ui.separator();
                    if let Some(updated) = edit_component_ui(ui, comp, mask) {
                        out = Some(commit_edit(mask_edit::set_component(stack, mask_idx, i, updated)));
                        let _ = mode;
                        mask.editing_component = None;
                    }
                }
            }
        });
    if !open {
        mask.components_modal_open = false;
        mask.editing_component = None;
    }
    out
}
```

Implement the helper functions in the same file:
- `component_label(&MaskComponent) -> &'static str` — "Brush" / "Linear gradient" / "Radial gradient" / "Luminance range" / "Color range" / "Imported".
- `is_editable(&MaskComponent) -> bool` — `true` for `LumaRange`/`ColorRange` only.
- `load_component_into_state(&MaskComponent, &mut MaskUiState)` — copy a Luma component's `lo/hi/softness` into `mask.range_lo/hi/softness`, or a Color component's `tolerance/softness/samples` into `mask.color_tolerance/softness/samples`.
- `edit_component_ui(ui, &MaskComponent, &mut MaskUiState) -> Option<MaskComponent>` — render the Luma sliders (lo/hi/softness) or Color sliders (tolerance/softness + swatches) via `EguiSlider` (per-control reset preserved), plus an **"Update"** button that returns the rebuilt `MaskComponent` from the current `mask.*` fields, and a **"Cancel"** button that sets `mask.editing_component = None` and returns `None`.
- `commit_edit(stack: OpStack) -> EditOutcome` — `EditOutcome { stack, kind: OpKind::LocalAdjustments, commit: true }` (mask edits share `OpKind::LocalAdjustments`; match how `mask_panel.rs`'s `commit(...)` helper builds outcomes — reuse it if `pub(crate)`).

**Extract the pure param↔component conversions** (`load_component_into_state`'s inverse — building a `MaskComponent` from the `mask.*` fields for Luma/Color) as a small pure function with a unit test (round-trip: component → load into state → rebuild → equal), since that's the logic most worth testing.

- [ ] **Step 2b: Write the pure round-trip test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn luma_component_round_trips_through_state() {
        let mut st = MaskUiState::default();
        let c = MaskComponent::LumaRange { lo: 0.2, hi: 0.7, softness: 0.15 };
        load_component_into_state(&c, &mut st);
        let rebuilt = luma_from_state(&st); // the pure rebuild helper
        assert_eq!(rebuilt, c);
    }
    // (analogous color round-trip if the color rebuild is pure)
}
```

Run: `cargo test -p ferrolite-app --lib mask_components_modal` — RED then GREEN.

- [ ] **Step 3: Panel entry point**

In `mask_panel::selected_section`, replace/augment the "N components" count label with a **"Manage components"** button (with `icons::EDIT`) that sets `mask.components_modal_open = true`:

```rust
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{} components", layer.mask.components.len())).size(11.0).color(theme::TEXT_FAINT));
        if crate::widgets::tool_button(ui, crate::icons::EDIT, "Manage components", false, true, None).clicked() {
            mask.components_modal_open = true;
        }
    });
```

- [ ] **Step 4: Render the modal + route its outcome (app.rs)**

Declare `pub mod mask_components_modal;` in `develop/mod.rs`. In `app.rs`, where the Develop mask UI is driven (near the mask panel/overlay dispatch), render the modal each frame and apply its outcome — pre-extract the stack (mirror the mask-panel borrow pattern), then:

```rust
    let modal_out = {
        let stack = self.state.viewer.as_ref().map(|v| v.op_stack.clone());
        match (stack, self.state.viewer.as_mut()) {
            (Some(stack), Some(v)) => crate::develop::mask_components_modal::show(ctx, &stack, &mut v.mask),
            _ => None,
        }
    };
    if let Some(o) = modal_out {
        self.apply_edit(ctx, frame, o.kind, o.stack, o.commit);
    }
```

(Adapt to the real borrow structure; `egui::Window` renders on `ctx`, so it can be called outside the SidePanel/CentralPanel closures. Ensure `modal_active()` includes this modal if keybind/canvas suppression is desired while it's open — add `components_modal_open` to that check if appropriate.)

- [ ] **Step 5: Gate + commit**

Run: `cargo test -p ferrolite-app --lib` + `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: pass + clean.

```bash
git add ferrolite-app/src/develop/mask_components_modal.rs ferrolite-app/src/develop/mask_ui.rs ferrolite-app/src/develop/mask_panel.rs ferrolite-app/src/develop/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): mask component-management modal (list + delete + Luma/Color edit)"
```

---

## Task 10: Workspace gate + author visual-test hand-off

**Files:** none (verification only).

- [ ] **Step 1: Remove any remaining plan-added scaffolding allows** (grep the touched files) and confirm clippy stays clean.

- [ ] **Step 2: Full gate**

Run: `cargo fmt --all --check` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo test --workspace`
Expected: all green.

- [ ] **Step 3: STOP — hand over the visual test plan**

Per CLAUDE.md, hold for Jann's hands-on test. Checklist:
1. **Icons everywhere:** tool palette (Adjust/Crop/Mask/Heal) + undo/redo, mask sub-tool strip (brush/linear/radial/luma/color), the overlay toggle, delete/edit in the component modal — all render (no tofu). **Library-wide:** rating stars (0–5, filled/empty), pick/reject flags, dropdown carets, and the per-control **reset arrow** on every slider look right and sit where they did.
2. **Keybinds:** A/C/M switch tools; O (or the chosen key) toggles the red mask overlay; all rebindable in Settings ▸ Keyboard; help sheet lists them.
3. **Ctrl+scroll** over the image with Mask ▸ Brush active resizes the brush (up=bigger), clamped; does NOT zoom the canvas; outside that context scroll/zoom is normal.
4. **Overlay toggle** (button + key): red tint off persistently → the real adjusted image shows at rest; dragging a Light/Color slider still momentarily reveals the effect.
5. **Color picker:** Mask ▸ Color ▸ Pick color → picker cursor + zoom loupe follow the pointer; click samples a swatch; works even before a mask is selected; "Add Color range" enabled only once a mask is selected.
6. **Component modal:** "Manage components" opens the modal; each component lists with delete (any) + edit (Luma/Color); editing loads its params, "Update" applies via the pipeline, "Cancel" discards; delete removes it; undo/redo works; closing/reopening is clean.
7. **No regressions:** all prior Develop behavior (tabs, tools, per-control reset, masking, persistence) unchanged; no freeze.

Address findings, then finish per CLAUDE.md (do not merge/PR on your own).

---

## Self-Review

**1. Spec coverage:** §4 icon library + install + CLAUDE.md rule → Task 1; §4.5 full audit/migration → Tasks 2 (Develop) + 3 (library/reset) + the Task 2 grep; §5.1 keymap actions → Task 4; §5.2 Ctrl+scroll gesture → Task 5; §5.3 rebind UI + help → Task 4; §6 overlay toggle → Task 6 (+ keybind Task 4); §7 picker fix → Task 7; §8 remove_component → Task 8, modal + `editing_component`/`components_modal_open` → Task 9; §10 error handling → covered across tasks (bounds-checked helper, gesture clamp, stale-state resets, modal close-on-no-selection); §11 decomposition → matches; brush perf → correctly out of scope. Gate + hold → Task 10.

**2. Placeholder scan:** The `p::NAME` Phosphor constants + the egui-phosphor version + the exact scroll field + `edit_layer` fit are flagged "verify against the pinned crate / real code" with the source named and compiler-enforcement noted — appropriate for values only knowable at execution against a newly-added dependency; not hand-waving (each names exactly what to check and the fallback). All code steps show complete code.

**3. Type consistency:** `icons::*` are `&'static str` (match `icon()`/`tool_button`'s `&str`); `icons::font(f32) -> FontId`; `remove_component(&OpStack, usize, usize) -> OpStack` used identically in Tasks 8/9; `EditOutcome { stack, kind, commit }` + `OpKind::LocalAdjustments` reused; `select_tool(id, enabled, reg)` matches the palette call; `MaskUiState` new fields (`components_modal_open`, `editing_component`) consistent across Tasks 9/state-resets. Keymap `Action` new variants + `ALL` bump + `label()` + `defaults()` consistent.

**Open items to confirm at execution (flagged in-task):** egui-phosphor version for egui 0.29 + exact `regular::*` constant names; the `O` keybind conflict resolution (likely move to a free key); the canvas zoom scroll-input field + how to consume it; `edit_layer` empty-components fit; `modal_active()` inclusion of the component modal.
