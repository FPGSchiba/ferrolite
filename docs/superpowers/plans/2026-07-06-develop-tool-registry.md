# Develop Tool Registry, Floating Palette & Tabbed Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the ad-hoc section-driven Develop tool entry with one standardized `DevelopTool` + `PanelTab` trait/registry system, surfaced as a toggleable floating tool palette and a contextual tabbed right panel, migrating Adjust/Crop/Mask/Heal with no behavior loss and making the mask color eyedropper a discoverable Pick-color + zoom-loupe sub-tool.

**Architecture:** A new `develop/tool.rs` defines `ToolId`/`TabId`/`DevelopCtx`/`PanelTab`/`DevelopTool` + a `DevelopToolRegistry` built once and stored on `FerroliteApp`. A pure, `Copy`, egui-free `ToolState` (on `ViewerState`) holds active tool / active tab / remembered base tab / palette visibility and is unit-tested. The floating palette (`tool_palette.rs`, an interactive `egui::Area` mirroring `draw_histogram_overlay`) returns `PaletteAction`s the app applies; the right-panel shell (`tool_panel.rs`) renders global chrome + a shared tab bar (base tabs ++ the active tool's temporary tab) then dispatches the active tab's `show`. The concrete tools (`develop/tools/{adjust,crop,mask,heal}.rs`) and base tabs (`develop/base_tabs.rs`) wrap the **existing** `adjustment_panel` section bodies / `crop_overlay::show` / `mask_overlay::show` / `mask_panel::show` verbatim, so `OpStack`, history, persistence, render tiers, and `apply_edit` are untouched — this is a presentation + input-routing reorganization only.

**Tech Stack:** Rust, `egui`/`eframe`, `ferrolite-pipeline` (`OpStack`/`OpKind`), the existing `ferrolite-app` Develop UI (`adjustment_panel`, `crop_overlay`, `mask_overlay`, `mask_panel`, `mask_ui`, `mask_affordance`, `widgets::EguiSlider`/`draw_reset_arrow`, `settings`, `chrome`).

## Global Constraints

- **Presentation/interaction refactor only** — NO new edits, ops, or mask components; NO change to `OpStack`, history, persistence, `EditOutcome`, `apply_edit`, `apply_undo_redo`, the render pipeline, or `EditOutcome`/`OpKind`. (spec §2, §3, §13)
- **Reuse verbatim:** `EditOutcome { stack, kind, commit }` (in `adjustment_panel.rs:31-35`), `OpKind` (`ferrolite-pipeline/src/op.rs:163-177`), `EguiSlider` + `draw_reset_arrow` — **per-control reset stays load-bearing for every migrated slider** (spec §9; CLAUDE.md). Migrated code MOVES existing section/overlay bodies without altering their edit logic.
- **No behavior loss:** Adjust/Crop/Mask produce the same edits, same per-control reset, same overlays; Heal stays greyed/inert (P5). (spec §1, §8)
- **Nothing slow on the UI thread (CLAUDE.md §1):** palette + tab bar are plain egui vector drawing; the mask overlay's existing bounded rebuild (`rebuild_mask_overlay_if_needed`) is unchanged. Build the registry ONCE (stored on `FerroliteApp`), never per frame.
- **`ToolState` is pure + egui-free + `Copy`** (all fields `Copy`), unit-tested to the 80%+ target (spec §5, §11). Any hit-test/state math added stays a pure tested unit; egui only routes input (the masking-overlay discipline).
- **Settings tolerance:** `Settings` is `#[serde(default)]`; a new `show_tool_palette: bool` (default `true`) must be added to the struct, `Default`, and `settings/persist.rs`, mirroring `show_histogram`. (spec §6)
- **Scaffolding hygiene:** new modules/tools that are not yet wired carry a scoped `#[allow(dead_code)]` until their consumer task lands; **all such allows are removed in Task 13**, and their removal must leave `cargo clippy --workspace --all-targets -- -D warnings` clean (proves nothing is left unconsumed) — the same discipline P1 Plan 4 used.
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → then STOP and hold for the author's (Jann's) hands-on visual test before finishing the branch (CLAUDE.md).
- **Branch:** `feat/develop-tool-registry` (already checked out; stacked on main which now has P1 masking Plans 1–5).

---

## File Structure

**New files (`ferrolite-app/src/develop/`):**
- `tool.rs` — `ToolId`, `TabId`, `DevelopCtx<'a>`, `PanelTab` trait, `DevelopTool` trait, `DevelopToolRegistry`. The extensibility API. (Tasks 1, 9)
- `tool_state.rs` — pure `ToolState` selection logic (active tool, active/base tab, palette visibility). (Task 2)
- `base_tabs.rs` — the 5 always-present adjustment tabs (`LightTab`/`ColorTab`/`CurveTab`/`DetailTab`/`OpticsTab`) + `base_tabs()`; each wraps an existing `adjustment_panel` section body. (Task 5)
- `tool_palette.rs` — the floating toggleable palette (`egui::Area`) rendering tools + undo/redo, returning `PaletteAction`. (Task 10)
- `tool_panel.rs` — the right-panel shell: global chrome + shared tab bar + active-tab dispatch. (Task 11)
- `tools/mod.rs`, `tools/adjust.rs`, `tools/crop.rs`, `tools/mask.rs`, `tools/heal.rs` — the concrete tools. (Tasks 6/7/8/9)

**New widgets (`ferrolite-app/src/widgets/`):**
- `tool_button.rs` — shared tool/tab button (icon/label, active/hover/disabled). (Task 4)

**Modified:**
- `ferrolite-app/src/develop/mod.rs` — declare the new modules.
- `ferrolite-app/src/widgets/mod.rs` — declare + export `tool_button`.
- `ferrolite-app/src/viewer/mod.rs` — add `tool_state: ToolState` (default) to `ViewerState` (~line 199, beside `mask`).
- `ferrolite-app/src/settings/mod.rs`, `settings/persist.rs`, `settings/ui.rs` — `show_tool_palette`. (Task 3)
- `ferrolite-app/src/chrome/mod.rs` — `MenuAction::ToggleToolPalette` + View-menu item + `title_bar` param. (Task 3)
- `ferrolite-app/src/app.rs` — build/store the registry; add `mark_settings_dirty` handler; frame-start `crop_active`/`mask.active` derived from `tool_state`; palette `Area`; swap the `SidePanel::right("develop_adjust")` body from `adjustment_panel::show` to `tool_panel::show`; drop the section-based `crop_active`/`mask.active` triggers. (Tasks 3, 10, 11, 13)
- `ferrolite-app/src/develop/adjustment_panel.rs` — section bodies MOVE into base tabs / crop tab; the file keeps `EditOutcome`/`PanelOutcome` type defs + the top chrome helpers (extracted for reuse); `show` is removed in Task 13.
- `ferrolite-app/src/develop/mask_ui.rs` — add `picking_color: bool` to `MaskUiState`. (Task 12)
- `ferrolite-app/src/develop/mask_overlay.rs` — eyedropper armed pick mode + picker cursor + zoom loupe in `route_color_eyedropper`. (Task 12)
- `ferrolite-app/src/develop/mask_panel.rs` — sub-tool strip (icons) + "Pick color" button. (Tasks 7, 12)

**Untouched (wrapped, not edited in their edit logic):** `crop_overlay.rs`, `mask_affordance.rs`, `mask_edit.rs`, `curve_widget.rs`, `hsl_widget.rs`, all pipeline/pipeline-test/golden code.

---

## Task 1: `tool.rs` — traits, ids, registry skeleton

**Files:**
- Create: `ferrolite-app/src/develop/tool.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (add `pub mod tool;`)
- Test: `#[cfg(test)] mod tests` in `tool.rs`

**Interfaces:**
- Consumes: `crate::develop::adjustment_panel::EditOutcome`, `crate::state::AppState`, `egui`.
- Produces:
  - `pub enum ToolId { Adjust, Crop, Mask, Heal }` (`Clone, Copy, PartialEq, Eq, Debug`).
  - `pub struct TabId(pub &'static str)` (`Clone, Copy, PartialEq, Eq, Debug`).
  - `pub struct DevelopCtx<'a> { pub state: &'a AppState }`.
  - `pub trait PanelTab { fn id(&self)->TabId; fn label(&self)->&str; fn show(&self, ui:&mut egui::Ui, state:&mut AppState)->Option<EditOutcome>; }`
  - `pub trait DevelopTool { fn id(&self)->ToolId; fn icon(&self)->&'static str; fn label(&self)->&'static str; fn enabled(&self, ctx:&DevelopCtx)->bool; fn tabs(&self)->Vec<Box<dyn PanelTab>> { Vec::new() } fn canvas(&self, ui:&mut egui::Ui, image_rect:egui::Rect, state:&mut AppState)->Option<EditOutcome> { let _=(ui,image_rect,state); None } }`
  - `pub struct DevelopToolRegistry { tools: Vec<Box<dyn DevelopTool>>, base_tabs: Vec<Box<dyn PanelTab>> }` with `new(base_tabs, tools)`, `tools()->&[Box<dyn DevelopTool>]`, `base_tabs()->&[Box<dyn PanelTab>]`, `get(ToolId)->Option<&dyn DevelopTool>`.
  - **Deviation from spec §4/§5, documented here:** `standard()` is added later (Task 9, once concrete tools exist); Task 1 provides `new(...)` so `ToolState` (Task 2) and tests can build a registry from dummy tools without the whole migration.

- [ ] **Step 1: Declare the module (scaffolding)**

In `ferrolite-app/src/develop/mod.rs`, add near the other `pub mod` lines:

```rust
pub mod tool;
```

- [ ] **Step 2: Write the failing test**

Create `ferrolite-app/src/develop/tool.rs` with the definitions below AND this test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTab(TabId, &'static str);
    impl PanelTab for DummyTab {
        fn id(&self) -> TabId { self.0 }
        fn label(&self) -> &str { self.1 }
        fn show(&self, _ui: &mut egui::Ui, _state: &mut AppState) -> Option<EditOutcome> { None }
    }

    struct DummyTool { id: ToolId, enabled: bool, tabs: Vec<TabId> }
    impl DevelopTool for DummyTool {
        fn id(&self) -> ToolId { self.id }
        fn icon(&self) -> &'static str { "x" }
        fn label(&self) -> &'static str { "dummy" }
        fn enabled(&self, _ctx: &DevelopCtx) -> bool { self.enabled }
        fn tabs(&self) -> Vec<Box<dyn PanelTab>> {
            self.tabs.iter().map(|t| Box::new(DummyTab(*t, "t")) as Box<dyn PanelTab>).collect()
        }
    }

    fn dummy_registry() -> DevelopToolRegistry {
        DevelopToolRegistry::new(
            vec![Box::new(DummyTab(TabId("light"), "Light")) as Box<dyn PanelTab>],
            vec![
                Box::new(DummyTool { id: ToolId::Adjust, enabled: true, tabs: vec![] }) as Box<dyn DevelopTool>,
                Box::new(DummyTool { id: ToolId::Crop, enabled: true, tabs: vec![TabId("crop")] }),
                Box::new(DummyTool { id: ToolId::Heal, enabled: false, tabs: vec![] }),
            ],
        )
    }

    #[test]
    fn registry_get_resolves_by_id_and_reports_enabled() {
        let reg = dummy_registry();
        assert_eq!(reg.tools().len(), 3);
        let crop = reg.get(ToolId::Crop).expect("crop present");
        assert_eq!(crop.id(), ToolId::Crop);
        assert_eq!(reg.base_tabs().len(), 1);
        assert!(reg.get(ToolId::Mask).is_none(), "mask not in this dummy registry");
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p ferrolite-app --lib develop::tool`
Expected: FAIL to compile — the types/`new`/`get` don't exist yet.

- [ ] **Step 4: Write the definitions**

At the top of `ferrolite-app/src/develop/tool.rs` (above the test module):

```rust
//! The Develop tool/tab extensibility API (design §4). A `DevelopTool` is a palette
//! entry with an optional canvas overlay + temporary panel tab(s); a `PanelTab` is one
//! control group in the right panel. The `DevelopToolRegistry` (built once, on
//! `FerroliteApp`) owns the always-present base adjustment tabs + the ordered canvas
//! tools. Adding a tool = implement `DevelopTool` + push it in `standard()`; adding a
//! tab = implement `PanelTab` + include it in `base_tabs()` or a tool's `tabs()`.

use crate::develop::adjustment_panel::EditOutcome;
use crate::state::AppState;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolId {
    Adjust,
    Crop,
    Mask,
    Heal,
}

/// Stable per-tab id (e.g. `TabId("light")`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TabId(pub &'static str);

/// Read-only context a tool reads to decide its enabled-state.
pub struct DevelopCtx<'a> {
    pub state: &'a AppState,
}

/// One control group in the right panel.
pub trait PanelTab {
    fn id(&self) -> TabId;
    fn label(&self) -> &str;
    /// Render this tab's controls; return an op edit if one was made this frame.
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome>;
}

/// A Develop canvas tool: a palette entry + its overlay + the temporary tab(s) it
/// injects while active. `Adjust` is the default no-canvas-tool state (base tabs
/// only, no overlay). `Heal` is disabled (P5).
pub trait DevelopTool {
    fn id(&self) -> ToolId;
    fn icon(&self) -> &'static str;
    fn label(&self) -> &'static str;
    fn enabled(&self, ctx: &DevelopCtx) -> bool;
    /// Temporary tab(s) appended after the base tabs while this tool is active.
    fn tabs(&self) -> Vec<Box<dyn PanelTab>> {
        Vec::new()
    }
    /// Optional canvas overlay/affordances; returns an op edit if a gesture produced
    /// one this frame. `None` = no overlay.
    fn canvas(
        &self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        state: &mut AppState,
    ) -> Option<EditOutcome> {
        let _ = (ui, image_rect, state);
        None
    }
}

/// Base adjustment tabs + the ordered canvas tools shown in the palette. Built once.
pub struct DevelopToolRegistry {
    tools: Vec<Box<dyn DevelopTool>>,
    base_tabs: Vec<Box<dyn PanelTab>>,
}

impl DevelopToolRegistry {
    pub fn new(base_tabs: Vec<Box<dyn PanelTab>>, tools: Vec<Box<dyn DevelopTool>>) -> Self {
        Self { tools, base_tabs }
    }
    pub fn tools(&self) -> &[Box<dyn DevelopTool>] {
        &self.tools
    }
    pub fn base_tabs(&self) -> &[Box<dyn PanelTab>] {
        &self.base_tabs
    }
    pub fn get(&self, id: ToolId) -> Option<&dyn DevelopTool> {
        self.tools.iter().find(|t| t.id() == id).map(|b| b.as_ref())
    }
}
```

Add a crate-level scaffolding allow at the top of the file (removed in Task 13):

```rust
#![allow(dead_code)] // wired incrementally across this plan; removed at Task 13
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p ferrolite-app --lib develop::tool` — Expected: PASS.
Run: `cargo clippy -p ferrolite-app --lib -- -D warnings` — Expected: clean (the `#![allow(dead_code)]` covers the not-yet-consumed API).

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/develop/tool.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(develop): DevelopTool + PanelTab traits + registry skeleton (extensibility API)"
```

---

## Task 2: `tool_state.rs` — pure selection state

**Files:**
- Create: `ferrolite-app/src/develop/tool_state.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod tool_state;`)
- Test: `#[cfg(test)] mod tests` in `tool_state.rs`

**Interfaces:**
- Consumes: `ToolId`, `TabId`, `DevelopToolRegistry` (Task 1).
- Produces:
  - `pub struct ToolState { pub active: ToolId, pub active_tab: TabId, pub base_tab: TabId, pub palette_visible: bool }` — derives `Clone, Copy, PartialEq, Debug`. `Default`: `active=Adjust, active_tab=TabId("light"), base_tab=TabId("light"), palette_visible=true`.
  - `pub fn select_tool(&mut self, id: ToolId, enabled: bool, reg: &DevelopToolRegistry)` — **deviation from spec §5:** takes an `enabled: bool` (computed by the caller via `DevelopTool::enabled`) so `ToolState` stays free of `AppState`/`DevelopCtx`. Ignores `enabled == false`. On `Adjust`: restore `active_tab = base_tab`. On a real enabled tool: `active_tab =` that tool's first `tabs()` id (or `base_tab` if it has none).
  - `pub fn select_tab(&mut self, tab: TabId, reg: &DevelopToolRegistry)` — set `active_tab = tab`; if `tab` is one of `reg.base_tabs()` ids, also set `base_tab = tab`.
  - `pub fn tab_bar(&self, reg: &DevelopToolRegistry) -> Vec<TabId>` — base tab ids ++ (if `active != Adjust`) the active tool's `tabs()` ids.
  - `pub fn ensure_valid_tab(&mut self, reg: &DevelopToolRegistry)` — if `active_tab` is not in `tab_bar(reg)`, clamp to the first entry (never a blank panel).

- [ ] **Step 1: Declare the module**

In `ferrolite-app/src/develop/mod.rs`: `pub mod tool_state;`

- [ ] **Step 2: Write the failing tests**

Create `ferrolite-app/src/develop/tool_state.rs` with (reuse the dummy-registry test helpers pattern — define them in this test module too):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::tool::{DevelopCtx, DevelopTool, PanelTab};
    use crate::state::AppState;

    struct DummyTab(TabId);
    impl PanelTab for DummyTab {
        fn id(&self) -> TabId { self.0 }
        fn label(&self) -> &str { "t" }
        fn show(&self, _ui: &mut egui::Ui, _s: &mut AppState) -> Option<crate::develop::adjustment_panel::EditOutcome> { None }
    }
    struct DummyTool { id: ToolId, enabled: bool, tabs: Vec<TabId> }
    impl DevelopTool for DummyTool {
        fn id(&self) -> ToolId { self.id }
        fn icon(&self) -> &'static str { "x" }
        fn label(&self) -> &'static str { "d" }
        fn enabled(&self, _c: &DevelopCtx) -> bool { self.enabled }
        fn tabs(&self) -> Vec<Box<dyn PanelTab>> {
            self.tabs.iter().map(|t| Box::new(DummyTab(*t)) as Box<dyn PanelTab>).collect()
        }
    }
    fn reg() -> DevelopToolRegistry {
        DevelopToolRegistry::new(
            vec![
                Box::new(DummyTab(TabId("light"))) as Box<dyn PanelTab>,
                Box::new(DummyTab(TabId("color"))),
            ],
            vec![
                Box::new(DummyTool { id: ToolId::Adjust, enabled: true, tabs: vec![] }) as Box<dyn DevelopTool>,
                Box::new(DummyTool { id: ToolId::Crop, enabled: true, tabs: vec![TabId("crop")] }),
                Box::new(DummyTool { id: ToolId::Heal, enabled: false, tabs: vec![] }),
            ],
        )
    }

    #[test]
    fn default_is_adjust_with_a_base_tab() {
        let s = ToolState::default();
        assert_eq!(s.active, ToolId::Adjust);
        assert_eq!(s.active_tab, TabId("light"));
        assert!(s.palette_visible);
    }

    #[test]
    fn selecting_disabled_tool_is_a_no_op() {
        let reg = reg();
        let mut s = ToolState::default();
        s.select_tool(ToolId::Heal, false, &reg);
        assert_eq!(s.active, ToolId::Adjust, "disabled Heal ignored");
    }

    #[test]
    fn selecting_a_tool_auto_selects_its_first_temporary_tab() {
        let reg = reg();
        let mut s = ToolState::default();
        s.select_tool(ToolId::Crop, true, &reg);
        assert_eq!(s.active, ToolId::Crop);
        assert_eq!(s.active_tab, TabId("crop"), "auto-selects the crop temp tab");
    }

    #[test]
    fn selecting_adjust_restores_remembered_base_tab() {
        let reg = reg();
        let mut s = ToolState::default();
        s.select_tab(TabId("color"), &reg); // remember color as base
        s.select_tool(ToolId::Crop, true, &reg);
        assert_eq!(s.active_tab, TabId("crop"));
        s.select_tool(ToolId::Adjust, true, &reg);
        assert_eq!(s.active, ToolId::Adjust);
        assert_eq!(s.active_tab, TabId("color"), "restores remembered base tab");
    }

    #[test]
    fn tab_bar_is_base_then_active_tool_tabs() {
        let reg = reg();
        let mut s = ToolState::default();
        assert_eq!(s.tab_bar(&reg), vec![TabId("light"), TabId("color")]);
        s.select_tool(ToolId::Crop, true, &reg);
        assert_eq!(s.tab_bar(&reg), vec![TabId("light"), TabId("color"), TabId("crop")]);
    }

    #[test]
    fn ensure_valid_tab_clamps_stale_tab() {
        let reg = reg();
        let mut s = ToolState { active: ToolId::Adjust, active_tab: TabId("gone"), base_tab: TabId("gone"), palette_visible: true };
        s.ensure_valid_tab(&reg);
        assert_eq!(s.active_tab, TabId("light"), "stale tab clamps to first base tab");
    }
}
```

- [ ] **Step 3: Run to verify fail**

Run: `cargo test -p ferrolite-app --lib develop::tool_state`
Expected: FAIL to compile — `ToolState` and its methods don't exist.

- [ ] **Step 4: Implement**

At the top of `tool_state.rs`:

```rust
//! Pure, egui-free Develop tool/tab selection state (design §5). `Copy` so the app can
//! read it out of `ViewerState`, mutate a local while rendering, and write it back —
//! avoiding a multi-field borrow against `&mut AppState`.

use crate::develop::tool::{DevelopToolRegistry, TabId, ToolId};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ToolState {
    pub active: ToolId,
    pub active_tab: TabId,
    pub base_tab: TabId,
    pub palette_visible: bool,
}

impl Default for ToolState {
    fn default() -> Self {
        Self {
            active: ToolId::Adjust,
            active_tab: TabId("light"),
            base_tab: TabId("light"),
            palette_visible: true,
        }
    }
}

impl ToolState {
    fn first_tab_of(&self, id: ToolId, reg: &DevelopToolRegistry) -> Option<TabId> {
        reg.get(id).and_then(|t| t.tabs().first().map(|tab| tab.id()))
    }

    pub fn select_tool(&mut self, id: ToolId, enabled: bool, reg: &DevelopToolRegistry) {
        if !enabled {
            return;
        }
        self.active = id;
        self.active_tab = if id == ToolId::Adjust {
            self.base_tab
        } else {
            self.first_tab_of(id, reg).unwrap_or(self.base_tab)
        };
    }

    pub fn select_tab(&mut self, tab: TabId, reg: &DevelopToolRegistry) {
        self.active_tab = tab;
        if reg.base_tabs().iter().any(|t| t.id() == tab) {
            self.base_tab = tab;
        }
    }

    pub fn tab_bar(&self, reg: &DevelopToolRegistry) -> Vec<TabId> {
        let mut ids: Vec<TabId> = reg.base_tabs().iter().map(|t| t.id()).collect();
        if self.active != ToolId::Adjust {
            if let Some(t) = reg.get(self.active) {
                ids.extend(t.tabs().iter().map(|tab| tab.id()));
            }
        }
        ids
    }

    pub fn ensure_valid_tab(&mut self, reg: &DevelopToolRegistry) {
        let bar = self.tab_bar(reg);
        if !bar.contains(&self.active_tab) {
            if let Some(first) = bar.first() {
                self.active_tab = *first;
            }
        }
    }
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ferrolite-app --lib develop::tool_state` — Expected: PASS (6 tests).
Run: `cargo clippy -p ferrolite-app --lib -- -D warnings` — Expected: clean (`ToolState` still unused outside tests → covered by `tool.rs`'s allow? No — add `#[allow(dead_code)]` on `ToolState` here until Task 9 wires it).

Add above `pub struct ToolState`:

```rust
#[allow(dead_code)] // wired onto ViewerState in Task 9; removed at Task 13
```

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/develop/tool_state.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(develop): pure ToolState selection logic (active tool/tab, base-tab memory, clamp) + tests"
```

---

## Task 3: `settings.show_tool_palette` + View-menu toggle

**Files:**
- Modify: `ferrolite-app/src/settings/mod.rs` (struct + `Default`)
- Modify: `ferrolite-app/src/settings/persist.rs` (persistence tuple, ~line 75)
- Modify: `ferrolite-app/src/settings/ui.rs` (Settings-window checkbox, ~line 178)
- Modify: `ferrolite-app/src/chrome/mod.rs` (`MenuAction`, `title_bar` param + menu item)
- Modify: `ferrolite-app/src/app.rs` (handler + `title_bar` call site)
- Test: `#[cfg(test)] mod tests` in `settings/mod.rs` (default value)

**Interfaces:**
- Produces: `Settings.show_tool_palette: bool` (default `true`); `MenuAction::ToggleToolPalette`; `title_bar(..., show_tool_palette: bool)` param.

- [ ] **Step 1: Write the failing test**

Add to `settings/mod.rs` test module (create one if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_palette_defaults_on() {
        assert!(Settings::default().show_tool_palette);
    }
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p ferrolite-app --lib settings::` — Expected: FAIL to compile (no `show_tool_palette`).

- [ ] **Step 3: Add the field + default + persistence + settings checkbox**

In `settings/mod.rs`, add to the `Settings` struct (after `show_histogram: bool,`):

```rust
    pub show_tool_palette: bool,
```

In `impl Default for Settings`, after `show_histogram: true,`:

```rust
            show_tool_palette: true,
```

In `settings/persist.rs`, locate the line referencing `s.show_histogram,` (~line 75) and add `s.show_tool_palette,` immediately after it (mirror whatever tuple/row form is used — match the surrounding pattern exactly).

In `settings/ui.rs` (~line 178), after the `show_histogram` checkbox line, add:

```rust
    .checkbox(&mut settings.show_tool_palette, "Show tool palette")
```

(match the surrounding builder-call chaining; if it's a standalone `ui.checkbox(...)` add a sibling `ui.checkbox(&mut settings.show_tool_palette, "Show tool palette");`.)

- [ ] **Step 4: Add the menu action + title_bar param + item**

In `chrome/mod.rs`, add to `MenuAction` (after `ToggleHistogram,`):

```rust
    ToggleToolPalette,
```

Add a `show_tool_palette: bool` parameter to `title_bar(...)` (after `show_histogram: bool,`). In the View menu, after the existing `show_histogram` checkbox block (~line 222-230), add:

```rust
    let mut palette_checked = show_tool_palette;
    if ui
        .checkbox(&mut palette_checked, "Show tool palette")
        .clicked()
    {
        action = Some(MenuAction::ToggleToolPalette);
        ui.close_menu();
    }
```

- [ ] **Step 5: Wire the app handler + call site**

In `app.rs`, at the `title_bar(...)` call site (~line 2894-2905), add `self.state.settings.show_tool_palette,` as the new argument (in the same position as the new param).

Add the handler beside the `ToggleHistogram` arm (~line 2975-2978):

```rust
                Some(crate::chrome::MenuAction::ToggleToolPalette) => {
                    self.state.settings.show_tool_palette = !self.state.settings.show_tool_palette;
                    self.mark_settings_dirty();
                }
```

- [ ] **Step 6: Run to verify pass + gate**

Run: `cargo test -p ferrolite-app --lib settings::` — Expected: PASS.
Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/settings ferrolite-app/src/chrome/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): settings.show_tool_palette (default on) + View-menu toggle"
```

---

## Task 4: shared `tool_button` widget

**Files:**
- Create: `ferrolite-app/src/widgets/tool_button.rs`
- Modify: `ferrolite-app/src/widgets/mod.rs` (declare + `pub(crate) use`)
- Test: build + clippy (egui rendering; no golden — matches masking-UI discipline)

**Interfaces:**
- Produces: `pub(crate) fn tool_button(ui: &mut egui::Ui, icon: &str, tooltip: &str, active: bool, enabled: bool, disabled_reason: Option<&str>) -> egui::Response` — a square icon button, accent when `active`, faint when `!enabled` (with a hover reason), normal otherwise. Used by both the palette (tools) and the tab bar (Task 11 reuses it or a `tab_button` sibling for labels).

- [ ] **Step 1: Implement the widget**

Create `ferrolite-app/src/widgets/tool_button.rs`:

```rust
//! Shared Develop tool/tab button — consistent active/hover/disabled styling using
//! design-system tokens, so palette tools and panel tabs look identical everywhere.

use crate::theme;

/// A compact icon button. `active` → accent fill; `!enabled` → faint + a hover reason;
/// otherwise idle with a hover highlight. Returns the click `Response`.
pub(crate) fn tool_button(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    active: bool,
    enabled: bool,
    disabled_reason: Option<&str>,
) -> egui::Response {
    let size = egui::vec2(28.0, 28.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = resp.hovered() && enabled;
    let bg = if active {
        theme::ACCENT_BRIGHT
    } else if hovered {
        theme::BG_TOOLBAR
    } else {
        egui::Color32::TRANSPARENT
    };
    let fg = if !enabled {
        theme::TEXT_FAINT
    } else if active {
        theme::BG_BASE
    } else {
        theme::TEXT_DIM
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, bg);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(15.0),
        fg,
    );
    if enabled {
        resp = resp.on_hover_text(tooltip);
    } else if let Some(reason) = disabled_reason {
        resp = resp.on_hover_text(reason);
    }
    resp
}
```

- [ ] **Step 2: Declare + export**

In `ferrolite-app/src/widgets/mod.rs`, add:

```rust
mod tool_button;
pub(crate) use tool_button::tool_button;
```

Add `#[allow(dead_code)]` on the `tool_button` fn (removed at Task 13) OR rely on Task 10/11 consuming it this same branch — since it is consumed in Task 10, add the allow now and it is naturally exercised then; simplest is the scoped allow.

- [ ] **Step 3: Build + clippy**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/widgets/tool_button.rs ferrolite-app/src/widgets/mod.rs
git commit -m "feat(develop): shared tool_button widget (active/hover/disabled tokens)"
```

---

## Task 5: base adjustment tabs (Light · Color · Curve · Detail · Optics)

**Files:**
- Create: `ferrolite-app/src/develop/base_tabs.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod base_tabs;`)
- Modify: `ferrolite-app/src/develop/adjustment_panel.rs` (section bodies referenced/moved — keep `EditOutcome`/`PanelOutcome`/chrome helpers)
- Test: build + clippy (egui migration; per-control reset preserved via `EguiSlider`)

**Interfaces:**
- Consumes: `PanelTab`, `TabId`, `EditOutcome`, `AppState`.
- Produces: `LightTab`, `ColorTab`, `CurveTab`, `DetailTab`, `OpticsTab` (unit structs impl `PanelTab`) and `pub fn base_tabs() -> Vec<Box<dyn PanelTab>>` returning them in order. TabIds: `"light"`, `"color"`, `"curve"`, `"detail"`, `"optics"`.

**Mapping (from research) — each tab's `show()` body is the exact section body moved out of `adjustment_panel::show`:**
- `LightTab` ("Light") ← "Basic" section `adjustment_panel.rs:133-224` (Exposure/Contrast/Temp/Tint + section reset).
- `ColorTab` ("Color") ← "HSL" section `adjustment_panel.rs:234-240` (`hsl_widget::show(ui, &stack, &mut v.hsl_band)`).
- `CurveTab` ("Curve") ← "Tone Curve" section `adjustment_panel.rs:227-231` (`curve_widget::show(ui, &stack)`).
- `DetailTab` ("Detail") ← "Detail" section `adjustment_panel.rs:255-291` (Sharpen Amount/Radius).
- `OpticsTab` ("Optics") ← "Lens Corrections" section `adjustment_panel.rs:297-784` (distortion/TCA/vignette + nested Focal/Aperture Adjust + lens picker; moves the transient `v.lens_picker_open`/`v.lens_picker_query`/`v.lens_resolved_name`/`v.lens_vignette` access with it — all reachable via `state.viewer`).

- [ ] **Step 1: Scaffold the module + first tab (Light)**

Create `ferrolite-app/src/develop/base_tabs.rs`:

```rust
//! The always-present global adjustment tabs (design §7/§8). Each wraps an existing
//! `adjustment_panel` section body verbatim as a `PanelTab`; per-control reset is
//! preserved because each keeps its `EguiSlider` (the reset column is baked into the
//! widget). `base_tabs()` is registered once as the registry's base.

#![allow(dead_code)] // consumed by DevelopToolRegistry::standard() in Task 9; allow removed at Task 13

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::tool::{PanelTab, TabId};
use crate::state::AppState;
use crate::widgets::EguiSlider;

pub struct LightTab;
impl PanelTab for LightTab {
    fn id(&self) -> TabId { TabId("light") }
    fn label(&self) -> &str { "Light" }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        // MOVE the "Basic" section body from adjustment_panel.rs:133-224 here:
        // read `let stack = state.viewer.as_ref()?.op_stack.clone();`, render the
        // Exposure/Contrast/Temp/Tint EguiSliders + section "Reset" button exactly as
        // today, accumulating into a local `let mut out = None;`, and return `out`.
        // (No working_space here — that stays as panel chrome, Task 11.)
        let _ = (ui, state);
        None // replaced by the moved body
    }
}
```

Then MOVE the exact Basic-section code (Exposure/Contrast/Temp/Tint sliders + reset, `adjustment_panel.rs:133-224`) into `LightTab::show`, adapting only the state access: the section today runs inside `adjustment_panel::show(ui, state, working_space)` with `let stack = ...` computed at the top of that fn — replicate that read at the top of `show` (`let stack = state.viewer.as_ref()?.op_stack.clone();`), keep every `EguiSlider {..}` and the `EditOutcome { stack: .., kind: OpKind::Exposure/Contrast/WhiteBalance, commit: .. }` construction verbatim, and `return out;`.

- [ ] **Step 2: Add the remaining four tabs**

Add `ColorTab`, `CurveTab`, `DetailTab`, `OpticsTab` following the same pattern, each moving its mapped section body verbatim:

```rust
pub struct ColorTab;
impl PanelTab for ColorTab {
    fn id(&self) -> TabId { TabId("color") }
    fn label(&self) -> &str { "Color" }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        // MOVE the "HSL" section body (adjustment_panel.rs:234-240): needs `&stack`
        // and `&mut v.hsl_band` — pull both from state.viewer; call
        // `crate::develop::hsl_widget::show(ui, &stack, &mut v.hsl_band)` and return
        // its outcome.
        let _ = (ui, state);
        None
    }
}

pub struct CurveTab;
impl PanelTab for CurveTab {
    fn id(&self) -> TabId { TabId("curve") }
    fn label(&self) -> &str { "Curve" }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        // MOVE the "Tone Curve" section body (adjustment_panel.rs:227-231):
        // `crate::develop::curve_widget::show(ui, &stack)`.
        let _ = (ui, state);
        None
    }
}

pub struct DetailTab;
impl PanelTab for DetailTab {
    fn id(&self) -> TabId { TabId("detail") }
    fn label(&self) -> &str { "Detail" }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        // MOVE the "Detail" section body (adjustment_panel.rs:255-291): Sharpen
        // Amount/Radius EguiSliders + their EditOutcome { kind: OpKind::Sharpen, .. }.
        let _ = (ui, state);
        None
    }
}

pub struct OpticsTab;
impl PanelTab for OpticsTab {
    fn id(&self) -> TabId { TabId("optics") }
    fn label(&self) -> &str { "Optics" }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        // MOVE the "Lens Corrections" section body (adjustment_panel.rs:297-784),
        // INCLUDING the nested Focal/Aperture "Adjust" CollapsingHeaders and the
        // lens_picker::show modal + the transient state it mutates
        // (v.lens_picker_open / v.lens_picker_query / v.lens_resolved_name /
        // v.lens_vignette). All are reachable through state.viewer. Keep every
        // EguiSlider + EditOutcome { kind: OpKind::LensCorrection, .. } verbatim.
        let _ = (ui, state);
        None
    }
}

pub fn base_tabs() -> Vec<Box<dyn PanelTab>> {
    vec![
        Box::new(LightTab),
        Box::new(ColorTab),
        Box::new(CurveTab),
        Box::new(DetailTab),
        Box::new(OpticsTab),
    ]
}
```

Fill each `show` body by moving the mapped section code. Because these bodies still exist in `adjustment_panel::show` (which the app still calls until Task 11), **duplicate the code into the tabs now and delete it from `adjustment_panel::show` in Task 11/13** — do NOT delete from `adjustment_panel` yet (keeps the app compiling + behaving until the panel shell is wired). The `#![allow(dead_code)]` covers the not-yet-called tabs.

- [ ] **Step 3: Declare the module**

In `ferrolite-app/src/develop/mod.rs`: `pub mod base_tabs;`

- [ ] **Step 4: Build + clippy**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean.
Run: `cargo build -p ferrolite-app` — Expected: OK.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/develop/base_tabs.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(develop): base adjustment tabs (Light/Color/Curve/Detail/Optics) wrapping existing sections"
```

---

## Task 6: Crop tool

**Files:**
- Create: `ferrolite-app/src/develop/tools/mod.rs`, `ferrolite-app/src/develop/tools/crop.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod tools;`)
- Test: build + clippy

**Interfaces:**
- Produces: `pub struct CropTool;` impl `DevelopTool` (`id=Crop`, `icon="⌗"`, `label="Crop"`, `enabled` = viewer present, `tabs()` = `[CropTab]`, `canvas()` = wraps `crop_overlay::show`). `CropTab` (`TabId("crop")`) holds the Geometry controls (`adjustment_panel.rs:787-851` body).

- [ ] **Step 1: Create the tools module**

Create `ferrolite-app/src/develop/tools/mod.rs`:

```rust
//! Concrete Develop tools registered by `DevelopToolRegistry::standard()`. Each wraps
//! the existing overlay/panel functions so the migration is behavior-preserving.

#![allow(dead_code)] // consumed by standard() in Task 9; allow removed at Task 13

pub mod crop;
```

In `ferrolite-app/src/develop/mod.rs`: `pub mod tools;`

- [ ] **Step 2: Implement `CropTool` + `CropTab`**

Create `ferrolite-app/src/develop/tools/crop.rs`:

```rust
use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::tool::{DevelopCtx, DevelopTool, PanelTab, TabId, ToolId};
use crate::state::AppState;

pub struct CropTool;
impl DevelopTool for CropTool {
    fn id(&self) -> ToolId { ToolId::Crop }
    fn icon(&self) -> &'static str { "⌗" }
    fn label(&self) -> &'static str { "Crop" }
    fn enabled(&self, ctx: &DevelopCtx) -> bool { ctx.state.viewer.is_some() }
    fn tabs(&self) -> Vec<Box<dyn PanelTab>> { vec![Box::new(CropTab)] }
    fn canvas(&self, ui: &mut egui::Ui, image_rect: egui::Rect, state: &mut AppState) -> Option<EditOutcome> {
        // Wrap the existing crop overlay verbatim. Pre-extract what it needs from the
        // viewer (mirrors app.rs:3676-3703): the OpStack and the aspect dims.
        let (stack, dims) = {
            let v = state.viewer.as_ref()?;
            (v.op_stack.clone(), v.image_dims)
        };
        crate::develop::crop_overlay::show(ui, image_rect, &stack, dims)
    }
}

pub struct CropTab;
impl PanelTab for CropTab {
    fn id(&self) -> TabId { TabId("crop") }
    fn label(&self) -> &str { "Crop" }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        // MOVE the "Geometry" section body from adjustment_panel.rs:787-851 here
        // (Angle EguiSlider + Aspect combo + "Reset crop" button, producing
        // EditOutcome { kind: OpKind::Geometry, .. }). Read `let stack =
        // state.viewer.as_ref()?.op_stack.clone();`. Do NOT set crop_active here — the
        // app derives it from ToolState.active == Crop (Task 11).
        let _ = (ui, state);
        None // replaced by the moved Geometry body
    }
}
```

Confirm `v.image_dims` is the correct `(u32,u32)` field name the current crop call passes as `dims` (research: `app.rs:3698` passes `dims`); if the app derives `dims` differently (e.g. an aspect-corrected pair), replicate that exact derivation here and note it in the report.

- [ ] **Step 3: Build + clippy + commit**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean.

```bash
git add ferrolite-app/src/develop/tools/mod.rs ferrolite-app/src/develop/tools/crop.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(develop): Crop tool (crop_overlay canvas + Geometry temp tab)"
```

---

## Task 7: Mask tool + sub-tool strip

**Files:**
- Create: `ferrolite-app/src/develop/tools/mask.rs`
- Modify: `ferrolite-app/src/develop/tools/mod.rs` (`pub mod mask;`)
- Modify: `ferrolite-app/src/develop/mask_panel.rs` (sub-tool strip with icons — replace the text `selectable_label` row at `mask_panel.rs:130-142`)
- Test: build + clippy

**Interfaces:**
- Produces: `pub struct MaskTool;` impl `DevelopTool` (`id=Mask`, `icon="◯"`, `label="Mask"`, `enabled` = viewer present, `tabs()` = `[MaskTab]`, `canvas()` = wraps `mask_overlay::show`). `MaskTab` (`TabId("mask")`) = `mask_panel::show` (masks list + selected controls). The sub-tool strip lives inside `mask_panel::selected_section` (icon buttons via `tool_button`).

- [ ] **Step 1: Implement `MaskTool` + `MaskTab`**

Create `ferrolite-app/src/develop/tools/mask.rs`:

```rust
use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::tool::{DevelopCtx, DevelopTool, PanelTab, TabId, ToolId};
use crate::state::AppState;

pub struct MaskTool;
impl DevelopTool for MaskTool {
    fn id(&self) -> ToolId { ToolId::Mask }
    fn icon(&self) -> &'static str { "◯" }
    fn label(&self) -> &'static str { "Mask" }
    fn enabled(&self, ctx: &DevelopCtx) -> bool { ctx.state.viewer.is_some() }
    fn tabs(&self) -> Vec<Box<dyn PanelTab>> { vec![Box::new(MaskTab)] }
    fn canvas(&self, ui: &mut egui::Ui, image_rect: egui::Rect, state: &mut AppState) -> Option<EditOutcome> {
        // Wrap mask_overlay::show verbatim. Pre-extract the shared bits (mirrors
        // app.rs:3734-3744) so the &mut v.mask borrow is disjoint: clone the OpStack,
        // dims, the overlay texture handle, and the preview source out of the viewer
        // first (all cheap Arc/handle clones), then take &mut v.mask.
        // NOTE: the app must have already called rebuild_mask_overlay_if_needed(ctx)
        // this frame when the Mask tool is active (Task 11 keeps that glue), so
        // v.mask_overlay_tex is current.
        let (stack, dims, tex, preview_source) = {
            let v = state.viewer.as_ref()?;
            (
                v.op_stack.clone(),
                v.image_dims,
                v.mask_overlay_tex.clone(),
                v.preview_source.clone(),
            )
        };
        let v = state.viewer.as_mut()?;
        crate::develop::mask_overlay::show(
            ui,
            image_rect,
            &stack,
            &mut v.mask,
            tex.as_ref(),
            dims,
            preview_source.as_ref(),
        )
    }
}

pub struct MaskTab;
impl PanelTab for MaskTab {
    fn id(&self) -> TabId { TabId("mask") }
    fn label(&self) -> &str { "Mask" }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        // The Plan-4 masks list + selected-mask controls. Mirror the current call
        // (adjustment_panel.rs:244-252): pull the OpStack out, then &mut v.mask.
        let stack = state.viewer.as_ref()?.op_stack.clone();
        let v = state.viewer.as_mut()?;
        crate::develop::mask_panel::show(ui, &stack, &mut v.mask)
    }
}
```

Confirm the exact ViewerState field names `mask_overlay_tex` (the built `Option<egui::TextureHandle>`) and `preview_source` (`Option<Arc<LinearRgbaF32>>`) and `image_dims` against `viewer/mod.rs`; adjust `.clone()`/`.as_ref()` to match their real types (the current app extraction at `app.rs:3734` is the source of truth — replicate it) and note any adjustment in the report.

- [ ] **Step 2: Sub-tool strip with icons**

In `mask_panel.rs`, replace the text `selectable_label` tool row (`mask_panel.rs:130-142`) with an icon strip using `crate::widgets::tool_button`, preserving the exact `mask.tool` assignment behavior:

```rust
        ui.horizontal(|ui| {
            for (tool, icon, tip) in [
                (MaskTool::Brush, "🖌", "Brush"),
                (MaskTool::Linear, "▤", "Linear gradient"),
                (MaskTool::Radial, "◎", "Radial gradient"),
                (MaskTool::LumaRange, "◐", "Luminance range"),
                (MaskTool::ColorRange, "🎨", "Color range"),
            ] {
                if crate::widgets::tool_button(ui, icon, tip, mask.tool == tool, true, None).clicked() {
                    mask.tool = tool;
                }
            }
        });
```

(`MaskTool` here is `crate::develop::mask_ui::MaskTool` — keep the existing import in `mask_panel.rs`.)

- [ ] **Step 3: Declare + build + clippy + commit**

In `tools/mod.rs`: `pub mod mask;`

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean.

```bash
git add ferrolite-app/src/develop/tools/mask.rs ferrolite-app/src/develop/tools/mod.rs ferrolite-app/src/develop/mask_panel.rs
git commit -m "feat(develop): Mask tool (mask_overlay canvas + masks temp tab) + icon sub-tool strip"
```

---

## Task 8: Heal tool (inert)

**Files:**
- Create: `ferrolite-app/src/develop/tools/heal.rs`
- Modify: `ferrolite-app/src/develop/tools/mod.rs` (`pub mod heal;`)
- Test: build + clippy

**Interfaces:**
- Produces: `pub struct HealTool;` impl `DevelopTool` (`id=Heal`, `icon="🩹"`, `label="Heal"`, `enabled` = always `false`, no tabs, no canvas — inherits the default `tabs()`/`canvas()`).

- [ ] **Step 1: Implement**

Create `ferrolite-app/src/develop/tools/heal.rs`:

```rust
use crate::develop::tool::{DevelopCtx, DevelopTool, ToolId};

/// Inert P5 placeholder — registered but always disabled (greyed in the palette with
/// a "coming in P5" hover reason, supplied by the palette rendering).
pub struct HealTool;
impl DevelopTool for HealTool {
    fn id(&self) -> ToolId { ToolId::Heal }
    fn icon(&self) -> &'static str { "🩹" }
    fn label(&self) -> &'static str { "Heal (P5)" }
    fn enabled(&self, _ctx: &DevelopCtx) -> bool { false }
}
```

- [ ] **Step 2: Declare + build + clippy + commit**

In `tools/mod.rs`: `pub mod heal;`

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean.

```bash
git add ferrolite-app/src/develop/tools/heal.rs ferrolite-app/src/develop/tools/mod.rs
git commit -m "feat(develop): Heal tool (inert, always disabled — P5)"
```

---

## Task 9: `AdjustTool` + `DevelopToolRegistry::standard()` + `ToolState` on `ViewerState` + registry on `FerroliteApp`

**Files:**
- Create: `ferrolite-app/src/develop/tools/adjust.rs`
- Modify: `ferrolite-app/src/develop/tools/mod.rs` (`pub mod adjust;`)
- Modify: `ferrolite-app/src/develop/tool.rs` (add `impl DevelopToolRegistry { pub fn standard() -> Self }`)
- Modify: `ferrolite-app/src/viewer/mod.rs` (add `tool_state: ToolState` field + default)
- Modify: `ferrolite-app/src/app.rs` (`FerroliteApp` gains `tool_registry: DevelopToolRegistry`, built once in the constructor)
- Test: `#[cfg(test)] mod tests` in `tool.rs` for `standard()`

**Interfaces:**
- Produces: `pub struct AdjustTool;` (`id=Adjust`, `icon="🎚"`, `label="Adjust"`, `enabled` = viewer present, no tabs, no canvas — default state). `DevelopToolRegistry::standard()` = `new(base_tabs(), vec![AdjustTool, CropTool, MaskTool, HealTool])`. `ViewerState.tool_state: ToolState`. `FerroliteApp.tool_registry`.

- [ ] **Step 1: Implement `AdjustTool`**

Create `ferrolite-app/src/develop/tools/adjust.rs`:

```rust
use crate::develop::tool::{DevelopCtx, DevelopTool, ToolId};

/// The default no-canvas-tool state: base adjustment tabs only, no overlay. Selecting
/// it in the palette deselects any active tool.
pub struct AdjustTool;
impl DevelopTool for AdjustTool {
    fn id(&self) -> ToolId { ToolId::Adjust }
    fn icon(&self) -> &'static str { "🎚" }
    fn label(&self) -> &'static str { "Adjust" }
    fn enabled(&self, ctx: &DevelopCtx) -> bool { ctx.state.viewer.is_some() }
}
```

In `tools/mod.rs`: `pub mod adjust;`

- [ ] **Step 2: Add `standard()` + its test (write test first)**

Add to `tool.rs` test module:

```rust
    #[test]
    fn standard_registry_has_the_shipped_tools_in_order() {
        let reg = DevelopToolRegistry::standard();
        let ids: Vec<ToolId> = reg.tools().iter().map(|t| t.id()).collect();
        assert_eq!(ids, vec![ToolId::Adjust, ToolId::Crop, ToolId::Mask, ToolId::Heal]);
        assert_eq!(reg.base_tabs().len(), 5, "Light/Color/Curve/Detail/Optics");
        // Heal is the only always-disabled tool.
        let no_viewer = AppState::for_test(); // see note below
        let ctx = DevelopCtx { state: &no_viewer };
        assert!(!reg.get(ToolId::Heal).unwrap().enabled(&ctx));
    }
```

If `AppState` has no cheap test constructor, assert Heal's disabled-ness through a `DevelopCtx` built from a minimal state, OR — simpler and state-independent — drop the `enabled` line and instead assert `reg.get(ToolId::Heal).unwrap().tabs().is_empty()` (Heal has no tabs). Choose whichever avoids constructing a full `AppState`; note the choice in the report.

Run: `cargo test -p ferrolite-app --lib develop::tool` — Expected: FAIL (no `standard()`).

Add to `tool.rs`:

```rust
impl DevelopToolRegistry {
    /// The shipped tool set: Adjust (default), Crop, Mask, Heal (disabled).
    pub fn standard() -> Self {
        use crate::develop::tools::{adjust::AdjustTool, crop::CropTool, heal::HealTool, mask::MaskTool};
        Self::new(
            crate::develop::base_tabs::base_tabs(),
            vec![
                Box::new(AdjustTool),
                Box::new(CropTool),
                Box::new(MaskTool),
                Box::new(HealTool),
            ],
        )
    }
}
```

Run: `cargo test -p ferrolite-app --lib develop::tool` — Expected: PASS.

- [ ] **Step 3: Add `tool_state` to `ViewerState`**

In `viewer/mod.rs`, beside `pub mask: ...` (~line 199) add:

```rust
    /// Develop tool/tab selection state (design §5). Per-image, like `mask`/`hsl_band`.
    pub tool_state: crate::develop::tool_state::ToolState,
```

In the `ViewerState` constructor (~line 328, beside `mask: ...::default()`):

```rust
            tool_state: crate::develop::tool_state::ToolState::default(),
```

Remove the `#[allow(dead_code)]` on `ToolState` (Task 2) now that it is a live field.

- [ ] **Step 4: Store the registry on `FerroliteApp`**

In `app.rs`, add to the `FerroliteApp` struct (line 6-41):

```rust
    tool_registry: crate::develop::tool::DevelopToolRegistry,
```

In `FerroliteApp`'s constructor (the `new`/`default` that builds the struct), initialize it once:

```rust
            tool_registry: crate::develop::tool::DevelopToolRegistry::standard(),
```

Remove the `#![allow(dead_code)]` from `tool.rs` and `tools/mod.rs` and `base_tabs.rs`? NOT yet — the tabs/tools are still not *rendered* until Tasks 10-11. Keep those allows; only the `ToolState` field allow is removed here (it is now a live field). The registry field on `FerroliteApp` is read in Tasks 10-11; add `#[allow(dead_code)]` on the `tool_registry` field until then, removed at Task 13.

- [ ] **Step 5: Build + clippy + commit**

Run: `cargo test -p ferrolite-app --lib develop::tool` — Expected: PASS.
Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean.

```bash
git add ferrolite-app/src/develop/tools/adjust.rs ferrolite-app/src/develop/tools/mod.rs ferrolite-app/src/develop/tool.rs ferrolite-app/src/viewer/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): AdjustTool + registry standard() + ToolState on viewer + registry on app"
```

---

## Task 10: Floating tool palette wired into the canvas

**Files:**
- Create: `ferrolite-app/src/develop/tool_palette.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod tool_palette;`)
- Modify: `ferrolite-app/src/app.rs` (call the palette in the Develop `CentralPanel`, beside `draw_histogram_overlay`; handle its actions)
- Test: build + clippy + author visual test

**Interfaces:**
- Produces:
  - `pub enum PaletteAction { SelectTool(ToolId), Undo, Redo }`
  - `pub fn show(ui: &egui::Ui, reg: &DevelopToolRegistry, ts: ToolState, ctx: &DevelopCtx, can_undo: bool, can_redo: bool) -> Option<PaletteAction>` — an interactive `egui::Area` (`Order::Foreground`) anchored top-left of the canvas under the filmstrip, rendering `reg.tools()` as `tool_button`s (active = `ts.active`, enabled via `tool.enabled(ctx)`, Heal greyed with a "coming in P5" reason), a divider, then Undo/Redo buttons (disabled per `can_undo`/`can_redo`). Returns the action clicked this frame.

- [ ] **Step 1: Implement the palette**

Create `ferrolite-app/src/develop/tool_palette.rs`:

```rust
//! The floating Develop tool palette (design §6): a toggleable, interactive egui::Area
//! under the filmstrip holding the registered tools + undo/redo. Mirrors the histogram
//! overlay's Area pattern but is clickable. Plain vector drawing — cheap per frame.

use crate::develop::tool::{DevelopCtx, DevelopToolRegistry, ToolId};
use crate::develop::tool_state::ToolState;
use crate::widgets::tool_button;

pub enum PaletteAction {
    SelectTool(ToolId),
    Undo,
    Redo,
}

pub fn show(
    ui: &egui::Ui,
    reg: &DevelopToolRegistry,
    ts: ToolState,
    ctx: &DevelopCtx,
    can_undo: bool,
    can_redo: bool,
) -> Option<PaletteAction> {
    const MARGIN: f32 = 12.0;
    let canvas_rect = ui.min_rect();
    let pos = egui::pos2(canvas_rect.left() + MARGIN, canvas_rect.top() + MARGIN);
    let mut action = None;

    egui::Area::new(egui::Id::new("develop_tool_palette"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_black_alpha(160))
                .rounding(4.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for tool in reg.tools() {
                            let enabled = tool.enabled(ctx);
                            let reason = (!enabled).then_some("Coming in P5");
                            if tool_button(ui, tool.icon(), tool.label(), ts.active == tool.id(), enabled, reason)
                                .clicked()
                                && enabled
                            {
                                action = Some(PaletteAction::SelectTool(tool.id()));
                            }
                        }
                        ui.separator();
                        if tool_button(ui, "↶", "Undo", false, can_undo, None).clicked() && can_undo {
                            action = Some(PaletteAction::Undo);
                        }
                        if tool_button(ui, "↷", "Redo", false, can_redo, None).clicked() && can_redo {
                            action = Some(PaletteAction::Redo);
                        }
                    });
                });
        });
    action
}
```

In `develop/mod.rs`: `pub mod tool_palette;`

- [ ] **Step 2: Wire into the canvas**

In `app.rs`, in the Develop `CentralPanel` arm, beside `if self.state.settings.show_histogram { self.draw_histogram_overlay(ui); }` (~line 3670), add the palette. Because it needs `ts`, `can_undo`/`can_redo`, and returns an action the app applies, structure it as:

```rust
                if self.state.settings.show_tool_palette && self.state.viewer.is_some() {
                    let ts = self.state.viewer.as_ref().map(|v| v.tool_state).unwrap_or_default();
                    let can_undo = self.state.viewer.as_ref().map(|v| v.history.can_undo()).unwrap_or(false);
                    let can_redo = self.state.viewer.as_ref().map(|v| v.history.can_redo()).unwrap_or(false);
                    let ctx_ro = crate::develop::tool::DevelopCtx { state: &self.state };
                    let action = crate::develop::tool_palette::show(ui, &self.tool_registry, ts, &ctx_ro, can_undo, can_redo);
                    match action {
                        Some(crate::develop::tool_palette::PaletteAction::SelectTool(id)) => {
                            let enabled = self.tool_registry.get(id).map(|t| {
                                let c = crate::develop::tool::DevelopCtx { state: &self.state };
                                t.enabled(&c)
                            }).unwrap_or(false);
                            if let Some(v) = self.state.viewer.as_mut() {
                                v.tool_state.select_tool(id, enabled, &self.tool_registry);
                            }
                        }
                        Some(crate::develop::tool_palette::PaletteAction::Undo) => self.apply_undo_redo(ctx, frame, true),
                        Some(crate::develop::tool_palette::PaletteAction::Redo) => self.apply_undo_redo(ctx, frame, false),
                        None => {}
                    }
                }
```

Confirm `history.can_undo()`/`can_redo()` exist (research shows `can_undo`/`can_redo` are already computed for `title_bar` at `app.rs:2894` — reuse that exact source; if they're locals there, compute the same way here). Note the borrow: `ctx_ro` borrows `&self.state` only for the `show` call; the match re-borrows freshly — keep these in separate statements as written so the borrow checker is satisfied.

- [ ] **Step 3: Build + clippy**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/develop/tool_palette.rs ferrolite-app/src/develop/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): floating tool palette (tools + undo/redo) wired into canvas"
```

---

## Task 11: Tabbed right-panel shell + tool canvas dispatch; retire section triggers

**Files:**
- Create: `ferrolite-app/src/develop/tool_panel.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod tool_panel;`)
- Modify: `ferrolite-app/src/app.rs` (swap the `SidePanel::right("develop_adjust")` body to `tool_panel::show`; frame-start `crop_active`/`mask.active` from `tool_state`; dispatch the active tool's `canvas()`)
- Modify: `ferrolite-app/src/develop/adjustment_panel.rs` (delete the migrated section bodies + `show`'s section calls; keep the top chrome as a reusable helper + `EditOutcome`/`PanelOutcome`)
- Test: build + clippy + author visual test

**Interfaces:**
- Produces: `pub fn show(ui: &mut egui::Ui, state: &mut AppState, reg: &DevelopToolRegistry, working_space: WorkingSpace) -> PanelOutcome` — renders (1) global chrome (camera/coverage line, save-state, working-space combo) via a helper moved from `adjustment_panel`; (2) the tab bar from `ToolState::tab_bar(reg)` (base tabs + the active tool's temp tab, the temp tab visually separated), mutating `state.viewer.tool_state` on click; (3) the active tab's `show(ui, state)`. Returns `PanelOutcome { edit, working_space }`.

- [ ] **Step 1: Extract the panel chrome helper**

In `adjustment_panel.rs`, extract the three top chrome blocks (camera/coverage `61-87`, save-state `89-113`, working-space combo `117-130`) into a `pub(crate) fn chrome(ui: &mut egui::Ui, state: &mut AppState, working_space: WorkingSpace) -> Option<WorkingSpace>` (returns a working-space change). Keep `EditOutcome`/`PanelOutcome` defs in this file.

- [ ] **Step 2: Implement `tool_panel::show`**

Create `ferrolite-app/src/develop/tool_panel.rs`:

```rust
//! The Develop right-panel shell (design §7): global chrome + a shared tab bar (base
//! adjustment tabs ++ the active canvas tool's temporary tab) + active-tab dispatch.
//! Replaces the flat CollapsingHeader `adjustment_panel::show`.

use crate::develop::adjustment_panel::{EditOutcome, PanelOutcome};
use crate::develop::tool::DevelopToolRegistry;
use crate::state::AppState;
use ferrolite_pipeline::WorkingSpace;

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    reg: &DevelopToolRegistry,
    working_space: WorkingSpace,
) -> PanelOutcome {
    // 1) Global chrome (not tool-specific) stays above the tab bar.
    let ws_change = crate::develop::adjustment_panel::chrome(ui, state, working_space);
    ui.separator();

    // 2) Copy ToolState out (it is Copy) so the tab-bar mutation doesn't fight the
    //    &mut AppState borrow used by the active tab's show().
    let Some(mut ts) = state.viewer.as_ref().map(|v| v.tool_state) else {
        return PanelOutcome { edit: None, working_space: ws_change };
    };
    ts.ensure_valid_tab(reg);
    let bar = ts.tab_bar(reg);
    let base_len = reg.base_tabs().len();

    // Render the tab bar (base tabs, then a separator + the active tool's temp tab).
    ui.horizontal_wrapped(|ui| {
        for (i, id) in bar.iter().enumerate() {
            if i == base_len && i > 0 {
                ui.separator(); // visual break before the active tool's temp tab
            }
            let label = tab_label(reg, ts, *id);
            if ui.selectable_label(ts.active_tab == *id, label).clicked() {
                ts.select_tab(*id, reg);
            }
        }
    });
    ui.separator();

    // 3) Dispatch the active tab's show(). Look the tab object up fresh (base ++ active
    //    tool tabs) and call it.
    let active = ts.active_tab;
    let mut out: Option<EditOutcome> = None;
    // Base tabs:
    let mut rendered = false;
    for tab in reg.base_tabs() {
        if tab.id() == active {
            out = tab.show(ui, state);
            rendered = true;
            break;
        }
    }
    // Active tool's temp tabs:
    if !rendered && ts.active != crate::develop::tool::ToolId::Adjust {
        if let Some(tool) = reg.get(ts.active) {
            for tab in tool.tabs() {
                if tab.id() == active {
                    out = tab.show(ui, state);
                    break;
                }
            }
        }
    }

    // Write ToolState back.
    if let Some(v) = state.viewer.as_mut() {
        v.tool_state = ts;
    }
    PanelOutcome { edit: out, working_space: ws_change }
}

fn tab_label(reg: &DevelopToolRegistry, ts: crate::develop::tool_state::ToolState, id: crate::develop::tool::TabId) -> String {
    for tab in reg.base_tabs() {
        if tab.id() == id {
            return tab.label().to_string();
        }
    }
    if let Some(tool) = reg.get(ts.active) {
        for tab in tool.tabs() {
            if tab.id() == id {
                return tab.label().to_string();
            }
        }
    }
    id.0.to_string()
}
```

In `develop/mod.rs`: `pub mod tool_panel;`

- [ ] **Step 3: Swap the SidePanel body + frame-start derivation + canvas dispatch in `app.rs`**

Replace the `adjustment_panel::show(...)` call in `SidePanel::right("develop_adjust")` (`app.rs:3558-3564`) with:

```rust
                            outcome = Some(crate::develop::tool_panel::show(
                                ui,
                                &mut self.state,
                                &self.tool_registry,
                                working_space,
                            ));
```

(`self.tool_registry` and `&mut self.state` are both borrowed — since `tool_registry` and `state` are distinct fields of `FerroliteApp`, this is a disjoint borrow and compiles.)

Replace the frame-start reset (`app.rs:3537-3542`) so `crop_active`/`mask.active` derive from the tool state instead of being re-armed by open sections:

```rust
    if self.module == crate::module::Module::Develop && self.state.viewer.is_some() {
        if let Some(v) = self.state.viewer.as_mut() {
            let active = v.tool_state.active;
            v.crop_active = active == crate::develop::tool::ToolId::Crop;
            v.mask.active = active == crate::develop::tool::ToolId::Mask;
            v.mask.adjusting = false; // still reset each frame; panel sets it on a drag
        }
        // ... keep any following lines in this block
    }
```

In the Develop `CentralPanel`, replace the direct `crop_overlay::show`/`mask_overlay::show` calls (`app.rs:3676-3748`) with a single dispatch to the active tool's `canvas()`, keeping the mask-overlay rebuild glue:

```rust
                    // Active-tool canvas overlay. Keep the mask overlay's bounded
                    // rebuild glue (needs ctx + &mut self) here, before dispatch.
                    let active = self.state.viewer.as_ref().map(|v| v.tool_state.active);
                    if active == Some(crate::develop::tool::ToolId::Mask) {
                        self.rebuild_mask_overlay_if_needed(ctx);
                    }
                    if let Some(id) = active {
                        if let Some(tool) = self.tool_registry.get(id) {
                            // image_rect is the same rect the crop/mask overlays used
                            // (compute it exactly as the removed code did).
                            let image_rect = /* the existing image_rect expression */ ui.min_rect();
                            let out = tool.canvas(ui, image_rect, &mut self.state);
                            if let Some(o) = out {
                                self.apply_edit(ctx, frame, o.kind, o.stack, o.commit);
                            }
                        }
                    }
```

**Important:** use the exact `image_rect` expression the removed crop/mask overlay code used (research: both received `image_rect` — locate how the app computed it around `app.rs:3676-3744` and reuse verbatim; do NOT substitute `ui.min_rect()` if the real value differs). Note the exact expression in the report.

- [ ] **Step 4: Delete the migrated section bodies from `adjustment_panel::show`**

Remove `adjustment_panel::show` (and its now-duplicated section bodies for Basic/Tone/HSL/Masks/Detail/Lens/Geometry) — they now live in the base tabs (Task 5) + Crop/Mask tabs (Tasks 6/7). Keep in `adjustment_panel.rs`: `EditOutcome`, `PanelOutcome`, and the `chrome(...)` helper (Step 1). Update `develop/mod.rs`/imports as needed. This removes the section-based `crop_active = true`/`mask.active = true` triggers entirely.

- [ ] **Step 5: Build + clippy**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean (some scaffolding allows may now be removable; leave the final sweep to Task 13).
Run: `cargo build -p ferrolite-app` — Expected: OK.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/develop/tool_panel.rs ferrolite-app/src/develop/mod.rs ferrolite-app/src/app.rs ferrolite-app/src/develop/adjustment_panel.rs
git commit -m "feat(develop): tabbed right-panel shell + tool canvas dispatch; retire section-based crop/mask triggers"
```

---

## Task 12: Eyedropper — Pick-color arm + picker cursor + zoom loupe

**Files:**
- Modify: `ferrolite-app/src/develop/mask_ui.rs` (add `picking_color: bool` to `MaskUiState` + default `false`)
- Modify: `ferrolite-app/src/develop/mask_panel.rs` (add a "Pick color" toggle button in the Color sub-tool block, `mask_panel.rs:264-333`)
- Modify: `ferrolite-app/src/develop/mask_overlay.rs` (`route_color_eyedropper`: gate sampling on `picking_color`; draw picker cursor + zoom loupe)
- Test: build + clippy + author visual test (sampling math is the existing pure `sample_source` unit — unchanged)

**Interfaces:**
- Produces: `MaskUiState.picking_color: bool`; a "Pick color" toggle in the Color sub-tool panel; an armed pick mode where the canvas shows a picker cursor + a magnifying loupe circle following the pointer, and a click samples via the existing `mask_affordance::sample_source` (unchanged).

- [ ] **Step 1: Add the state field**

In `mask_ui.rs`, add to `MaskUiState` (after `color_samples: Vec<Rgb>,`):

```rust
    /// Armed color-pick mode (the Color sub-tool's "Pick color" toggle). While true,
    /// the canvas shows a picker cursor + zoom loupe and a click samples a pixel.
    pub picking_color: bool,
```

In `MaskUiState::default()`: `picking_color: false,`.

- [ ] **Step 2: Add the "Pick color" toggle in the panel**

In `mask_panel.rs`, in the `MaskTool::ColorRange` block (`mask_panel.rs:264-333`), before the Tolerance slider, add:

```rust
    let pick_label = if mask.picking_color { "Picking… (click image)" } else { "Pick color" };
    if ui.selectable_label(mask.picking_color, pick_label).clicked() {
        mask.picking_color = !mask.picking_color;
    }
```

Also disarm on "Add Color range": in the existing `if ui.add_enabled(can_add, ...).clicked()` block, after `mask.color_samples.clear();` add `mask.picking_color = false;`.

- [ ] **Step 3: Gate sampling + draw the loupe in `route_color_eyedropper`**

In `mask_overlay.rs`, modify `route_color_eyedropper` (`mask_overlay.rs:484-534`): only sample when `mask.picking_color`, and while armed draw a picker crosshair + a zoom-loupe circle (mirroring the brush cursor pattern at `mask_overlay.rs:414-425`, bounded egui vector drawing). Replace the body with:

```rust
    if !mask.picking_color {
        return; // sampling only while armed
    }
    let resp = ui.interact(
        image_rect,
        ui.id().with("mask_overlay_affordance"),
        egui::Sense::click(),
    );
    let hover = resp.hover_pos().or_else(|| resp.interact_pointer_pos());
    // Sample on click.
    if resp.clicked() {
        if let (Some(p), Some(src_img)) = (resp.interact_pointer_pos(), preview_source) {
            let geo = stack.geometry();
            let (src_w, src_h) = src_dims;
            let norm = (
                ((p.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0),
                ((p.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0),
            );
            let src_norm = display_to_source(geo, src_w, src_h, norm);
            let rgb = mask_affordance::sample_source(src_img, src_norm);
            mask.color_samples.push(rgb);
        }
    }
    // Picker cursor + zoom loupe while armed (bounded vector drawing).
    if let (Some(p), Some(src_img)) = (hover, preview_source) {
        let painter = ui.painter();
        const LOUPE_R: f32 = 44.0;
        const ZOOM: f32 = 8.0; // source px per loupe px
        let center = p - egui::vec2(0.0, LOUPE_R + 16.0); // float above the pointer
        // Sample a small grid around the pointer and paint magnified pixels.
        let geo = stack.geometry();
        let (src_w, src_h) = src_dims;
        let span = (LOUPE_R / ZOOM) as i32; // half-width in source px
        for dy in -span..=span {
            for dx in -span..=span {
                let n = (
                    ((p.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0)
                        + dx as f32 / src_w as f32,
                    ((p.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0)
                        + dy as f32 / src_h as f32,
                );
                let sn = display_to_source(geo, src_w, src_h, (n.0.clamp(0.0, 1.0), n.1.clamp(0.0, 1.0)));
                let rgb = mask_affordance::sample_source(src_img, sn);
                let cell = egui::Rect::from_center_size(
                    center + egui::vec2(dx as f32 * ZOOM, dy as f32 * ZOOM),
                    egui::vec2(ZOOM, ZOOM),
                );
                painter.rect_filled(
                    cell,
                    0.0,
                    egui::Color32::from_rgb(
                        (rgb.r.clamp(0.0, 1.0) * 255.0) as u8,
                        (rgb.g.clamp(0.0, 1.0) * 255.0) as u8,
                        (rgb.b.clamp(0.0, 1.0) * 255.0) as u8,
                    ),
                );
            }
        }
        painter.circle_stroke(center, LOUPE_R, egui::Stroke::new(1.5, theme::ACCENT_BRIGHT));
        // Crosshair on the exact sampled (center) pixel.
        painter.line_segment([center - egui::vec2(6.0, 0.0), center + egui::vec2(6.0, 0.0)], egui::Stroke::new(1.0, theme::BG_BASE));
        painter.line_segment([center - egui::vec2(0.0, 6.0), center + egui::vec2(0.0, 6.0)], egui::Stroke::new(1.0, theme::BG_BASE));
        // Picker dot at the pointer itself.
        painter.circle_stroke(p, 3.0, egui::Stroke::new(1.5, theme::ACCENT_BRIGHT));
    }
    // Keep the existing accumulated-swatch strip (mask_overlay.rs ~511-533) below.
```

Keep the existing accumulated-swatch drawing after this. If `display_to_source`, `preview_source`, `src_dims`, `stack`, `mask_affordance` are already the fn's params/imports (they are — see the current signature), no new imports beyond `theme` (already used in the file). Verify and adjust in the report if a helper name differs.

- [ ] **Step 4: Build + clippy + commit**

Run: `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: clean.

```bash
git add ferrolite-app/src/develop/mask_ui.rs ferrolite-app/src/develop/mask_panel.rs ferrolite-app/src/develop/mask_overlay.rs
git commit -m "feat(develop): color eyedropper Pick-color arm + picker cursor + zoom loupe"
```

---

## Task 13: Remove scaffolding allows + cleanup

**Files:**
- Modify: `tool.rs`, `tool_state.rs`, `base_tabs.rs`, `tools/mod.rs`, `widgets/mod.rs`/`tool_button.rs`, `app.rs` (remove every `#[allow(dead_code)]`/`#![allow(dead_code)]` added by this plan; remove any now-dead code left in `adjustment_panel.rs`)
- Test: full workspace gate

- [ ] **Step 1: Remove all scaffolding allows**

Delete every `#[allow(dead_code)]` / `#![allow(dead_code)]` this plan added (Tasks 1, 2, 4, 5, 6, 9 — the `tool.rs` crate-level allow, `ToolState`/`tool_button`/`base_tabs`/`tools` allows, and the `FerroliteApp.tool_registry` field allow). If clippy then reports a genuinely-unused item, that item is dead — remove it (do not re-add an allow); everything this plan created is consumed by the wiring in Tasks 10-11.

- [ ] **Step 2: Confirm `adjustment_panel` has no dead code**

Ensure `adjustment_panel.rs` retains only `EditOutcome`, `PanelOutcome`, and `chrome(...)` (all consumed by `tool_panel`); delete any leftover section helpers no longer referenced.

- [ ] **Step 3: Full workspace gate**

Run: `cargo fmt --all --check` — Expected: clean (run `cargo fmt --all` + re-commit if not).
Run: `cargo clippy --workspace --all-targets -- -D warnings` — Expected: clean, NO `allow(dead_code)` remaining from this plan.
Run: `cargo test --workspace` — Expected: all green (the new `develop::tool` + `develop::tool_state` unit tests pass; all existing masking/pipeline/golden tests unchanged).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore(develop): remove tool-registry scaffolding allows; confirm no dead code"
```

---

## Task 14: Gate + author visual test hand-off

**Files:** none (verification only).

- [ ] **Step 1: Full gate (repeat, authoritative)**

Run: `cargo fmt --all --check` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo test --workspace`
Expected: all green.

- [ ] **Step 2: STOP and hand over the visual test plan**

Per CLAUDE.md, the gate is necessary but not sufficient — this is almost entirely egui UI. Hand Jann this numbered checklist and hold:

1. **Palette toggle** — View menu → "Show tool palette" off/on; palette (under the filmstrip, top-left of the canvas) hides/shows. Default is on. Failure: menu item missing, or palette doesn't toggle.
2. **Tool switching** — click Adjust/Crop/Mask in the palette. Adjust = base tabs only, no overlay; Crop = crop overlay on canvas + a Crop tab appears (auto-selected) after the base tabs; Mask = mask overlay + a Mask tab. Deselecting a tool (click Adjust) removes the temp tab and restores the last base tab. Failure: overlay/tab doesn't match the active tool, or the global adjustment tabs disappear.
3. **Heal greyed** — Heal is visibly faint and unclickable with a "Coming in P5" hover reason.
4. **Undo/redo from palette** — make an edit, click ↶/↷ in the palette; matches menu undo/redo; buttons grey when nothing to undo/redo.
5. **No behavior loss (regression)** — Light (Exposure/Contrast/Temp/Tint), Color (HSL), Curve, Detail (Sharpen), Optics (Lens: distortion/TCA/vignette + lens picker) all edit exactly as before; **every slider still has its per-control reset arrow**; Crop (angle/aspect/reset) and the full Plan-4 masking (create mask, brush/linear/radial/luma/color-range, overlay toggle, Light/Color per-mask, undo/redo, persist-on-reopen) behave as before.
6. **Eyedropper loupe** — in Mask → Color sub-tool, click "Pick color": the canvas cursor shows a picker + a zoom-loupe circle magnifying the pixel under the pointer with a crosshair on the exact sampled pixel; clicking samples it into the swatches; pick mode stays armed until toggled off or "Add Color range" is pressed. Failure: no loupe, loupe doesn't track/magnify, or clicking doesn't sample.
7. **Sub-tool icons** — the mask sub-tool strip shows icon buttons (Brush/Linear/Radial/Luma/Color) with the active one highlighted.
8. **No freeze** — switching tools, dragging sliders, and painting stay responsive (no multi-second stalls) — the palette/tab bar add no heavy per-frame work.

Address anything the author finds, then finish per CLAUDE.md (do not merge/PR on your own).

---

## Self-Review

**1. Spec coverage:**
- §4 API (traits + registry) → Task 1 (+ `standard()` Task 9). §5 pure `ToolState` → Task 2. §6 palette + `show_tool_palette` → Tasks 3 (setting/menu) + 10 (palette). §7 tabbed panel (base ++ temp tab, chrome above) → Task 11. §8 migration: base tabs → Task 5; Crop → Task 6; Mask + sub-tool strip → Task 7; Heal → Task 8; eyedropper loupe → Task 12. §9 visual consistency (`tool_button`, EguiSlider reset preserved) → Tasks 4/5-7. §10 edge cases (no viewer, disabled tool, stale tab, palette hidden) → Task 2 (`select_tool` enabled-gate, `ensure_valid_tab`) + Task 10 (viewer-gated palette) + Task 11 (chrome/empty-state). §11 testing (pure ToolState + registry unit tests; egui via build/clippy/visual) → Tasks 1/2/9 + Task 14. §12 decomposition (single plan, dependency order) → this plan. §13 decisions honored (Model A temp tab; floating palette; code-only registry; undo/redo in palette; wrap-existing migration; loupe eyedropper; presentation-only). 
- Regression guard (masking pure/golden + Adjust unchanged) → Tasks wrap existing fns verbatim; Task 13/14 gate re-runs the whole suite.

**2. Placeholder scan:** Migration tasks (5/6/11) say "MOVE the exact body from `file:line-range`" rather than re-pasting 100–500 lines — this is a precise, unambiguous instruction for a code MOVE of a known block (re-pasting risks transcription drift), with the wrapper signature + the state-access adaptation shown in full. Novel code (traits, `ToolState`, palette, panel shell, `tool_button`, loupe) is given complete. `image_rect`/`dims`/`can_undo` derivations are flagged "use the exact existing expression" with the source site named — the implementer copies the real expression rather than a guess.

**3. Type consistency:** `EditOutcome`/`PanelOutcome` (from `adjustment_panel`) used identically across tabs/tools/panel. `ToolState` (Copy) is read-copied in Tasks 10/11 and written back — consistent. `select_tool(id, enabled, reg)` signature used identically in Task 2 tests and Task 10 wiring. `DevelopCtx { state }` built identically in palette + registry `enabled` checks. `TabId` string ids (`"light"/"color"/"curve"/"detail"/"optics"/"crop"/"mask"`) are consistent between `base_tabs()`/tool `tabs()` and `ToolState` defaults. `tool_button` signature consistent between Task 4 def and Tasks 7/10 uses.

**Open items the implementer must confirm against the live code (flagged in-task, report any adjustment):** exact `ViewerState` field names/types for `image_dims`, `mask_overlay_tex`, `preview_source`; the exact `image_rect` expression in the Develop `CentralPanel`; `history.can_undo()/can_redo()` accessors; the `settings/persist.rs` tuple form; whether `AppState` has a test constructor for the `standard()` Heal-enabled assertion (fallback assertion provided).
