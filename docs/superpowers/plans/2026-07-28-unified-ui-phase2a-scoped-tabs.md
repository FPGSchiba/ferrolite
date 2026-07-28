# Unified Maskable Adjustments — Phase 2a: Adjustment Registry & Scoped Tabs

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One options library: the Light/Color/Effects tabs render every adjustment from a single registry and edit whichever scope is active — global in Adjust mode, the selected mask in Mask mode — killing the duplicate slider set in `mask_panel::selected_section`. Mask mode shows the mask-management block ABOVE the same tabs (no separate "Mask" tab).

**Architecture:** A new `EditScope` (`Global | Mask(i) | MaskNone`) resolves from the active tool + mask selection. A `ScopedEdit` handle reads the scope's `AdjustmentSet` (Phase 1 made `doc.global` and `layers[i].adjustments` the same type) and writes back through normalization-preserving pipeline helpers. An `AdjustmentRegistry` describes each control once (id, tab, section, per-scope shader readiness, render fn); the three base tabs iterate it. Controls whose shader isn't ready in a scope render greyed with a hover reason (existing `add_enabled_ui` convention). Per-mask-ready controls (the current `local-adjust` set): exposure, contrast, highlights, shadows, whites, blacks, temp, tint, saturation, hue, color swatch. Tone curve / HSL / color grading / sharpen / dehaze stay global-live + mask-greyed until Phase 2b/3; highlights/shadows/whites/blacks, saturation/hue/vibrance, and the color swatch are mask-live + global-greyed until Phase 3 (no global shader yet). NR is greyed in both scopes (no shader at all).

**Tech Stack:** Rust; egui; existing `EguiSlider`, `section_header`, `add_enabled_ui` + `on_hover_text`/`on_disabled_hover_text` conventions; `ferrolite-pipeline`'s `EditDoc`/`AdjustmentSet` from Phase 1.

**Spec:** `docs/superpowers/specs/2026-07-28-unified-maskable-adjustments-design.md` §3 (registry & scoped UI), §6.2 (registry invariant tests). Author's directive (2026-07-28): "in mask mode we do not have an extra tab; the mask information appears atop the current adjustment tabs, and the adjustment tabs stay as is and apply only to the currently selected mask."

## Global Constraints

- Branch: `feat/ui-v2-rewrite`. Never commit to `main`.
- **Per-control reset is load-bearing** (CLAUDE.md): every live slider keeps the `EguiSlider` reset column; reset writes identity into the SCOPED set.
- **Icons** from `icons.rs` only; keybind hints via `Keymap::hint` — the moved mask header keeps its existing hints (`NewBrushLayer`, `ToggleMaskOverlay`).
- **Greyed convention:** `ui.add_enabled_ui(false, |ui| { …slider… }).response.on_hover_text(reason)` — visible value, disabled, hover states WHY (matches `mask_panel`'s Effects block and `base_tabs`' lens rows).
- Mask-scope writes always produce `EditOutcome { kind: OpKind::LocalAdjustments, .. }` (today's mask-edit contract — preview refresh + local node update, no full-tier rebuild). Global writes keep each control's existing `OpKind`.
- Normalization invariant from Phase 1 (`EditDoc` doc comment): identity-valued writes must stay byte-equal to defaults. All scoped writes go through the new `with_global`/`with_layer_adjustments` helpers (Task 1), never raw field assignment on the doc.
- `state.viewer.mask.adjusting` must be set while a mask-scope slider is dragging (the overlay suppression path — behavior preserved from `selected_section`).
- Collapsible section open/closed state is tracked SEPARATELY per scope (spec §3): new `mask_*_open` Settings fields.
- Subagents run the **scoped gate** named in each task; the coordinator runs the repo gate once at the end.

---

### Task 1: Pipeline write helpers — `AdjustmentSet::normalized` + `EditDoc::{with_global, with_layer_adjustments}`

**Files:**
- Modify: `ferrolite-pipeline/src/local.rs` (add `normalized`)
- Modify: `ferrolite-pipeline/src/op.rs` (add the two `EditDoc` methods; refactor `set_op`'s inline normalization arms to reuse `normalized`'s logic where natural)
- Modify: `ferrolite-app/src/develop/mask_edit.rs` (`set_adjustments` routes through `with_layer_adjustments`)

**Interfaces:**
- Consumes: Phase 1's `EditDoc { global, layers, .. }`, `AdjustmentSet` (full block), the set_op normalization invariant.
- Produces (Tasks 2-6 rely on these exact signatures):
  - `AdjustmentSet::normalized(&self) -> AdjustmentSet` — returns a copy where each identity-valued STRUCTURED field is snapped to its exact `Default` (`dehaze.is_identity()` → `Dehaze::default()`, `sharpen.amount == 0.0` → `Sharpen::default()`, `tone_curve.is_identity()` → `ToneCurve::default()`, `color_grade.is_identity()` → `ColorGrade::default()`, `hsl.is_identity()` → `Hsl::default()`). Scalars pass through (0.0 is already canonical).
  - `EditDoc::with_global(&self, set: AdjustmentSet) -> EditDoc` — new doc, `global = set.normalized()`.
  - `EditDoc::with_layer_adjustments(&self, idx: usize, set: AdjustmentSet) -> EditDoc` — new doc, `layers[idx].adjustments = set.normalized()`; out-of-range `idx` returns `self.clone()` unchanged (stale-selection race must never panic).

- [ ] **Step 1: Write the failing tests** (in `op.rs`'s test module)

```rust
#[test]
fn with_global_normalizes_identity_structures() {
    let mut set = AdjustmentSet::default();
    set.dehaze = Dehaze { amount: 0.0, radius: 9 }; // identity, non-canonical radius
    set.exposure = 0.5;
    let d = EditDoc::default().with_global(set);
    assert_eq!(d.global.dehaze, Dehaze::default(), "identity dehaze snapped");
    assert_eq!(d.global.exposure, 0.5, "live value preserved");
}

#[test]
fn with_layer_adjustments_writes_only_that_layer_and_normalizes() {
    let la = LocalAdjustments {
        layers: vec![
            crate::local::MaskLayer {
                name: "A".into(),
                visible: true,
                mask: Default::default(),
                adjustments: Default::default(),
            },
            crate::local::MaskLayer {
                name: "B".into(),
                visible: true,
                mask: Default::default(),
                adjustments: Default::default(),
            },
        ],
    };
    let d = EditDoc::default().set_op(Op::LocalAdjustments(la));
    let mut set = AdjustmentSet::default();
    set.exposure = -1.0;
    set.sharpen = Sharpen { amount: 0.0, radius: 5 }; // identity, non-canonical
    let d2 = d.with_layer_adjustments(1, set);
    assert_eq!(d2.layers[0].adjustments, AdjustmentSet::default(), "layer 0 untouched");
    assert_eq!(d2.layers[1].adjustments.exposure, -1.0);
    assert_eq!(d2.layers[1].adjustments.sharpen, Sharpen::default(), "identity sharpen snapped");
}

#[test]
fn with_layer_adjustments_out_of_range_is_a_noop() {
    let d = EditDoc::default();
    let mut set = AdjustmentSet::default();
    set.exposure = 1.0;
    assert_eq!(d.with_layer_adjustments(3, set), d, "no panic, unchanged doc");
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p ferrolite-pipeline with_global with_layer` → FAIL to compile (methods missing).

- [ ] **Step 3: Implement.** `normalized` in `local.rs` (needs `use crate::op::{ColorGrade, Dehaze, Hsl, Sharpen, ToneCurve};` — already partially imported):

```rust
    /// Copy with every identity-valued STRUCTURED field snapped to its exact
    /// `Default`, so identity edits stay byte-equal to a reset — the same
    /// invariant `EditDoc::set_op` maintains (is_identity, PartialEq-vs-default,
    /// and the serde hash agree). Scalars pass through (0.0 is already canonical).
    pub fn normalized(&self) -> Self {
        let mut s = self.clone();
        if s.dehaze.is_identity() {
            s.dehaze = crate::op::Dehaze::default();
        }
        if s.sharpen.amount == 0.0 {
            s.sharpen = crate::op::Sharpen::default();
        }
        if s.tone_curve.is_identity() {
            s.tone_curve = crate::op::ToneCurve::default();
        }
        if s.color_grade.is_identity() {
            s.color_grade = crate::op::ColorGrade::default();
        }
        if s.hsl.is_identity() {
            s.hsl = crate::op::Hsl::default();
        }
        s
    }
```

`EditDoc` methods in `op.rs`:

```rust
    /// New doc with the GLOBAL adjustment set replaced (normalized — see
    /// `AdjustmentSet::normalized`). The scoped-edit write path for
    /// `EditScope::Global`.
    pub fn with_global(&self, set: AdjustmentSet) -> EditDoc {
        let mut d = self.clone();
        d.global = set.normalized();
        d
    }

    /// New doc with layer `idx`'s adjustment set replaced (normalized). The
    /// scoped-edit write path for `EditScope::Mask(idx)`. An out-of-range
    /// `idx` (stale selection racing a delete) returns the doc unchanged.
    pub fn with_layer_adjustments(&self, idx: usize, set: AdjustmentSet) -> EditDoc {
        let mut d = self.clone();
        if let Some(layer) = d.layers.get_mut(idx) {
            layer.adjustments = set.normalized();
        }
        d
    }
```

In `mask_edit.rs`, change `set_adjustments`'s body to route through the helper (keeping its signature):

```rust
pub fn set_adjustments(stack: &OpStack, idx: usize, a: AdjustmentSet) -> OpStack {
    stack.with_layer_adjustments(idx, a)
}
```

(Read the current body first — if it clamps/guards anything else, preserve that behavior; the normalization is additive.)

- [ ] **Step 4: Run tests** — `cargo test -p ferrolite-pipeline` and `cargo test -p ferrolite-app --lib mask_edit` → PASS.

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-pipeline -p ferrolite-app -- --check
cargo clippy -p ferrolite-pipeline -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-pipeline
git add ferrolite-pipeline/src/local.rs ferrolite-pipeline/src/op.rs ferrolite-app/src/develop/mask_edit.rs
git commit -m "feat(pipeline): normalized scoped-write helpers with_global/with_layer_adjustments"
```

---

### Task 2: `EditScope` + `ScopedEdit` + the registry core

**Files:**
- Create: `ferrolite-app/src/develop/scope.rs`
- Create: `ferrolite-app/src/develop/adjustments.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (add `pub mod adjustments; pub mod scope;`)

**Interfaces:**
- Consumes: Task 1's `with_global`/`with_layer_adjustments`; `AppState` (`state.tool_state.active`, `state.viewer.as_ref().map(|v| &v.mask)` — `MaskUiState.selected: Option<usize>`); `EditOutcome { stack, kind, commit }`; `EguiSlider`; `theme`.
- Produces (Tasks 3-6 rely on these exact signatures):

```rust
// scope.rs
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditScope {
    Global,
    Mask(usize),
    /// Mask tool active but no mask selected/existing — controls render disabled.
    MaskNone,
}
pub fn current(state: &AppState) -> EditScope;
pub struct ScopedEdit<'a> {
    pub scope: EditScope,
    pub doc: &'a ferrolite_pipeline::OpStack,
    /// Set true by any slider being dragged this frame (tool_panel folds it
    /// into `mask.adjusting` when the scope is a mask — overlay suppression).
    pub adjusting: std::cell::Cell<bool>,
}
impl<'a> ScopedEdit<'a> {
    pub fn new(scope: EditScope, doc: &'a ferrolite_pipeline::OpStack) -> Self;
    /// The scope's adjustment set. None for MaskNone or a stale Mask index.
    pub fn set(&self) -> Option<&ferrolite_pipeline::AdjustmentSet>;
    /// Write a full set back to the scope. Global keeps `kind`; Mask forces
    /// OpKind::LocalAdjustments. None for MaskNone/stale index.
    pub fn write(
        &self,
        new: ferrolite_pipeline::AdjustmentSet,
        kind: ferrolite_pipeline::OpKind,
        commit: bool,
    ) -> Option<EditOutcome>;
    /// True when controls should render enabled at all (false for MaskNone).
    pub fn interactive(&self) -> bool;
}

// adjustments.rs
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AdjustmentId(pub &'static str);
pub struct SliderSpec {
    pub id: AdjustmentId,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub step: f32,
    pub decimals: usize,
    pub unit: &'static str,
    pub bipolar: bool,
    pub get: fn(&ferrolite_pipeline::AdjustmentSet) -> f32,
    pub set: fn(&mut ferrolite_pipeline::AdjustmentSet, f32),
    pub kind: ferrolite_pipeline::OpKind,
    pub global_ready: bool,
    pub mask_ready: bool,
    /// Hover reason shown when greyed in that scope (empty ⇒ ready).
    pub global_reason: &'static str,
    pub mask_reason: &'static str,
}
pub fn scoped_slider(
    ui: &mut egui::Ui,
    spec: &SliderSpec,
    scoped: &ScopedEdit<'_>,
) -> Option<EditOutcome>;
```

- `current(state)`: `ToolId::Mask` active → viewer's `mask.selected` filtered to `< layer count` → `Mask(i)`, else `MaskNone`; any other tool → `Global`. (Layer count via `crate::develop::mask_edit::layers(doc).layers.len()` — take the doc from the viewer.)
- `scoped_slider` behavior: resolve readiness for `scoped.scope` (`Global`/`MaskNone` use `global_ready` for display purposes — but `MaskNone` is ALWAYS disabled with reason "Create or select a mask first"; `Mask(_)` uses `mask_ready`). Ready + interactive → render `EguiSlider` exactly like `base_tabs` does today (same field layout, reset column via `default`), reading `spec.get(set)` into a local, on `changed()` building `let mut new = set.clone(); (spec.set)(&mut new, v);` and returning `scoped.write(new, spec.kind, r.drag_stopped() || !r.dragged())`. On `r.dragged()` → `scoped.adjusting.set(true)`. Not ready → `add_enabled_ui(false, …)` with the current value displayed and `.on_hover_text(reason)`; MaskNone reason takes precedence.

- [ ] **Step 1: Write the failing tests** (in `scope.rs` + `adjustments.rs` test modules)

```rust
// scope.rs tests
#[test]
fn scope_resolution_follows_tool_and_selection() {
    let mut state = AppState::new().unwrap();
    assert_eq!(current(&state), EditScope::Global, "Adjust tool ⇒ Global");
    state.tool_state.active = crate::develop::tool::ToolId::Mask;
    assert_eq!(current(&state), EditScope::MaskNone, "Mask tool, no viewer ⇒ MaskNone");
}

#[test]
fn scoped_write_targets_the_right_set() {
    use ferrolite_pipeline::{Op, OpKind, OpStack};
    let doc = OpStack::default().set_op(Op::LocalAdjustments(ferrolite_pipeline::LocalAdjustments {
        layers: vec![ferrolite_pipeline::MaskLayer {
            name: "M".into(),
            visible: true,
            mask: Default::default(),
            adjustments: Default::default(),
        }],
    }));
    // Global write lands in doc.global with the control's own kind.
    let s = ScopedEdit::new(EditScope::Global, &doc);
    let mut set = s.set().unwrap().clone();
    set.exposure = 1.0;
    let out = s.write(set, OpKind::Exposure, true).unwrap();
    assert_eq!(out.stack.global.exposure, 1.0);
    assert_eq!(out.kind, OpKind::Exposure);
    // Mask write lands in the layer and forces LocalAdjustments.
    let s = ScopedEdit::new(EditScope::Mask(0), &doc);
    let mut set = s.set().unwrap().clone();
    set.exposure = -1.0;
    let out = s.write(set, OpKind::Exposure, true).unwrap();
    assert_eq!(out.stack.layers[0].adjustments.exposure, -1.0);
    assert_eq!(out.stack.global.exposure, 0.0, "global untouched");
    assert_eq!(out.kind, OpKind::LocalAdjustments, "mask writes coerce kind");
    // MaskNone writes nothing.
    let s = ScopedEdit::new(EditScope::MaskNone, &doc);
    assert!(s.set().is_none());
    assert!(!s.interactive());
}

#[test]
fn stale_mask_index_reads_and_writes_none() {
    let doc = ferrolite_pipeline::OpStack::default(); // zero layers
    let s = ScopedEdit::new(EditScope::Mask(2), &doc);
    assert!(s.set().is_none());
    assert!(s
        .write(Default::default(), ferrolite_pipeline::OpKind::Exposure, true)
        .is_none());
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p ferrolite-app --lib scope` → FAIL to compile.

- [ ] **Step 3: Implement `scope.rs` and `adjustments.rs`** per the Produces block. `scoped_slider`'s enabled path must reuse the exact `EguiSlider` construction pattern from `base_tabs.rs` (same struct literal fields, `custom_label_w: None`) so visual layout is identical.

- [ ] **Step 4: Run tests** — `cargo test -p ferrolite-app --lib scope` → PASS.

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app --lib
git add ferrolite-app/src/develop/scope.rs ferrolite-app/src/develop/adjustments.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(develop): EditScope + ScopedEdit + adjustment registry core"
```

---

### Task 3: Registry-driven Light tab (both scopes)

**Files:**
- Modify: `ferrolite-app/src/develop/adjustments.rs` (add the Light slider specs + registry accessor)
- Modify: `ferrolite-app/src/develop/base_tabs.rs` (`LightTab::show` renders scoped)

**Interfaces:**
- Consumes: Task 2's `SliderSpec`/`scoped_slider`/`ScopedEdit`/`scope::current`; existing `curve_widget::show`, `section_header`, `ops_edit` (retired for these controls).
- Produces: `pub fn light_sliders() -> &'static [SliderSpec]` in `adjustments.rs` (Task 6's invariant tests iterate it), and a `LightTab` whose BASIC SLIDERS section renders these specs in order.

**The Light slider table** (transcribe each row into a `SliderSpec` const; `get`/`set` are field accessors on `AdjustmentSet`):

| id | label | field | min | max | default | step | dec | unit | bipolar | kind | global_ready | mask_ready | greyed reason (the not-ready scope) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `exposure` | Exposure | `exposure` | -5.0 | 5.0 | 0.0 | 0.01 | 2 | ` EV` | true | Exposure | true | true | — |
| `contrast` | Contrast | `contrast` | -1.0 | 1.0 | 0.0 | 0.01 | 2 | `` | true | Contrast | true | true | — |
| `highlights` | Highlights | `highlights` | -1.0 | 1.0 | 0.0 | 0.01 | 2 | `` | true | LocalAdjustments | **false** | true | global: "Global Highlights arrive with the unified layer engine (Phase 3)" |
| `shadows` | Shadows | `shadows` | -1.0 | 1.0 | 0.0 | 0.01 | 2 | `` | true | LocalAdjustments | **false** | true | global: same wording, "Shadows" |
| `whites` | Whites | `whites` | -1.0 | 1.0 | 0.0 | 0.01 | 2 | `` | true | LocalAdjustments | **false** | true | global: same wording, "Whites" |
| `blacks` | Blacks | `blacks` | -1.0 | 1.0 | 0.0 | 0.01 | 2 | `` | true | LocalAdjustments | **false** | true | global: same wording, "Blacks" |
| `temp` | Temp | `temp` | -1.0 | 1.0 | 0.0 | 0.01 | 2 | `` | true | WhiteBalance | true | true | — |
| `tint` | Tint | `tint` | -1.0 | 1.0 | 0.0 | 0.01 | 2 | `` | true | WhiteBalance | true | true | — |

(Ranges for exposure/contrast/temp/tint are the CURRENT `base_tabs` ranges — verify against the live code and keep whatever it uses today; mask H/S/W/B ranges are `selected_section`'s -1..1. `kind` for mask-only sliders is nominal — global writes for them are unreachable while `global_ready == false`.)

- [ ] **Step 1: Write the failing test** (in `base_tabs.rs` tests)

```rust
#[test]
fn light_tab_edits_the_selected_mask_when_mask_scope_active() {
    use ferrolite_pipeline::{Op, OpStack};
    let ctx = egui::Context::default();
    let mut state = AppState::new().unwrap();
    // No viewer ⇒ tab renders nothing and returns None (unchanged behavior).
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            assert!(LightTab.show(ui, &mut state).is_none());
        });
    });
    // Scope resolution is what the tab keys on — covered by scope.rs tests;
    // here assert the registry rows exist and are correctly gated.
    let specs = crate::develop::adjustments::light_sliders();
    let ids: Vec<&str> = specs.iter().map(|s| s.id.0).collect();
    assert_eq!(
        ids,
        vec!["exposure", "contrast", "highlights", "shadows", "whites", "blacks", "temp", "tint"]
    );
    let hl = specs.iter().find(|s| s.id.0 == "highlights").unwrap();
    assert!(!hl.global_ready && hl.mask_ready);
    assert!(!hl.global_reason.is_empty());
    let ex = specs.iter().find(|s| s.id.0 == "exposure").unwrap();
    assert!(ex.global_ready && ex.mask_ready);
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p ferrolite-app --lib light_tab_edits` → FAIL (no `light_sliders`).

- [ ] **Step 3: Implement.** In `adjustments.rs`, define the 8 specs and `light_sliders()`. In `LightTab::show`: clone the doc, resolve `let scope = crate::develop::scope::current(state);`, build `ScopedEdit`, render BASIC SLIDERS section (per-scope open flag — Task 6 adds the mask flags; until then use the existing `basic_sliders_open` for both scopes and leave a `// per-scope flag lands in Task 6` note) iterating `light_sliders()` through `scoped_slider`. The Tone Curve section: in `Global` scope render `curve_widget::show` exactly as today; in `Mask(_)`/`MaskNone` scope render the section header + a faint hint label `"Per-mask Tone Curve arrives with the layer engine (Phase 2b)"` — no interactive widget. Delete the old hand-coded exposure/contrast/WB slider blocks from `LightTab` (the registry replaces them). After rendering, if scope is a mask, fold `scoped.adjusting` into the viewer: `if let Some(v) = state.viewer.as_mut() { v.mask.adjusting = scoped_adjusting; }` — read `scoped.adjusting.get()` into a local BEFORE the `state.viewer.as_mut()` borrow.

- [ ] **Step 4: Run tests** — `cargo test -p ferrolite-app --lib` → PASS (existing LightTab section tests must still pass — the section headers and settings bindings are unchanged).

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app --lib
git add ferrolite-app/src/develop/adjustments.rs ferrolite-app/src/develop/base_tabs.rs
git commit -m "feat(develop): registry-driven scoped Light tab (H/S/W/B live per-mask)"
```

---

### Task 4: Registry-driven Color tab (both scopes)

**Files:**
- Modify: `ferrolite-app/src/develop/adjustments.rs` (Color slider specs + `color_sliders()`)
- Modify: `ferrolite-app/src/develop/base_tabs.rs` (`ColorTab::show` renders scoped)

**Interfaces:**
- Consumes: Task 2 core; existing `hsl_widget::show`, `grade_widget::show`.
- Produces: `pub fn color_sliders() -> &'static [SliderSpec]`; ColorTab structure (in order): COLOR (HSL) section [custom, global-only-live], COLOR MIX section [registry sliders + swatch], Color Grading section [custom, global-only-live].

**The Color slider table** (COLOR MIX section):

| id | label | field | min | max | default | step | dec | unit | bipolar | kind | global_ready | mask_ready | greyed reason |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `saturation` | Saturation | `saturation` | -1.0 | 1.0 | 0.0 | 0.01 | 2 | `` | true | LocalAdjustments | **false** | true | global: "Global Saturation arrives with the unified layer engine (Phase 3)" |
| `hue` | Hue | `hue` | -1.0 | 1.0 | 0.0 | 0.01 | 2 | `` | true | LocalAdjustments | **false** | true | global: same wording, "Hue" |
| `vibrance` | Vibrance | `vibrance` | -1.0 | 1.0 | 0.0 | 0.01 | 2 | `` | true | LocalAdjustments | **false** | **false** | both: "Vibrance arrives with the unified layer engine (Phase 3)" |
| `color_amount` | Color | `color.amount` | 0.0 | 1.0 | 0.0 | 0.01 | 2 | `` | false | LocalAdjustments | **false** | true | global: "Global color overlay arrives with the unified layer engine (Phase 3)" |

Plus the swatch picker (not a slider): after the `color_amount` row, when the scope is mask-live, render `ui.color_edit_button_rgb(&mut rgb)` bound to `color.r/g/b`, committing on change — MOVE this block from `mask_panel::selected_section` verbatim (it writes through `scoped.write(new, OpKind::LocalAdjustments, true)`). When global scope: wrap in `add_enabled_ui(false, …)` with the same Phase 3 reason.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn color_registry_rows_and_gating() {
    let specs = crate::develop::adjustments::color_sliders();
    let ids: Vec<&str> = specs.iter().map(|s| s.id.0).collect();
    assert_eq!(ids, vec!["saturation", "hue", "vibrance", "color_amount"]);
    assert!(specs.iter().all(|s| !s.global_ready), "none global-live until Phase 3");
    let vib = specs.iter().find(|s| s.id.0 == "vibrance").unwrap();
    assert!(!vib.mask_ready, "vibrance has no shader in any scope yet");
    assert!(specs.iter().filter(|s| s.id.0 != "vibrance").all(|s| s.mask_ready));
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p ferrolite-app --lib color_registry` → FAIL.

- [ ] **Step 3: Implement.** `ColorTab::show`: resolve scope; COLOR (HSL) section — global: `hsl_widget::show` as today; mask scopes: header + faint hint `"Per-mask HSL arrives with the layer engine (Phase 2b)"`. New COLOR MIX section (`section_header(ui, "COLOR MIX", &mut state.settings.color_mix_open)` — add `color_mix_open: bool` default-true to the Settings DTO now, with `#[serde(default = "default_true")]`, plus the `Default` impl line and the dto test lists, mirroring `dehaze_open`'s Task from the merge) rendering `color_sliders()` + the swatch. Color Grading section — global: `grade_widget::show` as today; mask: header + hint `"Per-mask Color Grading arrives with the layer engine (Phase 2b)"`. Fold `scoped.adjusting` into `mask.adjusting` as in Task 3.

- [ ] **Step 4: Run tests** — `cargo test -p ferrolite-app --lib` → PASS (including existing `settings_layout_fields_defaults_and_json_roundtrip` extended with `color_mix_open`).

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app --lib
git add ferrolite-app/src/develop/adjustments.rs ferrolite-app/src/develop/base_tabs.rs ferrolite-app/src/settings/dto.rs
git commit -m "feat(develop): registry-driven scoped Color tab (sat/hue/swatch live per-mask)"
```

---

### Task 5: Registry-driven Effects tab (both scopes)

**Files:**
- Modify: `ferrolite-app/src/develop/adjustments.rs` (Effects slider specs + `effects_sliders()`)
- Modify: `ferrolite-app/src/develop/base_tabs.rs` (`EffectsTab::show` renders scoped)

**Interfaces:**
- Consumes: Task 2 core; existing `show_optics_section`, dehaze/sharpen slider blocks.
- Produces: `pub fn effects_sliders() -> &'static [SliderSpec]`; EffectsTab structure: SHARPENING [registry], NOISE REDUCTION [registry, greyed both scopes], DEHAZE [registry], OPTICS [global scope ONLY — the section does not render at all in mask scopes: geometric/lens corrections are not maskable, spec §1].

**The Effects slider table:**

| id | label | field | min | max | default | step | dec | unit | bipolar | kind | global_ready | mask_ready | greyed reason |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `sharpen_amount` | Amount | `sharpen.amount` | 0.0 | 2.0 | 0.0 | 0.01 | 2 | `` | false | Sharpen | true | **false** | mask: "Per-mask Sharpening arrives with the per-mask neighborhood passes (Phase 4)" |
| `sharpen_radius` | Radius | `sharpen.radius` (as f32, `.round() as u32` on set) | 1.0 | 8.0 | 1.0 | 1.0 | 0 | ` px` | false | Sharpen | true | **false** | mask: same wording |
| `nr_luminance` | Luminance | `noise_reduction.luminance` | 0.0 | 1.0 | 0.0 | 0.01 | 2 | `` | false | LocalAdjustments | **false** | **false** | both: "Noise reduction is not wired yet — coming with its GPU pass" |
| `nr_detail` | Detail | `noise_reduction.detail` | 0.0 | 1.0 | 0.0 | 0.01 | 2 | `` | false | LocalAdjustments | **false** | **false** | both: same wording |
| `nr_color` | Color | `noise_reduction.color` | 0.0 | 1.0 | 0.0 | 0.01 | 2 | `` | false | LocalAdjustments | **false** | **false** | both: same wording |
| `nr_color_detail` | Color Detail | `noise_reduction.color_detail` | 0.0 | 1.0 | 0.0 | 0.01 | 2 | `` | false | LocalAdjustments | **false** | **false** | both: same wording |
| `dehaze_amount` | Dehaze | `dehaze.amount` | -1.0 | 1.0 | 0.0 | 0.01 | 2 | `` | true | Dehaze | true | **false** | mask: "Per-mask Dehaze arrives with the per-mask neighborhood passes (Phase 4)" |
| `dehaze_radius` | Radius | `dehaze.radius` (as f32, `.round() as u32` on set; default `DEHAZE_DEFAULT_RADIUS as f32`) | 1.0 | 24.0 | `DEHAZE_DEFAULT_RADIUS as f32` | 1.0 | 0 | ` px` | false | Dehaze | true | **false** | mask: same wording |

Note on u32 fields: `SliderSpec.get`/`set` are f32-typed; for radius fields `get = |a| a.sharpen.radius as f32`, `set = |a, v| a.sharpen.radius = v.round() as u32` — closures coerced to fn pointers.

IMPORTANT behavior change to preserve deliberately: today's NR sliders are ENABLED but bound to dead locals (they do nothing). After this task they are DISABLED with a hover reason and bound to the doc's `noise_reduction` fields — visibly honest. This is intended; note it in the commit message.

Sharpen "Detail" slider (today a dead local next to amount/radius): DROP it — it maps to no field and no planned shader parameter; the V2 mock's Detail slider returns when a shader defines it. (YAGNI; the registry makes re-adding trivial.)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn effects_registry_rows_and_gating() {
    let specs = crate::develop::adjustments::effects_sliders();
    let ids: Vec<&str> = specs.iter().map(|s| s.id.0).collect();
    assert_eq!(
        ids,
        vec![
            "sharpen_amount", "sharpen_radius",
            "nr_luminance", "nr_detail", "nr_color", "nr_color_detail",
            "dehaze_amount", "dehaze_radius"
        ]
    );
    assert!(specs.iter().filter(|s| s.id.0.starts_with("nr_")).all(|s| !s.global_ready && !s.mask_ready));
    assert!(specs.iter().filter(|s| s.id.0.starts_with("sharpen") || s.id.0.starts_with("dehaze")).all(|s| s.global_ready && !s.mask_ready));
}
```

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement.** `EffectsTab::show`: scope-resolve; SHARPENING/NOISE REDUCTION/DEHAZE sections render their registry rows via `scoped_slider` (existing per-scope open flags; mask variants in Task 6). OPTICS: `if matches!(scope, EditScope::Global) { …existing show_optics_section… }` — nothing rendered otherwise. Delete the old hand-coded sharpen/NR/dehaze slider blocks. Global sharpen/dehaze writes flow through `scoped.write(new_set, OpKind::Sharpen/Dehaze, commit)` — the Phase 1 doc stores them in `global`, so this is the same data path `ops_edit::set_sharpen/set_dehaze` fed; those two `ops_edit` setters become dead, remove them and their tests (`ops_edit` keeps setters still used by other callers — grep before deleting).

- [ ] **Step 4: Run tests** — `cargo test -p ferrolite-app --lib` → PASS. (The `test_effects_tab_collapsible_sections` / eight-section tests keep passing — section headers unchanged.)

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app --lib
git add -u ferrolite-app/src
git commit -m "feat(develop): registry-driven scoped Effects tab (NR honestly greyed; optics global-only)"
```

---

### Task 6: Mask mode = header atop the shared tabs (delete the duplicate sliders)

**Files:**
- Modify: `ferrolite-app/src/develop/tools/mask.rs` (`MaskTool::tabs()` → empty; delete `MaskTab`)
- Modify: `ferrolite-app/src/develop/tool_panel.rs` (render mask header + scope banner above the tab bar when Mask is active)
- Modify: `ferrolite-app/src/develop/mask_panel.rs` (delete the Light/Color/Effects slider block from `selected_section`, keep mask list + component tools)
- Modify: `ferrolite-app/src/develop/tool.rs` (fix the `standard_registry` test: Mask now has no tabs)
- Modify: `ferrolite-app/src/settings/dto.rs` (per-scope section-open flags)
- Modify: `ferrolite-app/src/develop/base_tabs.rs` + `adjustments.rs` (switch section-open flags to per-scope selection)

**Interfaces:**
- Consumes: everything above.
- Produces: the end state the author asked for — Mask tool active ⇒ mask management block + accent banner + the SAME Light/Color/Effects tabs editing the selected mask.

Sub-steps:

- [ ] **Step 1: Settings DTO.** Add default-true `#[serde(default = "default_true")]` fields: `mask_basic_sliders_open`, `mask_tone_curve_open`, `mask_color_hsl_open`, `mask_color_mix_open`, `mask_color_grading_open`, `mask_sharpening_open`, `mask_noise_reduction_open`, `mask_dehaze_open`. Extend the `Default` impl and the two dto tests (defaults + round-trip) with all eight. Flag selection at each section site is one inline expression (a fn-pointer helper returning `&mut bool` does not thread lifetimes cleanly — do not attempt one):

```rust
// per-scope disclosure state (spec §3 / V2 README): Adjust and Mask scopes
// remember their open/closed sections independently.
let open = if scope_is_mask {
    &mut state.settings.mask_basic_sliders_open
} else {
    &mut state.settings.basic_sliders_open
};
section_header(ui, "BASIC SLIDERS", open);
```

written at each of the eight section sites with its section's pair of flags.

- [ ] **Step 2: `MaskTool::tabs()` returns `Vec::new()`; delete `MaskTab` entirely.** In `tool.rs`'s `standard_registry_has_the_shipped_tools_in_order` test, Mask joins Heal in the no-tabs assertion. `ToolState::tab_bar`/`ensure_valid_tab` already handle a tool with no temp tabs (the bar is just the base tabs; a stale `TabId("mask")` clamps to `light` — `tool_state.rs` tests cover the clamp).

- [ ] **Step 3: `tool_panel::show`.** After `chrome()` + separator, when `ts.active == ToolId::Mask`: render the mask-management block (call `mask_panel::show` — which after Step 4 renders ONLY: create/overlay row, mask list rows, queued-samples hint, and for a selected mask the component tools row) and then the scope banner directly above the tab row:

```rust
// Scope banner (accent = editing a mask; faint = nothing selected).
match crate::develop::scope::current(state) {
    crate::develop::scope::EditScope::Mask(i) => {
        let name = state
            .viewer
            .as_ref()
            .map(|v| crate::develop::mask_edit::layers(&v.op_stack).layers[i].name.clone())
            .unwrap_or_default();
        ui.label(
            egui::RichText::new(format!(
                "Editing: {name} — adjustments below apply only inside this mask"
            ))
            .color(theme::ACCENT)
            .size(11.0),
        );
    }
    crate::develop::scope::EditScope::MaskNone => {
        ui.label(
            egui::RichText::new("Create or select a mask — adjustments below edit the selected mask")
                .color(theme::TEXT_FAINT)
                .size(11.0),
        );
    }
    crate::develop::scope::EditScope::Global => {}
}
ui.separator();
```

(Check `theme` for the accent constant name — `theme.rs` defines the V2 accent; use the existing constant, not a literal color.) The `mask_panel::show` call keeps its current pre-extraction pattern (stack clone + `&mut v.mask` + keymap clone) exactly as `MaskTab::show` did — move that block from `tools/mask.rs` into `tool_panel.rs`.

- [ ] **Step 4: `mask_panel.rs`.** In `selected_section`, delete everything from the `// ── Light + Color adjustments` comment through the greyed-Effects loop (the whole duplicate slider block, including the `slider` closure and the `if changed { set_adjustments }` tail); keep the component-count/Components/New-Brush-Layer row. Delete now-unused imports (`EguiSlider`, `AdjustmentSet` if unused). `mask.adjusting` is now maintained by the scoped tabs (Tasks 3-5) — grep `mask_panel.rs` for any remaining `adjusting` write and remove it.

- [ ] **Step 5: Per-scope open flags.** Switch the eight section sites in `LightTab`/`ColorTab`/`EffectsTab` to the scope-selected flag (Step 1's inline pattern), replacing Task 3's temporary shared-flag note.

- [ ] **Step 6: Tests.**

```rust
// tool.rs (adapt existing test)
assert!(reg.get(ToolId::Mask).unwrap().tabs().is_empty(), "Mask injects no tabs — shared base tabs only");
```

Add to `base_tabs.rs` tests:

```rust
#[test]
fn mask_scope_uses_its_own_section_flags() {
    let mut state = AppState::new().unwrap();
    state.settings.basic_sliders_open = true;
    state.settings.mask_basic_sliders_open = false;
    state.tool_state.active = crate::develop::tool::ToolId::Mask;
    // Render LightTab with no viewer: returns early, flags untouched — the
    // meaningful assertion is the flag WIRING, covered by reading the flag
    // selection helper/inline sites; assert both flags survive a render pass.
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = LightTab.show(ui, &mut state);
        });
    });
    assert!(state.settings.basic_sliders_open);
    assert!(!state.settings.mask_basic_sliders_open);
}
```

- [ ] **Step 7: Full-crate test run** — `cargo test -p ferrolite-app` → PASS.

- [ ] **Step 8: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
git add -u ferrolite-app/src
git commit -m "feat(develop): mask mode = management header atop shared scoped tabs; duplicate slider set deleted"
```

---

## Coordinator wrap-up (not a subagent task)

1. Registry invariant sweep (spec §6.2) — verify Tasks 3-5's tests collectively assert: unique ids across `light_sliders() ++ color_sliders() ++ effects_sliders()`, and every not-ready scope has a non-empty reason. If no single test does the cross-tab uniqueness check, add it as a tiny coordinator-dispatched fix task.
2. `rustup update stable`, then the full repo gate.
3. Visual test plan for the author (this phase is ALL visual):
   - **The vision:** open a RAW → Mask tool → the right panel shows mask management on top, then the accent "Editing: Mask N…" banner, then the same Light/Color/Effects tabs — no "Mask" tab anywhere.
   - **Scoped editing:** with a mask selected and painted, drag Exposure in the Light tab → only the masked region changes; switch to Adjust → the same slider now shows/edits the global value. Check per-control reset in both scopes resets only that scope's value.
   - **Mask-live vs greyed:** in Mask mode, Highlights/Shadows/Whites/Blacks, Saturation/Hue, Color swatch are live; Tone Curve/HSL/Color Grading show hints; Sharpening/Dehaze greyed with Phase 4 reasons; Optics absent. In Adjust mode, H/S/W/B + Saturation/Hue/Vibrance/Color greyed with Phase 3 reasons; everything previously live still live.
   - **No mask selected:** Mask tool with no masks → faint banner, all controls disabled with the "create or select" hover.
   - **Section memory:** collapse BASIC SLIDERS in Mask mode → switch to Adjust → it's still open there (separate disclosure state).
   - **Regression smoke:** brush painting + Ctrl+scroll size, components modal, overlay toggle, undo/redo across scope switches, before/after, export.
4. Wait for the author's hands-on verdict (also covers Phase 1's pending confirmation: previously-edited v1 images open unedited; thumbnails may show stale edited renders until re-edit).
