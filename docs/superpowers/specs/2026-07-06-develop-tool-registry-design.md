# ferrolite — Develop tool registry, floating palette & tabbed panel (design)

> **Status:** Design — approved by user (2026-07-06); pending writing-plans.
> **Date:** 2026-07-06
> **Context:** `docs/design/ferrolite-design-system.md` (Develop module; 296px right panel;
> tokens/widgets), the existing Develop UI (`ferrolite-app/src/develop/adjustment_panel.rs`
> flat `CollapsingHeader` sections; `crop_overlay.rs`; the P1 masking UI in `mask_panel.rs`/
> `mask_overlay.rs`), and the P1 masking specs (`2026-07-05-p1-masking-design.md`).
> **Builds on / stacks after:** the P1 masking work (`feat/p1-mask-plan4-ui`) — the palette's
> **Mask** tool wraps the Plan-4 masking UI. This refactor is Develop-shell-wide and is
> developed on its own branch (`feat/develop-tool-registry`), **not** merged into the masking
> branch.
> **Proves:** a standardized, code-level tool/tab interface so new Develop tools and panel tabs
> are cheap to add; a toggleable floating tool palette (histogram-style) under the filmstrip; and
> a contextual tabbed right panel driven by the active tool — with the existing Adjust/Crop/Mask
> behavior migrated with no loss and the mask eyedropper made discoverable.

---

## 1. Goal & validation

Replace the ad-hoc, section-driven tool entry in Develop (crop via the open Geometry section,
masks via the open Masks section) with **one standardized tool system**:

> The Develop view keeps the **filmstrip on top**. A **toggleable floating tool palette**
> (default on, shown/hidden like the histogram) sits under the filmstrip over the canvas and
> holds the registered tools (**Adjust · Crop · Mask · Heal**, Heal greyed) plus **undo/redo**.
> The right panel **always** shows the global **adjustment tabs** (Light · Color · Curve · Detail ·
> Optics). Selecting a canvas tool activates its **overlay/affordances** and **injects a temporary,
> auto-selected tab** for that tool's options — removed again when the tool is deselected, so the
> global adjustments never disappear behind a tool. Adding a new tool or a new tab is a small,
> well-defined code change against a trait + registry — the user never creates tabs; the set is
> whatever is registered. The existing Adjust sliders, crop overlay, and masking UI all move under this system
> with no behavior loss, and the mask color-range **eyedropper** becomes a first-class, discoverable
> sub-tool.

**Success = the running app:** the palette toggles on/off; clicking a tool switches the overlay +
panel tabs; undo/redo work from the palette; Adjust/Crop/Mask behave exactly as before the refactor
(same edits, same per-control reset, same overlay); Heal is visibly greyed; and a developer can add
a trivial demo tool/tab in a few lines to prove the API. Automated gate green, then the author's
hands-on visual test (CLAUDE.md).

---

## 2. Scope

**In:**
- **`DevelopTool` + `PanelTab` traits + a `DevelopToolRegistry`** (the extensibility API): a tool
  declares its id/icon/label/enabled + its canvas overlay hook + its panel tabs; a tab declares its
  id/label + its `show`. Registration is code-only.
- **Tool/tab selection state** (pure, testable): active tool, per-tool active tab, enabled/greyed
  resolution, palette visibility — as egui-free logic.
- **The floating tool palette**: an interactive, toggleable `egui::Area` overlay under the
  filmstrip holding the registered tools + undo/redo, mirroring the histogram overlay's
  show/hide (`settings.show_tool_palette`, default on) and build-once discipline.
- **The tabbed right panel**: a shared tab-bar widget rendering the always-present **base
  adjustment tabs**, plus (while a canvas tool is active) that tool's **temporary auto-selected
  tab**; then the active tab's `show()`.
- **Migration** of the existing UI onto the system with no behavior change: the global **base tabs**
  (today's Basic/Tone/HSL/Detail/Lens sections → always-present tabs; **Adjust** = the default
  no-canvas-tool state), a **Crop** tool (Geometry controls as a temporary tab + the existing
  `crop_overlay`), a **Mask** tool (the Plan-4 masks list + selected-mask controls as a temporary
  tab + a **sub-tool strip** with the eyedropper), and an inert **Heal** tool (greyed, P5).
- **Shared visual widgets/tokens** for the tab bar + tool buttons (active/hover/disabled), reusing
  `EguiSlider` + `draw_reset_arrow` unchanged.

**Out (non-goals / later):**
- No new *editing* features — this is a shell/interaction refactor. No new adjustment ops, no new
  mask components, no Heal implementation (P5).
- No change to the render pipeline, the OpStack model, persistence, or `EditOutcome`/`apply_edit`.
- No user-facing tab customization (reordering/creating tabs) — the registry is developer-only.
- No re-theming beyond the tab-bar/tool-button consistency this introduces.
- Before/After + zoom controls MAY later join the palette via the same registry, but are **not** in
  this scope (only undo/redo are added now); they stay where they are today.

---

## 3. Architecture of the slice

```
ferrolite-app / Develop view (app.rs)
  TopBottomPanel::top("develop_filmstrip")          [unchanged — filmstrip stays on top]
  CentralPanel (canvas)
    ├── egui::Area "develop_tool_palette" (Order::Foreground, toggleable)   [NEW]
    │     renders registry.tools() as tool buttons + undo/redo; sets active ToolId
    ├── active_tool.canvas(ui, image_rect, state) -> Option<EditOutcome>    [overlay/affordances]
    └── egui::Area "develop_histogram_overlay"        [unchanged]
  SidePanel::right("develop_adjust")
    └── tab_bar(base_tabs ++ active_tool.tabs()) + active_tab.show(ui, state) -> Option<EditOutcome>
        [NEW shell]  base tabs always present; active canvas tool appends a temporary auto-selected tab

  develop/tool.rs         DevelopTool + PanelTab traits, ToolId/TabId, DevelopToolRegistry + base_tabs()  [NEW]
  develop/tool_state.rs   pure selection state (active tool, per-tool active tab, palette vis) [NEW]
  develop/tool_palette.rs the floating palette egui rendering (tools + undo/redo)             [NEW]
  develop/tool_panel.rs   the right-panel tab-bar shell + active-tab dispatch                 [NEW]
  develop/tools/          the concrete tools: adjust.rs, crop.rs, mask.rs, heal.rs            [NEW]
```

**Every tool's edit still flows through the existing `apply_edit(kind, stack, commit)` path** — the
tools produce the same `EditOutcome` the current panel/overlay produce. `OpStack`, history (incl.
the Plan-4 per-gesture sealing), persistence, and the render tiers are untouched. This is purely a
reorganization of *presentation + input routing* behind a registry.

---

## 4. The API: `DevelopTool` + `PanelTab` (the extensibility surface)

```rust
// develop/tool.rs
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolId { Adjust, Crop, Mask, Heal }   // extend by adding a variant + a tool

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TabId(pub &'static str);            // stable id per tab (e.g. TabId("light"))

/// Read-only context a tool needs to decide enabled-state, render, and route input.
/// Concrete shape TBD in the plan; carries at least &AppState (viewer, op_stack, settings)
/// and the image dims — everything the current panel/overlay already read.
pub struct DevelopCtx<'a> { /* &AppState + derived read-only fields */ }

/// One control group in the right panel.
pub trait PanelTab {
    fn id(&self) -> TabId;
    fn label(&self) -> &str;
    /// Render this tab's controls; return an op edit if one was made this frame.
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome>;
}

/// The always-present global adjustment tabs, shown regardless of which canvas tool
/// is active (Light · Color · Curve · Detail · Optics). Registered once, not owned
/// by any tool. This is the "base" of the tab bar.
pub fn base_tabs() -> Vec<Box<dyn PanelTab>>;

/// A Develop canvas tool = a palette entry + its overlay + the TEMPORARY tab(s) it
/// injects while active. Selecting the tool appends its `tabs()` to the base tabs
/// (auto-selecting the first) and activates its `canvas`; deselecting removes them.
pub trait DevelopTool {
    fn id(&self) -> ToolId;
    fn icon(&self) -> &'static str;               // palette glyph/icon key
    fn label(&self) -> &'static str;              // tooltip
    fn enabled(&self, ctx: &DevelopCtx) -> bool;  // Heal -> false (P5); others -> true
    /// Temporary tab(s) injected into the tab bar (after the base tabs) while this
    /// tool is active — usually one (e.g. Crop, Mask). Empty for a tool with no
    /// panel options. NOT the global adjustments (those are `base_tabs`).
    fn tabs(&self) -> Vec<Box<dyn PanelTab>>;
    /// Optional canvas overlay + affordances while this tool is active. `None` = no
    /// overlay. Returns an op edit if a canvas gesture produced one this frame.
    fn canvas(
        &self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        state: &mut AppState,
    ) -> Option<EditOutcome> { let _ = (ui, image_rect, state); None }
}

/// The registry: base tabs + the ordered set of canvas tools shown in the palette.
/// `ToolId::Adjust` is the DEFAULT no-canvas-tool state (base tabs only, no overlay);
/// the palette's Adjust button deselects any active tool. Built once.
pub struct DevelopToolRegistry { tools: Vec<Box<dyn DevelopTool>> }
impl DevelopToolRegistry {
    pub fn standard() -> Self;                    // Adjust, Crop, Mask, Heal — the shipped set
    pub fn tools(&self) -> &[Box<dyn DevelopTool>];
    pub fn get(&self, id: ToolId) -> Option<&dyn DevelopTool>;
}
```

**Adding a tool** = implement `DevelopTool` + push it in `standard()`. **Adding a tab** = implement
`PanelTab` + include it either in `base_tabs()` (an always-present global tab) or in a tool's
`tabs()` (a temporary tab shown only while that tool is active). No other surface changes.
`EditOutcome`, `OpKind`, `apply_edit` are reused verbatim.

> Note: `PanelTab::show`/`DevelopTool::canvas` take `&mut AppState` (matching today's
> `adjustment_panel::show`/`crop_overlay::show`/`mask_overlay::show` signatures) so migrated code
> moves with minimal reshaping. The plan may narrow this to a smaller borrow if it proves clean;
> the trait shape is the contract, the exact param type is a plan detail.

---

## 5. Tool/tab selection state (pure, testable)

A small egui-free state object (per-viewer, like `mask`/`hsl_band`) holds the interaction state:

```rust
// develop/tool_state.rs
pub struct ToolState {
    pub active: ToolId,               // default Adjust (= no canvas tool; base tabs only)
    pub active_tab: TabId,            // the currently-selected tab (base or the temporary tool tab)
    pub base_tab: TabId,             // remembered base tab, restored when a tool is deselected
    pub palette_visible: bool,        // mirrors settings.show_tool_palette default-on
}
impl ToolState {
    /// Select a canvas tool: ignores disabled tools; on a real tool, AUTO-SELECTS that
    /// tool's first temporary tab; selecting `Adjust` deselects any tool and restores
    /// `base_tab`.
    pub fn select_tool(&mut self, id: ToolId, reg: &DevelopToolRegistry);
    /// Select a tab by id. If it is a base tab, also remember it as `base_tab`.
    pub fn select_tab(&mut self, tab: TabId, reg: &DevelopToolRegistry);
    /// The ordered tab bar for the current state: `base_tabs()` ++ (if a tool is active)
    /// that tool's `tabs()`.
    pub fn tab_bar(&self, reg: &DevelopToolRegistry) -> Vec<TabId>;
}
```

Unit-tested: default active = Adjust with a base tab selected; selecting a disabled tool (Heal) is a
no-op; selecting a real tool appends its temporary tab(s) and auto-selects the first; selecting
`Adjust` drops the temporary tab(s) and restores the remembered `base_tab`; `tab_bar` = base ++
active-tool tabs in order; `active_tab` clamps to a valid entry if a remembered tab is no longer
present (never a blank panel). This is the "hit-test/state math is pure" discipline the masking
overlays already follow (Spec 2 §8).

---

## 6. Floating tool palette

- An **interactive** `egui::Area` (`Order::Foreground`) anchored under the filmstrip (top-left of
  the canvas region), same overlay family as `draw_histogram_overlay`'s `Area` but clickable.
- Renders `registry.tools()` as tool buttons (icon + tooltip; active highlighted; disabled greyed
  with a hover reason for Heal), a divider, then **Undo / Redo** buttons wired to the existing
  `apply_undo_redo(undo)`.
- **Toggleable** via a new `settings.show_tool_palette` (default `true`), shown/hidden exactly like
  `settings.show_histogram` (View menu item + optional shortcut). When hidden, tools can still be
  switched via keyboard shortcuts (optional, plan detail) — the active tool persists.
- Build-once discipline: no per-frame allocation of pipelines; the palette is plain egui vector
  drawing (like `crop_overlay`), cheap per frame.

---

## 7. Right panel = base tabs + a temporary tool tab

- `SidePanel::right("develop_adjust")` renders a shared **tab-bar widget** from
  `ToolState::tab_bar(reg)` = the always-present **base adjustment tabs** followed by (when a canvas
  tool is active) that tool's **temporary tab(s)**, visually distinguished (e.g. a leading separator
  or a subtle accent) so it reads as the active tool's options. Labels; active underlined; overflow
  wraps. It then calls the active tab's `show(ui, state)`, routing its `Option<EditOutcome>` into the
  existing panel outcome the app applies via `apply_edit`.
- Selecting a tool auto-selects its temporary tab; **deselecting the tool removes the temporary tab**
  and restores the last base tab — the global adjustments are never hidden behind a tool.
- Panel chrome that is not tool-specific (camera/coverage status line, save-state indicator,
  working-space combo) stays at the top of the panel above the tab bar (it's global, not per-tool).

---

## 8. Migration (no behavior loss) + the eyedropper fix

- **Base adjustment tabs** (`develop/base_tabs.rs`, `base_tabs()`): today's `adjustment_panel`
  sections become the always-present `PanelTab`s. Initial grouping starts minimal (e.g. **Light** =
  Exposure/Contrast/WB, **Curve** = Tone Curve, **Color** = HSL, **Detail** = Sharpen, **Optics** =
  Lens Corrections) — the exact split is not load-bearing and can be re-grouped freely later since
  each is just a registered `PanelTab`. Each slider keeps its `EguiSlider` reset column. **Adjust**
  is the default no-canvas-tool state: base tabs only, no overlay.
- **Crop tool** (`tools/crop.rs`): its `canvas()` is the existing `crop_overlay::show`; it injects a
  single temporary **Crop** tab holding the Geometry controls (angle/aspect/reset). This replaces
  the `crop_active`-via-open-section trigger — `crop_active` becomes "the Crop tool is active."
- **Mask tool** (`tools/mask.rs`): its `canvas()` is `mask_overlay::show`; it injects a temporary
  **Mask** tab = the Plan-4 masks list + the selected mask's controls (`mask_panel`). Within that
  tab, a **sub-tool strip** (Brush · Linear · Radial · Luma · **Color**) selects `MaskUiState.tool`
  with proper icons. `mask.active` becomes "the Mask tool is active."
  - **Eyedropper loupe UX** (folds in the author's request; supersedes the earlier bare-crosshair
    note): the color-range sub-tool exposes a **Pick color** button (in the Color sub-tool's panel
    UI). Pressing it enters an armed **pick mode** (`MaskUiState.picking_color`); while armed the
    canvas cursor becomes a **picker/eyedropper icon** and a **zoom-loupe circle** follows the
    pointer, magnifying the region under it (from the preview source) with a crosshair on the exact
    sampled pixel, so the user sees precisely which pixel they select. Clicking samples that pixel
    (via the existing pure `mask_affordance::sample_source`) into `color_samples`; pick mode stays
    armed for multiple picks until toggled off or "Add Color range" is pressed. The loupe render is
    egui vector drawing over the canvas (bounded, like the brush cursor); the pixel-sampling math is
    the existing pure unit. This makes the color mask obvious to use, not just discoverable.
- **Heal tool** (`tools/heal.rs`): registered, `enabled=false`, no tabs, no canvas — a greyed
  palette button with a "coming in P5" hover reason.

Because each tool wraps the *existing* `show`/overlay functions, the migration is mechanical and the
render/edit behavior is unchanged; the masking golden/pure tests and the Adjust behavior are
untouched.

---

## 9. Visual consistency

- Shared `tool_button` (icon, active/hover/disabled states) and `tab_bar` widgets with unified
  design-system tokens (accent for active, dim for idle, faint+hover-reason for disabled), so tools
  and tabs look identical everywhere and future additions are automatically consistent.
- Reuse `EguiSlider` + `draw_reset_arrow` unchanged — **per-control reset stays load-bearing** for
  every migrated slider.
- The palette + tab bar match the histogram overlay's visual weight (subtle elevated panel) so the
  canvas stays the focus.

---

## 10. Error handling / edge cases

- **No image open / no viewer:** the palette + panel render disabled/empty gracefully (mirror the
  current `state.viewer.is_none()` early-returns).
- **Disabled tool selected programmatically:** `select_tool` ignores disabled ids (Heal can never
  become active).
- **Stale remembered tab:** `tab_for` falls back to the tool's first registered tab if the
  remembered `TabId` is no longer present (e.g. after a code change) — never panics/blank-panels.
- **Palette hidden:** the active tool + its overlay/panel still function; only the floating switcher
  is hidden.
- **Nothing slow on the UI thread (CLAUDE.md §1):** palette + tab bar are plain egui drawing; no
  per-frame heavy work introduced. The mask overlay's existing bounded rebuild is unchanged.

---

## 11. Testing

**Pure CPU logic (unit-tested, the 80%+ target):**
- `ToolState`: default active = Adjust; `select_tool` ignores disabled; `select_tab`/`tab_for`
  remember per tool + clamp to a valid tab; palette-visibility toggle.
- `DevelopToolRegistry::standard()`: contains the expected tools in order; `get(id)` resolves;
  Heal reports `enabled=false`; each enabled tool returns a non-empty (or intentionally-empty for
  Heal) tab set with stable `TabId`s.
- Any pure hit-test/state helpers added for the sub-tool strip reuse the existing pure
  `mask_affordance` units (unchanged).

**egui rendering** (palette, tab bar, tool buttons, migrated tabs/overlays): `cargo build` + clippy
+ the author's hands-on visual test. No egui golden tests (matches the masking-UI discipline).

**Regression guard:** the existing masking pure/golden tests and the Adjust behavior must be
unchanged — migration wraps the existing `show`/overlay fns without altering them.

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → then STOP and hold for the author's visual test (CLAUDE.md).

---

## 12. Decomposition into implementation plans

Likely a **single plan** (this is one cohesive shell refactor), executed in dependency order:

1. `tool.rs` (traits + `ToolId`/`TabId` + registry skeleton) + `tool_state.rs` (pure state) with
   unit tests — no UI wired yet.
2. `tool_palette.rs` (floating toggleable palette + undo/redo + `settings.show_tool_palette`) wired
   into the canvas `Area`, driving `ToolState.active`.
3. `tool_panel.rs` (tab-bar shell) wired into `SidePanel::right`, rendering base tabs ++ the active
   tool's temporary tab (auto-selected).
4. Migrate the **base tabs** (sections → always-present tabs; Adjust = default state), **Crop**
   (Geometry temp tab + crop overlay), **Mask** (masks list + controls temp tab + sub-tool strip +
   the **eyedropper Pick-color button / picker cursor / zoom loupe**), **Heal** (inert) onto the registry.
5. Shared `tool_button`/`tab_bar` widgets + visual-consistency pass; retire the old
   section/`crop_active`/`mask.active`-via-section triggers in favor of the tool system.
6. Gate green + author visual test.

If step 4 proves large, the migration MAY split into per-tool plans; the writing-plans step decides
final task granularity.

---

## 13. Decisions recorded (resolved during brainstorming, 2026-07-06)

| Question | Decision | Rationale |
|---|---|---|
| Toolbar vs tabs responsibility | **Model A, refined** — the panel always shows the base **adjustment tabs**; selecting a canvas tool injects a **temporary, auto-selected tab** for its options (removed on deselect) | Contextual + scalable, but the global adjustments never disappear behind a tool, and each tool has a clean home for its options |
| Where tools live | **Toggleable floating palette (histogram-style) under the top filmstrip**, holding tools + undo/redo | User preference; filmstrip stays on top; reuses the proven overlay `Area` pattern; keeps the canvas clean |
| Palette visibility | **Toggleable, default on** (`settings.show_tool_palette`, like `show_histogram`) | Consistent with the existing histogram toggle; canvas can be cleared when desired |
| Extensibility surface | **Code-level `DevelopTool` + `PanelTab` traits + registry**; users cannot create tabs | A clean developer API to add tools/tabs cheaply; the user-facing set stays curated |
| Tab count/grouping | **Deferred** — start minimal, grow; not load-bearing | Real grouping only clear once more tools exist; the registry makes re-grouping trivial |
| Undo/redo placement | **In the floating palette** (divider after the tools) | User preference; keeps global edit actions with the tool switcher |
| Migration | **Wrap the existing Adjust sections / crop overlay / masking UI as tools+tabs** — no behavior change | Mechanical, low-risk; preserves all edits, per-control reset, overlays, and tests |
| Eyedropper UX | **A "Pick color" button that arms pick mode → picker cursor + a zoom-loupe circle magnifying the pixel under the pointer**, in the Mask tool's Color sub-tool | Bare discoverability isn't enough — the loupe shows the exact pixel being sampled, making the color mask obvious and precise to use (author request); sampling reuses the existing pure `sample_source` unit |
| Branch | **Own branch `feat/develop-tool-registry`, stacked on the masking work; not merged into the mask branch** | Develop-shell-wide, orthogonal to masking; the mask branch is 1 of 5 P1 plans and stays focused |
| Scope | **Presentation/interaction refactor only** — no new edits, no pipeline/OpStack/persistence changes | Keeps the refactor safe and reviewable; render + document model untouched |
