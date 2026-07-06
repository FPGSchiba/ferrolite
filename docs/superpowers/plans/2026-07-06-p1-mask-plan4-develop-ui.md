# P1 Masking — Plan 4: Develop unified Masking UI

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the Develop unified **Masking** tool: a right-panel Masks section (create / visibility / invert / rename / delete a `MaskLayer`; add a component with Brush / Linear / Radial / Luma-range / Color-range via Add/Subtract/Intersect; a per-mask Light+Color slider set with per-control reset and greyed neighborhood controls) plus a canvas colored mask overlay and tool affordances, all editing the Plan-3 `Op::LocalAdjustments` through the existing OpStack history.

**Architecture:** Mirror the shipped **crop-overlay discipline** (Spec 2 §8): every hit-test / handle-drag / threshold / overlay-color computation is a **pure, unit-tested function**; egui only routes pointer events and paints. The Masking tool is entered via a right-panel "Masks" `CollapsingHeader` that sets a `mask_active` flag driving a canvas overlay module — exactly like `crop_active` drives `crop_overlay`. The red coverage fill is composited on the GPU (reusing Plan 1–3's `ferrolite-mask` shape/brush/composite passes) at a bounded resolution, read back once per change, and painted as an egui texture. All edits flow through the existing `apply_edit`/`History`/`persist_ops` path; because every mask edit shares `OpKind::LocalAdjustments`, the history gains per-gesture sealing so a stroke and a later slider edit are distinct undo steps.

**Tech Stack:** Rust, egui (immediate-mode UI), `ferrolite_pipeline` (`Op::LocalAdjustments`, `LocalAdjustments`, `MaskLayer`, `AdjustmentSet`, `display_to_source`), `ferrolite_mask` (`MaskComponent`, `MaskDefinition`, `CompositeMode`, shape/brush/composite passes, `stroke_dabs`, `dab_alpha`, `composite_scalar`), the shared `EguiSlider` + `draw_reset_arrow` widgets.

## Global Constraints

- **Branch:** `feat/p1-mask-plan4-ui`, created off `feat/p1-mask-plan3-pipeline` (Plans 1–3 present). Do **not** merge/PR/finish — this is 1 of 5 plans; stop and report at the green gate, then hand the author a concrete visual test plan.
- **Per-control reset is mandatory** (CLAUDE.md): every editable mask control (each Light + Color slider) MUST expose its own reset via the shared `EguiSlider` reset column (`draw_reset_arrow`). Reserved neighborhood controls (Texture/Clarity/Dehaze/Sharpness/Noise) are shown **greyed with a hover reason**, not editable.
- **Nothing slow on the UI thread** (CLAUDE.md §1): no O(all-layers) or O(all-pixels) work per frame beyond the bounded overlay. The masks list is short (a handful of layers) so plain rendering is fine, but the overlay readback MUST be at a bounded resolution (`OVERLAY_MAX_EDGE`) and rebuilt only when its inputs change (mask definition / preview generation / toggle), never unconditionally per frame.
- **Pure-unit discipline** (Spec 2 §8): all hit-test / handle-drag / threshold / stroke-capture / overlay-color math lives in egui-free modules with `#[cfg(test)]` unit tests to **80%+**. egui code (panel rows, overlay painting, event routing) is verified by `cargo build` + `cargo clippy` + the author's visual test — **no egui golden tests**.
- **Unified tool** (§9.1): the Mask + Grad concepts fold into ONE Masking tool; linear/radial gradients are component *types*, not separate tools. **Heal stays absent** (P5).
- **Source-anchored masks** (§5.2): pointer input is in display space and MUST be inverse-mapped to normalized source coords via `ferrolite_pipeline::display_to_source`; stored mask params are normalized source coords. Handle placement maps source→display via `source_to_display` (Task 5). Overlay/handle geometry is exact under identity/crop-translation; rotation is a documented pragmatic limitation (parallels Plan 3's tile output-space note).
- **Undo/redo through Spec 2 history** (§9.4): masks live in the `OpStack`; a gesture (stroke, slider drag, discrete action) = exactly one history entry on commit. Mid-gesture frames apply live previews (`commit=false`, no history push).
- **Executor/engine shaders unchanged** where not required: Plan 4 adds a Rust `MaskCompositor` to `ferrolite-mask` (no WGSL changes) and reuses the existing GPU passes.
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` all green.

**Authoritative spec:** `docs/superpowers/specs/2026-07-05-p1-masking-design.md` (§9, §10, §12 plan 4; honor §13). Context: `docs/design/ferrolite-design-system.md` (Develop module, 296px right panel, tokens/widgets), `docs/superpowers/specs/2026-06-30-spec2-editing-design.md` §8 (crop-overlay discipline).

---

## File Structure

**Create (ferrolite-app):**
- `ferrolite-app/src/develop/mask_ui.rs` — `MaskTool` enum + `MaskUiState` (all per-image mask UI state: tool, selection, brush/range params, overlay toggle, in-progress gesture, overlay cache keys) + pure selection helpers. ~180 lines.
- `ferrolite-app/src/develop/mask_edit.rs` — pure `OpStack`/`LocalAdjustments` editing helpers (create/delete/visibility/invert/rename/add-component/set-adjustment/reset). ~250 lines.
- `ferrolite-app/src/develop/mask_panel.rs` — the right-panel Masks section (list rows + component add + Light+Color sliders). egui. ~400 lines.
- `ferrolite-app/src/develop/mask_overlay.rs` — canvas overlay: fill paint (egui texture) + tool-affordance routing (delegates to the pure affordance modules). egui. ~350 lines.
- `ferrolite-app/src/develop/mask_affordance.rs` — pure hit-test / handle-drag / stroke-capture / eyedropper-sample math for all tools (linear, radial, brush, range). ~350 lines.
- `ferrolite-app/src/develop/mask_overlay_color.rs` — pure coverage→RGBA overlay-image builder. ~60 lines.

**Create (ferrolite-mask / ferrolite-pipeline):**
- `ferrolite-mask/src/compositor.rs` — `MaskCompositor` (owns the shape/brush/composite passes, built once) + `MaskBuffer` CPU readback. Reused by the Plan-3 `LocalAdjustmentsNode` and the overlay. ~200 lines.
- `ferrolite-pipeline/src/mask_overlay.rs` — `MaskOverlayCompositor`: composites a `MaskDefinition` at a bounded resolution against a bounded input and returns coverage `Vec<f32>`. ~120 lines.

**Modify:**
- `ferrolite-mask/src/lib.rs` — export `MaskCompositor`, `read_mask_r32f`.
- `ferrolite-pipeline/src/local_node.rs` — use `MaskCompositor` instead of the inline composite match (goldens guard behavior).
- `ferrolite-pipeline/src/coord.rs` — add `source_to_display`.
- `ferrolite-pipeline/src/lib.rs` — export `source_to_display`, `MaskOverlayCompositor`.
- `ferrolite-app/src/develop/mod.rs` — declare the new modules.
- `ferrolite-app/src/develop/history.rs` — add `break_coalesce`.
- `ferrolite-app/src/develop/adjustment_panel.rs` — add the "Masks" `CollapsingHeader` calling `mask_panel::show`; set/clear `mask_active`.
- `ferrolite-app/src/viewer/mod.rs` — embed `MaskUiState` in `ViewerState`; init in the constructor.
- `ferrolite-app/src/app.rs` — seal history after a committed `LocalAdjustments` edit; call `mask_overlay::show` on the canvas (gated on `mask_active`), thread its `EditOutcome` through `apply_edit`; rebuild the overlay egui texture when its key changes.

---

## Interfaces (names later tasks depend on — defined once)

```rust
// ferrolite-app/src/develop/mask_ui.rs
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MaskTool { #[default] Brush, Linear, Radial, LumaRange, ColorRange }

pub struct MaskUiState {
    pub active: bool,                 // Masking tool engaged (overlay + affordances shown)
    pub selected: Option<usize>,      // selected MaskLayer index
    pub tool: MaskTool,
    pub next_mode: ferrolite_mask::CompositeMode, // Add/Subtract/Intersect for the next component
    pub overlay_on: bool,             // colored mask overlay visible (default true)
    // brush params (normalized source units for radius; [0,1] for hardness/flow)
    pub brush_radius: f32, pub brush_hardness: f32, pub brush_flow: f32, pub brush_erase: bool,
    // range params
    pub range_lo: f32, pub range_hi: f32, pub range_softness: f32,
    pub color_tolerance: f32, pub color_softness: f32,
    pub color_samples: Vec<ferrolite_mask::Rgb>,
    // in-progress gesture (None between gestures)
    pub gesture: Option<MaskGesture>,
    // overlay cache invalidation key (recomputed each frame; texture rebuilt on change)
    pub overlay_key: Option<u64>,
    pub rename_buf: Option<(usize, String)>, // (idx, editing text) while renaming
}
impl MaskUiState { pub fn clamp_selection(&mut self, layer_count: usize); }
pub enum MaskGesture { Stroke(Vec<ferrolite_mask::BrushNode>), DragHandle { /* see Task 10/11 */ } }

// ferrolite-app/src/develop/mask_edit.rs  (pure; returns new OpStack, kind always LocalAdjustments)
pub fn layers(stack: &OpStack) -> LocalAdjustments;       // stack.local_adjustments().unwrap_or_default()
pub fn create_mask(stack: &OpStack, name: String) -> OpStack;
pub fn delete_mask(stack: &OpStack, idx: usize) -> OpStack;
pub fn set_visible(stack: &OpStack, idx: usize, v: bool) -> OpStack;
pub fn set_invert(stack: &OpStack, idx: usize, v: bool) -> OpStack;
pub fn rename(stack: &OpStack, idx: usize, name: String) -> OpStack;
pub fn add_component(stack: &OpStack, idx: usize, c: MaskComponent, m: CompositeMode) -> OpStack;
pub fn set_adjustments(stack: &OpStack, idx: usize, a: AdjustmentSet) -> OpStack;
// all normalize: a LocalAdjustments with zero layers → reset(OpKind::LocalAdjustments)

// ferrolite-pipeline/src/coord.rs
pub fn source_to_display(geo: Option<Geometry>, src_w: u32, src_h: u32,
                         src_norm: (f32, f32)) -> (f32, f32); // inverse of display_to_source

// ferrolite-mask/src/compositor.rs
pub struct MaskCompositor { /* owns LinearGradientPass, RadialGradientPass, LumaRangePass,
                               ColorRangePass, BrushRasterizer, CompositePass */ }
impl MaskCompositor {
    pub fn new(ctx: Arc<GpuContext>) -> Self;
    pub fn composite(&self, def: &MaskDefinition, input: &wgpu::TextureView,
                     w: u32, h: u32) -> MaskBuffer;
}
pub fn read_mask_r32f(ctx: &GpuContext, buf: &MaskBuffer) -> Vec<f32>; // row-unpadded coverage

// ferrolite-pipeline/src/mask_overlay.rs
pub struct MaskOverlayCompositor { compositor: ferrolite_mask::MaskCompositor }
impl MaskOverlayCompositor {
    pub fn new(ctx: Arc<GpuContext>) -> Self;
    /// Composite `def` against `input` at input's dims; return (w, h, coverage[0..1]).
    pub fn coverage(&self, ctx: &GpuContext, def: &MaskDefinition,
                    input: &PipelineImage) -> (u32, u32, Vec<f32>);
}

// ferrolite-app/src/develop/mask_overlay_color.rs
pub const OVERLAY_MAX_EDGE: u32 = 512;
pub fn overlay_rgba(coverage: &[f32], strength: f32) -> Vec<u8>; // red, a = coverage*strength*255

// ferrolite-app/src/develop/mask_affordance.rs  (pure; see per-tool tasks)
// linear/radial hit-test + drag; brush stroke append; eyedropper sample.

// ferrolite-app/src/develop/history.rs
impl History { pub fn break_coalesce(&mut self); } // resets last_kind so next push won't coalesce
```

---

## Task 1: `mask_ui.rs` — tool + UI state

**Files:**
- Create: `ferrolite-app/src/develop/mask_ui.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod mask_ui;`)
- Modify: `ferrolite-app/src/viewer/mod.rs` (add `pub mask: crate::develop::mask_ui::MaskUiState`, init in ctor)

**Interfaces:** Produces `MaskTool`, `MaskUiState`, `MaskGesture` (see Interfaces block). `MaskGesture` variants are filled in by Tasks 10–11; define the enum now with the `Stroke` variant and a placeholder `DragHandle` carrying the raw data those tasks need.

- [ ] **Step 1: Write the failing test.** Create `ferrolite-app/src/develop/mask_ui.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_brush_no_selection_overlay_on() {
        let s = MaskUiState::default();
        assert!(!s.active);
        assert_eq!(s.selected, None);
        assert_eq!(s.tool, MaskTool::Brush);
        assert_eq!(s.next_mode, ferrolite_mask::CompositeMode::Add);
        assert!(s.overlay_on);
        assert!(s.gesture.is_none());
        // sane brush defaults in [0,1]-ish ranges
        assert!(s.brush_radius > 0.0 && s.brush_hardness >= 0.0 && s.brush_flow > 0.0);
    }

    #[test]
    fn clamp_selection_drops_out_of_range() {
        let mut s = MaskUiState { selected: Some(3), ..Default::default() };
        s.clamp_selection(2); // only indices 0,1 valid
        assert_eq!(s.selected, Some(1), "clamped to last valid index");
        s.clamp_selection(0); // no layers
        assert_eq!(s.selected, None);
        let mut s2 = MaskUiState { selected: Some(0), ..Default::default() };
        s2.clamp_selection(2);
        assert_eq!(s2.selected, Some(0), "in-range selection preserved");
    }
}
```

- [ ] **Step 2: Run to verify it fails.**

Run: `cargo test -p ferrolite-app --lib develop::mask_ui::tests`
Expected: FAIL — types not defined.

- [ ] **Step 3: Implement.** Prepend to `mask_ui.rs`:

```rust
//! Per-image Masking-tool UI state + the tool enum. Pure data + tiny selection
//! helpers (unit-tested); egui rendering lives in `mask_panel`/`mask_overlay`.
//! Mirrors how `hsl_band`/`crop_active` live on `ViewerState` (survives the
//! panel's per-frame `Option` plumbing).

use ferrolite_mask::{BrushNode, CompositeMode, Rgb};

/// The unified Masking tool's active component tool. Linear/Radial are gradient
/// component types, not separate tools (design §9.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MaskTool {
    #[default]
    Brush,
    Linear,
    Radial,
    LumaRange,
    ColorRange,
}

/// An in-progress canvas gesture (between pointer-down and pointer-up). `None`
/// between gestures. Filled by the affordance routing in `mask_overlay`.
pub enum MaskGesture {
    /// Brush stroke being captured: accumulated dab nodes (normalized source coords).
    Stroke(Vec<BrushNode>),
    /// A shape handle being dragged: the component index within the mask + which
    /// handle. The concrete handle payloads are defined by the affordance modules
    /// (linear/radial); this carries the raw drag origin so the affordance can
    /// resolve the new params each frame.
    DragHandle { component: usize, handle: u32, origin_src: (f32, f32) },
}

pub struct MaskUiState {
    pub active: bool,
    pub selected: Option<usize>,
    pub tool: MaskTool,
    pub next_mode: CompositeMode,
    pub overlay_on: bool,
    pub brush_radius: f32,
    pub brush_hardness: f32,
    pub brush_flow: f32,
    pub brush_erase: bool,
    pub range_lo: f32,
    pub range_hi: f32,
    pub range_softness: f32,
    pub color_tolerance: f32,
    pub color_softness: f32,
    pub color_samples: Vec<Rgb>,
    pub gesture: Option<MaskGesture>,
    pub overlay_key: Option<u64>,
    pub rename_buf: Option<(usize, String)>,
}

impl Default for MaskUiState {
    fn default() -> Self {
        Self {
            active: false,
            selected: None,
            tool: MaskTool::default(),
            next_mode: CompositeMode::Add,
            overlay_on: true,
            brush_radius: 0.08, // fraction of the image's smaller edge
            brush_hardness: 0.5,
            brush_flow: 1.0,
            brush_erase: false,
            range_lo: 0.3,
            range_hi: 0.7,
            range_softness: 0.1,
            color_tolerance: 0.15,
            color_softness: 0.1,
            color_samples: Vec::new(),
            gesture: None,
            overlay_key: None,
            rename_buf: None,
        }
    }
}

impl MaskUiState {
    /// Keep `selected` valid against the current layer count: clamp to the last
    /// index, or clear when there are no layers.
    pub fn clamp_selection(&mut self, layer_count: usize) {
        self.selected = match (self.selected, layer_count) {
            (_, 0) => None,
            (Some(i), n) => Some(i.min(n - 1)),
            (None, _) => None,
        };
    }
}
```

- [ ] **Step 4: Declare module + embed in ViewerState.** In `mod.rs` add `pub mod mask_ui;`. In `viewer/mod.rs`, add to `ViewerState` (near `hsl_band`):
```rust
    /// Masking-tool UI state (design §9). Per-image, like `hsl_band`/`crop_active`.
    pub mask: crate::develop::mask_ui::MaskUiState,
```
and in the `ViewerState` constructor initializer add `mask: crate::develop::mask_ui::MaskUiState::default(),`.

- [ ] **Step 5: Run tests + build.**

Run: `cargo test -p ferrolite-app --lib develop::mask_ui::tests` (PASS) then `cargo build -p ferrolite-app` (compiles with the new field).

- [ ] **Step 6: Commit.**

```bash
git add ferrolite-app/src/develop/mask_ui.rs ferrolite-app/src/develop/mod.rs ferrolite-app/src/viewer/mod.rs
git commit -m "feat(develop): MaskTool + MaskUiState scaffold on ViewerState"
```

---

## Task 2: `mask_edit.rs` — pure OpStack editing helpers

**Files:**
- Create: `ferrolite-app/src/develop/mask_edit.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod mask_edit;`)

**Interfaces:** Consumes `ferrolite_pipeline::{Op, OpKind, OpStack, LocalAdjustments, MaskLayer, AdjustmentSet}`, `ferrolite_mask::{MaskComponent, MaskDefinition, CompositeMode}`. Produces the `mask_edit::*` helpers in the Interfaces block. Every mutator returns a new `OpStack` (immutable) with `kind == OpKind::LocalAdjustments`; a `LocalAdjustments` with zero layers is written back as `reset(OpKind::LocalAdjustments)` so `is_identity()`/`has_edits` stay correct (mirroring `ops_edit`).

- [ ] **Step 1: Write the failing tests.** Create `mask_edit.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_mask::{CompositeMode, MaskComponent, Vec2};
    use ferrolite_pipeline::{AdjustmentSet, OpKind, OpStack};

    fn brush() -> MaskComponent { MaskComponent::Brush { strokes: vec![] } }

    #[test]
    fn create_appends_a_layer_and_is_not_identity() {
        let s = create_mask(&OpStack::default(), "Mask 1".into());
        let la = layers(&s);
        assert_eq!(la.layers.len(), 1);
        assert_eq!(la.layers[0].name, "Mask 1");
        assert!(la.layers[0].visible);
        assert!(!s.is_identity());
    }

    #[test]
    fn delete_last_layer_resets_the_op() {
        let s = create_mask(&OpStack::default(), "m".into());
        let s2 = delete_mask(&s, 0);
        assert!(s2.local_adjustments().is_none(), "empty layers => op removed");
        assert!(s2.is_identity());
    }

    #[test]
    fn visibility_invert_rename_roundtrip() {
        let s = create_mask(&OpStack::default(), "m".into());
        let s = set_visible(&s, 0, false);
        assert!(!layers(&s).layers[0].visible);
        let s = set_invert(&s, 0, true);
        assert!(layers(&s).layers[0].mask.invert);
        let s = rename(&s, 0, "sky".into());
        assert_eq!(layers(&s).layers[0].name, "sky");
    }

    #[test]
    fn add_component_appends_with_mode() {
        let s = create_mask(&OpStack::default(), "m".into());
        let s = add_component(&s, 0,
            MaskComponent::LinearGradient { start: Vec2::new(0.0,0.0), end: Vec2::new(0.0,1.0) },
            CompositeMode::Subtract);
        let comps = &layers(&s).layers[0].mask.components;
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].1, CompositeMode::Subtract);
    }

    #[test]
    fn set_adjustments_replaces_the_layers_set() {
        let s = create_mask(&OpStack::default(), "m".into());
        let a = AdjustmentSet { exposure: 0.5, ..Default::default() };
        let s = set_adjustments(&s, 0, a);
        assert_eq!(layers(&s).layers[0].adjustments.exposure, 0.5);
    }

    #[test]
    fn out_of_range_index_is_a_noop() {
        let s = create_mask(&OpStack::default(), "m".into());
        let same = set_visible(&s, 9, false); // idx 9 doesn't exist
        assert_eq!(same, s, "out-of-range edit returns the stack unchanged");
    }

    #[test]
    fn edits_keep_kind_local_adjustments() {
        // Sanity: create/add both live under the one op kind.
        let s = add_component(&create_mask(&OpStack::default(), "m".into()), 0, brush(), CompositeMode::Add);
        assert_eq!(s.local_adjustments().unwrap().layers[0].mask.components.len(), 1);
        let _ = OpKind::LocalAdjustments; // kind used by the app when pushing history
    }
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p ferrolite-app --lib develop::mask_edit::tests`
Expected: FAIL — helpers not defined.

- [ ] **Step 3: Implement.** Prepend to `mask_edit.rs`:

```rust
//! Pure helpers mapping a Masking-UI action to a new immutable `OpStack`. A
//! `LocalAdjustments` with zero layers REMOVES the op (reset) so
//! `is_identity()`/`has_edits` stay correct — mirroring `ops_edit`. All edits
//! carry `OpKind::LocalAdjustments`; the app pushes one history entry per gesture.

use ferrolite_mask::{CompositeMode, MaskComponent};
use ferrolite_pipeline::{AdjustmentSet, LocalAdjustments, MaskLayer, Op, OpKind, OpStack};

pub fn layers(stack: &OpStack) -> LocalAdjustments {
    stack.local_adjustments().unwrap_or_default()
}

/// Write `la` back into `stack`, resetting the op when there are no layers.
fn write(stack: &OpStack, la: LocalAdjustments) -> OpStack {
    if la.layers.is_empty() {
        stack.reset(OpKind::LocalAdjustments)
    } else {
        stack.set_op(Op::LocalAdjustments(la))
    }
}

/// Edit the layer at `idx` in place (immutably); out-of-range → unchanged stack.
fn edit_layer(stack: &OpStack, idx: usize, f: impl FnOnce(&mut MaskLayer)) -> OpStack {
    let mut la = layers(stack);
    let Some(layer) = la.layers.get_mut(idx) else {
        return stack.clone();
    };
    f(layer);
    write(stack, la)
}

pub fn create_mask(stack: &OpStack, name: String) -> OpStack {
    let mut la = layers(stack);
    la.layers.push(MaskLayer {
        name,
        visible: true,
        mask: Default::default(),
        adjustments: AdjustmentSet::default(),
    });
    write(stack, la)
}

pub fn delete_mask(stack: &OpStack, idx: usize) -> OpStack {
    let mut la = layers(stack);
    if idx >= la.layers.len() {
        return stack.clone();
    }
    la.layers.remove(idx);
    write(stack, la)
}

pub fn set_visible(stack: &OpStack, idx: usize, v: bool) -> OpStack {
    edit_layer(stack, idx, |l| l.visible = v)
}

pub fn set_invert(stack: &OpStack, idx: usize, v: bool) -> OpStack {
    edit_layer(stack, idx, |l| l.mask.invert = v)
}

pub fn rename(stack: &OpStack, idx: usize, name: String) -> OpStack {
    edit_layer(stack, idx, |l| l.name = name)
}

pub fn add_component(stack: &OpStack, idx: usize, c: MaskComponent, m: CompositeMode) -> OpStack {
    edit_layer(stack, idx, |l| l.mask.components.push((c, m)))
}

pub fn set_adjustments(stack: &OpStack, idx: usize, a: AdjustmentSet) -> OpStack {
    edit_layer(stack, idx, |l| l.adjustments = a)
}
```

- [ ] **Step 4: Declare module.** In `mod.rs` add `pub mod mask_edit;`.

- [ ] **Step 5: Run tests.**

Run: `cargo test -p ferrolite-app --lib develop::mask_edit::tests`
Expected: PASS (7 tests).

- [ ] **Step 6: fmt + clippy + commit.**

```bash
cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --lib -- -D warnings
git add ferrolite-app/src/develop/mask_edit.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(develop): pure mask_edit OpStack helpers (create/delete/visibility/invert/rename/component/adjustments)"
```

---

## Task 3: Per-gesture history sealing

**Files:**
- Modify: `ferrolite-app/src/develop/history.rs` (add `break_coalesce` + test)
- Modify: `ferrolite-app/src/app.rs:1448` (seal after a committed `LocalAdjustments` push)

**Interfaces:** Produces `History::break_coalesce(&mut self)`. Consumed by `app.rs`'s `apply_edit`.

**Why:** `History::push` coalesces consecutive same-`OpKind` pushes into one step. Global ops each have a distinct kind, so a run coalesces one gesture. But **every mask edit is `OpKind::LocalAdjustments`**, so a brush-stroke commit followed by a slider-drag commit would wrongly merge into one undo step. Sealing after each committed mask gesture makes each its own entry (§9.4: "stroke = one entry on commit").

- [ ] **Step 1: Write the failing test.** Add to `history.rs` tests:

```rust
#[test]
fn break_coalesce_separates_same_kind_gestures() {
    use ferrolite_pipeline::{LocalAdjustments, Op};
    let la = |n: usize| {
        let mut d = LocalAdjustments::default();
        for i in 0..n {
            d.layers.push(ferrolite_pipeline::MaskLayer {
                name: format!("m{i}"), visible: true, mask: Default::default(),
                adjustments: Default::default() });
        }
        OpStack::default().set_op(Op::LocalAdjustments(d))
    };
    let mut h = History::new(OpStack::default(), 50);
    // Gesture 1: one mask (a "stroke" commit).
    h.push(OpKind::LocalAdjustments, la(1));
    h.break_coalesce();
    // Gesture 2: two masks (a distinct "slider" commit) — must NOT coalesce into gesture 1.
    h.push(OpKind::LocalAdjustments, la(2));
    // Two distinct undo steps back to identity.
    assert_eq!(h.undo(), Some(la(1)), "undo returns to after gesture 1");
    assert_eq!(h.undo(), Some(OpStack::default()), "undo returns to identity");
    assert!(!h.can_undo());
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p ferrolite-app --lib develop::history::tests::break_coalesce_separates_same_kind_gestures`
Expected: FAIL — `break_coalesce` not defined (or, if you stub it empty, the two pushes coalesce and the first `undo` returns identity, not `la(1)`).

- [ ] **Step 3: Implement.** In `impl History`, add:

```rust
    /// Force the NEXT `push` to start a fresh step instead of coalescing into the
    /// current tip. Called after a committed mask gesture (all mask edits share
    /// `OpKind::LocalAdjustments`, so kind-based coalescing would otherwise merge
    /// separate gestures — a stroke and a later slider edit — into one undo step).
    pub fn break_coalesce(&mut self) {
        self.last_kind = None;
    }
```

- [ ] **Step 4: Wire into `apply_edit`.** In `app.rs`, immediately after `v.history.push(kind, stack.clone());` (line ~1448), add:

```rust
        // Mask edits all share OpKind::LocalAdjustments; seal so each committed
        // gesture (stroke, slider drag, discrete action) is its own undo step.
        if kind == ferrolite_pipeline::OpKind::LocalAdjustments {
            v.history.break_coalesce();
        }
```

- [ ] **Step 5: Run tests + build.**

Run: `cargo test -p ferrolite-app --lib develop::history::tests` (PASS, incl. existing coalescing tests unchanged) then `cargo build -p ferrolite-app`.

- [ ] **Step 6: fmt + clippy + commit.**

```bash
cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --lib -- -D warnings
git add ferrolite-app/src/develop/history.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): per-gesture history sealing for LocalAdjustments edits"
```

---

## Task 4: `source_to_display` inverse coordinate mapping

**Files:**
- Modify: `ferrolite-pipeline/src/coord.rs` (add `source_to_display` + tests)
- Modify: `ferrolite-pipeline/src/lib.rs` (`pub use coord::source_to_display;`)

**Interfaces:** Consumes `Geometry`, `crate::uniforms::geometry_uniform` (returns `(GeometryUniform, out_w, out_h)`; `m` is row-major 2×2 `[m00,m01,m10,m11]`, `off`, `src_dims`, `out_dims`, with `src_px = m·out_px + off`). Produces `source_to_display(geo, src_w, src_h, src_norm) -> (f32,f32)` — the inverse of `display_to_source`: maps a normalized SOURCE point to normalized OUTPUT/crop space (for placing handles on the displayed image). Under identity geometry it is the identity map.

- [ ] **Step 1: Write the failing tests.** Add to `coord.rs` tests:

```rust
#[test]
fn source_to_display_is_identity_for_identity_geometry() {
    let p = source_to_display(None, 100, 80, (0.25, 0.75));
    assert!((p.0 - 0.25).abs() < 1e-4 && (p.1 - 0.75).abs() < 1e-4);
}

#[test]
fn source_to_display_round_trips_display_to_source_under_crop() {
    use crate::op::{Aspect, CropRect, Geometry};
    let geo = Geometry { crop: CropRect { x: 0.25, y: 0.25, w: 0.5, h: 0.5 },
        angle_deg: 0.0, aspect: Aspect::Free };
    for &(ox, oy) in &[(0.0f32, 0.0f32), (1.0, 1.0), (0.3, 0.6)] {
        let src = display_to_source(Some(geo), 100, 100, (ox, oy));
        let back = source_to_display(Some(geo), 100, 100, src);
        assert!((back.0 - ox).abs() < 1e-3 && (back.1 - oy).abs() < 1e-3, "round-trip {ox},{oy} -> {back:?}");
    }
}

#[test]
fn source_to_display_round_trips_under_rotation() {
    use crate::op::{Aspect, CropRect, Geometry};
    let geo = Geometry { crop: CropRect::full(), angle_deg: 30.0, aspect: Aspect::Original };
    let src = display_to_source(Some(geo), 120, 90, (0.4, 0.55));
    let back = source_to_display(Some(geo), 120, 90, src);
    assert!((back.0 - 0.4).abs() < 2e-3 && (back.1 - 0.55).abs() < 2e-3, "rot round-trip -> {back:?}");
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p ferrolite-pipeline --lib coord::tests::source_to_display_is_identity_for_identity_geometry`
Expected: FAIL — not defined.

- [ ] **Step 3: Implement.** Add to `coord.rs` (after `display_to_source`):

```rust
/// Inverse of `display_to_source`: map a normalized SOURCE point to normalized
/// OUTPUT/crop space, for placing mask handles on the displayed (cropped/rotated)
/// image. `src_px = m·out_px + off` ⇒ `out_px = m⁻¹·(src_px − off)`; then normalize
/// by the output dims. Identity geometry → the identity map.
pub fn source_to_display(
    geo: Option<Geometry>,
    src_w: u32,
    src_h: u32,
    src_norm: (f32, f32),
) -> (f32, f32) {
    let (u, out_w, out_h) = geometry_uniform(geo, src_w, src_h);
    let sx = src_norm.0 * u.src_dims[0];
    let sy = src_norm.1 * u.src_dims[1];
    // Invert the row-major 2×2 m = [a b; c d].
    let (a, b, c, d) = (u.m[0], u.m[1], u.m[2], u.m[3]);
    let det = a * d - b * c;
    let inv_det = if det.abs() < 1e-12 { 0.0 } else { 1.0 / det };
    let dx = sx - u.off[0];
    let dy = sy - u.off[1];
    let ox = (d * dx - b * dy) * inv_det;
    let oy = (-c * dx + a * dy) * inv_det;
    (ox / out_w as f32, oy / out_h as f32)
}
```

- [ ] **Step 4: Re-export.** In `ferrolite-pipeline/src/lib.rs`, extend the coord re-export: `pub use coord::{display_to_source, source_to_display};`.

- [ ] **Step 5: Run tests.**

Run: `cargo test -p ferrolite-pipeline --lib coord::tests`
Expected: PASS (existing + 3 new).

- [ ] **Step 6: fmt + clippy + commit.**

```bash
cargo fmt -p ferrolite-pipeline && cargo clippy -p ferrolite-pipeline --lib -- -D warnings
git add ferrolite-pipeline/src/coord.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): source_to_display inverse coord mapping (handle placement)"
```

---

## Task 5: `MaskCompositor` extraction + `MaskBuffer` readback

**Files:**
- Create: `ferrolite-mask/src/compositor.rs`
- Modify: `ferrolite-mask/src/lib.rs` (`mod compositor; pub use compositor::{MaskCompositor, read_mask_r32f};`)
- Modify: `ferrolite-pipeline/src/local_node.rs` (use `MaskCompositor` in place of the inline composite match)

**Interfaces:** Produces `ferrolite_mask::MaskCompositor` + `read_mask_r32f` (see Interfaces block). Consumes the existing public passes (`LinearGradientPass`, `RadialGradientPass`, `LumaRangePass`, `ColorRangePass`, `BrushRasterizer`, `CompositePass`), `stroke_dabs`, `SPACING_FRAC`, `MaskBuffer`, `MASK_FORMAT`. The `LocalAdjustmentsNode` (Plan 3) is refactored to delegate its per-layer mask compositing to `MaskCompositor::composite` — behavior MUST stay identical (the `local_golden`/`two_layer_masked` goldens guard this).

**Why:** the overlay (Task 6) needs the exact same "composite a `MaskDefinition` against an input at (w,h)" logic the node already has. Extract it once (single source of truth for empty→ones / empty+invert→zero / `Imported`→zeroed / brush stamping / fold+invert) rather than duplicate it.

- [ ] **Step 1: Study the current node logic.** Read `ferrolite-pipeline/src/local_node.rs` — the `composite_mask`, `eval_component`, and `ones_mask` methods. The extracted `MaskCompositor::composite` reproduces them verbatim (same semantics), owning the passes.

- [ ] **Step 2: Write the compositor with a smoke test.** Create `ferrolite-mask/src/compositor.rs`:

```rust
//! `MaskCompositor` — composite a `MaskDefinition` into one `MaskBuffer` by
//! evaluating each component (analytic shapes, range shapes sampling `input`,
//! brush dab-stamping) and folding by `CompositeMode` (+ final invert). Owns the
//! shape/brush/composite passes, built ONCE. The single source of truth for mask
//! compositing semantics: used by `ferrolite_pipeline::LocalAdjustmentsNode`
//! (the edit DAG) and `MaskOverlayCompositor` (the UI overlay).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;

use crate::buffer::{MaskBuffer, MASK_FORMAT};
use crate::model::{CompositeMode, MaskComponent, MaskDefinition};
use crate::shapes::{ColorRangePass, LinearGradientPass, LumaRangePass, RadialGradientPass};
use crate::stroke::{stroke_dabs, SPACING_FRAC};
use crate::vec::{Rgb, Vec2};
use crate::{BrushRasterizer, CompositePass};

pub struct MaskCompositor {
    ctx: Arc<GpuContext>,
    linear: LinearGradientPass,
    radial: RadialGradientPass,
    luma: LumaRangePass,
    color: ColorRangePass,
    brush: BrushRasterizer,
    composite: CompositePass,
}

impl MaskCompositor {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        Self {
            linear: LinearGradientPass::new(ctx.clone()),
            radial: RadialGradientPass::new(ctx.clone()),
            luma: LumaRangePass::new(ctx.clone()),
            color: ColorRangePass::new(ctx.clone()),
            brush: BrushRasterizer::new(ctx.clone()),
            composite: CompositePass::new(ctx.clone()),
            ctx,
        }
    }

    fn ones(&self, w: u32, h: u32) -> MaskBuffer {
        let buf = MaskBuffer::alloc(&self.ctx, w, h);
        let ones = vec![1.0f32; (buf.width * buf.height) as usize];
        self.ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &buf.texture, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytemuck::cast_slice(&ones),
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(buf.width * 4),
                rows_per_image: Some(buf.height) },
            wgpu::Extent3d { width: buf.width, height: buf.height, depth_or_array_layers: 1 });
        buf
    }

    fn eval(&self, comp: &MaskComponent, input: &wgpu::TextureView, w: u32, h: u32) -> MaskBuffer {
        match comp {
            MaskComponent::LinearGradient { start, end } =>
                self.linear.run(Vec2::new(start.x, start.y), Vec2::new(end.x, end.y), w, h),
            MaskComponent::RadialGradient { center, radius, rotation, feather, invert } =>
                self.radial.run(Vec2::new(center.x, center.y), Vec2::new(radius.x, radius.y),
                                *rotation, *feather, *invert, w, h),
            MaskComponent::LumaRange { lo, hi, softness } =>
                self.luma.run(*lo, *hi, *softness, input, w, h),
            MaskComponent::ColorRange { samples, tolerance, softness } => {
                let s: Vec<Rgb> = samples.iter().map(|c| Rgb::new(c.r, c.g, c.b)).collect();
                self.color.run(&s, *tolerance, *softness, input, w, h)
            }
            MaskComponent::Brush { strokes } => {
                let mut acc = MaskBuffer::alloc_zeroed(&self.ctx, w, h);
                for st in strokes {
                    let dabs = stroke_dabs(st, SPACING_FRAC);
                    acc = self.brush.stamp_onto(&acc, &dabs, st.erase, (0, 0), (w, h));
                }
                acc
            }
            MaskComponent::Imported { .. } => MaskBuffer::alloc_zeroed(&self.ctx, w, h),
        }
    }

    /// Composite `def` into one mask at `(w,h)`. Empty → ones (or zeroed if
    /// inverted); otherwise fold each component by its mode, then invert.
    pub fn composite(&self, def: &MaskDefinition, input: &wgpu::TextureView, w: u32, h: u32) -> MaskBuffer {
        if def.components.is_empty() {
            return if def.invert { MaskBuffer::alloc_zeroed(&self.ctx, w, h) } else { self.ones(w, h) };
        }
        let inputs: Vec<(MaskBuffer, CompositeMode)> =
            def.components.iter().map(|(c, m)| (self.eval(c, input, w, h), *m)).collect();
        self.composite.composite(&inputs, def.invert)
    }
}

/// Read a `MaskBuffer` (R32Float) back to a row-unpadded `Vec<f32>` of length w*h.
pub fn read_mask_r32f(ctx: &GpuContext, buf: &MaskBuffer) -> Vec<f32> {
    let (w, h) = (buf.width, buf.height);
    let bpp = 4u32;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bpr = (w * bpp).div_ceil(align) * align;
    let rb = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mask-readback"), size: (bpr * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    let mut enc = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture { texture: &buf.texture, mip_level: 0,
            origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::ImageCopyBuffer { buffer: &rb, layout: wgpu::ImageDataLayout {
            offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 });
    ctx.queue.submit([enc.finish()]);
    let slice = rb.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    ctx.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let mut out = vec![0.0f32; (w * h) as usize];
    for row in 0..h {
        let start = (row * bpr) as usize;
        for x in 0..w {
            let o = start + x as usize * 4;
            out[(row * w + x) as usize] = f32::from_le_bytes([data[o], data[o+1], data[o+2], data[o+3]]);
        }
    }
    drop(data);
    rb.unmap();
    let _ = MASK_FORMAT; // documents the format assumption
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CompositeMode, MaskComponent};

    #[test]
    fn empty_definition_is_ones_or_zero_by_invert() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let comp = MaskCompositor::new(ctx.clone());
        // A 4x4 input (unused for empty defs).
        let input = MaskBuffer::alloc_zeroed(&ctx, 4, 4);
        let iv = input.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let full = comp.composite(&MaskDefinition { components: vec![], invert: false }, &iv, 4, 4);
        assert!(read_mask_r32f(&ctx, &full).iter().all(|&v| (v - 1.0).abs() < 1e-4), "empty => ones");
        let none = comp.composite(&MaskDefinition { components: vec![], invert: true }, &iv, 4, 4);
        assert!(read_mask_r32f(&ctx, &none).iter().all(|&v| v.abs() < 1e-4), "empty+invert => zero");
    }

    #[test]
    fn imported_component_contributes_zero() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let comp = MaskCompositor::new(ctx.clone());
        let input = MaskBuffer::alloc_zeroed(&ctx, 4, 4);
        let iv = input.texture.create_view(&wgpu::TextureViewDescriptor::default());
        use crate::model::{MaskProvenance, RasterHandle};
        let def = MaskDefinition { components: vec![(
            MaskComponent::Imported { handle: RasterHandle(1),
                provenance: MaskProvenance { model_id: "x".into(), model_version: "1".into(), prompt: "p".into() } },
            CompositeMode::Add)], invert: false };
        let out = comp.composite(&def, &iv, 4, 4);
        assert!(read_mask_r32f(&ctx, &out).iter().all(|&v| v.abs() < 1e-4), "imported inert => zero");
    }
}
```

- [ ] **Step 3: Run the compositor smoke tests.**

Run: `cargo test -p ferrolite-mask --lib compositor::tests`
Expected: PASS on the dev GPU (skips headless).

- [ ] **Step 4: Export.** In `ferrolite-mask/src/lib.rs` add `mod compositor;` and `pub use compositor::{read_mask_r32f, MaskCompositor};`.

- [ ] **Step 5: Refactor `LocalAdjustmentsNode` to use it.** In `ferrolite-pipeline/src/local_node.rs`: replace the node's own `linear/radial/luma/color/brush/composite` fields + `eval_component`/`composite_mask`/`ones_mask` with a single `compositor: ferrolite_mask::MaskCompositor` field (built in `new`), and change `composite_mask` call sites to `self.compositor.composite(def, color_view, w, h)`. Keep the node's A/B ping-pong apply, cache, and `is_identity` fast path exactly as-is.

- [ ] **Step 6: Verify the node behavior is unchanged (goldens are the guard).**

Run: `cargo test -p ferrolite-pipeline --test local_golden` and `cargo test -p ferrolite-pipeline --test golden`
Expected: PASS — `local_radial_exposure`, `two_layer_masked`, the parity + multi-layer + halo tests all still green (byte-identical composite semantics). If any golden drifts, the extraction changed behavior — fix the compositor to match the node verbatim.

- [ ] **Step 7: fmt + clippy + commit.**

```bash
cargo fmt -p ferrolite-mask -p ferrolite-pipeline && cargo clippy -p ferrolite-mask -p ferrolite-pipeline --all-targets -- -D warnings
git add ferrolite-mask/src/compositor.rs ferrolite-mask/src/lib.rs ferrolite-pipeline/src/local_node.rs
git commit -m "refactor(mask): extract MaskCompositor + read_mask_r32f; node delegates compositing"
```

---

## Task 6: Overlay compositor + coverage→RGBA

**Files:**
- Create: `ferrolite-pipeline/src/mask_overlay.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (`mod mask_overlay; pub use mask_overlay::MaskOverlayCompositor;`)
- Create: `ferrolite-app/src/develop/mask_overlay_color.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod mask_overlay_color;`)

**Interfaces:** Produces `ferrolite_pipeline::MaskOverlayCompositor` (composite `MaskDefinition` against a `PipelineImage` input → coverage `Vec<f32>`) and `mask_overlay_color::{OVERLAY_MAX_EDGE, overlay_rgba}`. Consumes `MaskCompositor`/`read_mask_r32f` (Task 5), `PipelineImage`.

- [ ] **Step 1: Write the pure overlay-color test first.** Create `ferrolite-app/src/develop/mask_overlay_color.rs`:

```rust
//! Pure conversion of a mask coverage buffer to a red RGBA overlay image. Alpha
//! = coverage · strength; RGB is the overlay color (default red). No egui/GPU.

/// Bounded overlay resolution (longest edge) — keeps the GPU composite + readback
/// small enough to rebuild every frame during a stroke (CLAUDE.md §1).
pub const OVERLAY_MAX_EDGE: u32 = 512;

/// Red overlay: each texel becomes (255, 0, 0, coverage·strength·255).
pub fn overlay_rgba(coverage: &[f32], strength: f32) -> Vec<u8> {
    let s = strength.clamp(0.0, 1.0);
    let mut out = Vec::with_capacity(coverage.len() * 4);
    for &c in coverage {
        let a = (c.clamp(0.0, 1.0) * s * 255.0).round() as u8;
        out.extend_from_slice(&[255, 0, 0, a]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_coverage_is_transparent_full_is_opaque_red() {
        let px = overlay_rgba(&[0.0, 1.0], 1.0);
        assert_eq!(&px[0..4], &[255, 0, 0, 0], "zero coverage -> transparent");
        assert_eq!(&px[4..8], &[255, 0, 0, 255], "full coverage -> opaque red");
    }

    #[test]
    fn strength_scales_alpha() {
        let px = overlay_rgba(&[1.0], 0.5);
        assert_eq!(px[3], 128, "half strength -> ~50% alpha");
    }

    #[test]
    fn coverage_is_clamped() {
        let px = overlay_rgba(&[-0.2, 1.5], 1.0);
        assert_eq!(px[3], 0);
        assert_eq!(px[7], 255);
    }
}
```

- [ ] **Step 2: Run the pure test (fails → passes).**

Run: `cargo test -p ferrolite-app --lib develop::mask_overlay_color::tests`
First add `pub mod mask_overlay_color;` to `mod.rs`. Expected: PASS (3 tests).

- [ ] **Step 3: Write the overlay compositor.** Create `ferrolite-pipeline/src/mask_overlay.rs`:

```rust
//! `MaskOverlayCompositor` — composites a `MaskDefinition` against a (bounded,
//! downscaled) input image and returns a CPU coverage buffer for the Develop
//! canvas overlay. Reuses `ferrolite_mask::MaskCompositor` (the same passes the
//! edit DAG uses), so the overlay is faithful to the actual mask. The app caches
//! one instance (built once) and a bounded input; it calls `coverage` only when
//! the mask/preview/toggle change (never unconditionally per frame).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{read_mask_r32f, MaskCompositor, MaskDefinition};

use crate::image::PipelineImage;

pub struct MaskOverlayCompositor {
    compositor: MaskCompositor,
}

impl MaskOverlayCompositor {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        Self { compositor: MaskCompositor::new(ctx) }
    }

    /// Composite `def` against `input` at `input`'s dimensions; return
    /// `(w, h, coverage)` with `coverage[i] ∈ [0,1]`, row-major, length w*h.
    /// `input` must already be bounded (≤ OVERLAY_MAX_EDGE) by the caller so the
    /// readback stays cheap. Range shapes sample `input`; analytic/brush shapes
    /// ignore it.
    pub fn coverage(&self, ctx: &GpuContext, def: &MaskDefinition, input: &PipelineImage)
        -> (u32, u32, Vec<f32>)
    {
        let (w, h) = (input.width, input.height);
        let iv = input.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let buf = self.compositor.composite(def, &iv, w, h);
        (w, h, read_mask_r32f(ctx, &buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::upload_source;
    use ferrolite_image::LinearRgbaF32;
    use ferrolite_mask::{CompositeMode, MaskComponent, Vec2 as MVec2};

    #[test]
    fn linear_gradient_coverage_ramps_left_to_right() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let oc = MaskOverlayCompositor::new(ctx.clone());
        // 8x1 mid-grey input (unused by the linear shape).
        let src = LinearRgbaF32::new(8, 1, vec![0.5; 8 * 4]).unwrap();
        let img = upload_source(&ctx, &src);
        let def = MaskDefinition { components: vec![(
            MaskComponent::LinearGradient { start: MVec2::new(0.0, 0.5), end: MVec2::new(1.0, 0.5) },
            CompositeMode::Add)], invert: false };
        let (w, h, cov) = oc.coverage(&ctx, &def, &img);
        assert_eq!((w, h), (8, 1));
        assert!(cov[0] < cov[7], "coverage increases left->right: {} !< {}", cov[0], cov[7]);
    }
}
```

- [ ] **Step 4: Export + run.** Add `mod mask_overlay;` and `pub use mask_overlay::MaskOverlayCompositor;` to `ferrolite-pipeline/src/lib.rs`. Also ensure `PipelineImage`'s fields (`texture`,`width`,`height`) are reachable — they are `pub`.

Run: `cargo test -p ferrolite-pipeline --lib mask_overlay::tests`
Expected: PASS on dev GPU (skips headless).

- [ ] **Step 5: fmt + clippy + commit.**

```bash
cargo fmt -p ferrolite-pipeline -p ferrolite-app && cargo clippy -p ferrolite-pipeline -p ferrolite-app --all-targets -- -D warnings
git add ferrolite-pipeline/src/mask_overlay.rs ferrolite-pipeline/src/lib.rs ferrolite-app/src/develop/mask_overlay_color.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(pipeline,develop): mask overlay compositor + coverage->RGBA"
```

---

## Task 7: Masks panel — list rows + tool entry

**Files:**
- Create: `ferrolite-app/src/develop/mask_panel.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod mask_panel;`)
- Modify: `ferrolite-app/src/develop/adjustment_panel.rs` (add the "Masks" `CollapsingHeader`)

**Interfaces:** Produces `mask_panel::show(ui: &mut egui::Ui, stack: &OpStack, mask: &mut MaskUiState) -> Option<EditOutcome>`. Consumes `mask_edit::*` (Task 2), `MaskUiState` (Task 1), `EditOutcome`. Emits `EditOutcome { kind: OpKind::LocalAdjustments, commit: true }` for discrete actions (create/delete/visibility/invert/rename-commit). Sets `mask.active = true` while the Masks section is open (mirrors `crop_active`).

This task builds the LIST half (create / rows with visibility+invert+rename+delete + selection). The adjustments + component-add half is Task 8.

- [ ] **Step 1: Scaffold `mask_panel.rs` with the list.** Create:

```rust
//! Develop right-panel Masks section (design §9.2): the masks list + Create,
//! per-row visibility / invert / rename / delete, and selection. The selected
//! mask's Light+Color set + component tools live in `selected_section` (Task 8).
//! Discrete actions emit a committed `EditOutcome` (kind = LocalAdjustments);
//! the app pushes one history entry each (per-gesture sealing, Task 3).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::mask_edit;
use crate::develop::mask_ui::MaskUiState;
use crate::theme;
use ferrolite_pipeline::{OpKind, OpStack};

pub fn show(ui: &mut egui::Ui, stack: &OpStack, mask: &mut MaskUiState) -> Option<EditOutcome> {
    let la = mask_edit::layers(stack);
    mask.clamp_selection(la.layers.len());
    let mut out: Option<EditOutcome> = None;

    let commit = |s: OpStack| EditOutcome { stack: s, kind: OpKind::LocalAdjustments, commit: true };

    if ui.button("Create New Mask").clicked() {
        let name = format!("Mask {}", la.layers.len() + 1);
        mask.selected = Some(la.layers.len()); // select the new one
        out = Some(commit(mask_edit::create_mask(stack, name)));
    }

    ui.add_space(4.0);

    // Masks list. Short (a handful of layers) so plain iteration is fine.
    for (i, layer) in la.layers.iter().enumerate() {
        ui.horizontal(|ui| {
            // Visibility toggle (eye).
            let mut vis = layer.visible;
            if ui.checkbox(&mut vis, "").changed() {
                out = Some(commit(mask_edit::set_visible(stack, i, vis)));
            }
            // Invert toggle.
            let mut inv = layer.mask.invert;
            if ui.selectable_label(inv, "Inv").clicked() {
                inv = !inv;
                out = Some(commit(mask_edit::set_invert(stack, i, inv)));
            }
            // Name / rename.
            let renaming = matches!(&mask.rename_buf, Some((idx, _)) if *idx == i);
            if renaming {
                if let Some((_, buf)) = mask.rename_buf.as_mut() {
                    let te = ui.text_edit_singleline(buf);
                    if te.lost_focus() {
                        let name = buf.clone();
                        mask.rename_buf = None;
                        if !name.trim().is_empty() {
                            out = Some(commit(mask_edit::rename(stack, i, name)));
                        }
                    }
                }
            } else {
                let selected = mask.selected == Some(i);
                let resp = ui.selectable_label(selected, &layer.name);
                if resp.clicked() {
                    mask.selected = Some(i);
                }
                if resp.double_clicked() {
                    mask.rename_buf = Some((i, layer.name.clone()));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("\u{1f5d1}").on_hover_text("Delete mask").clicked() {
                    out = Some(commit(mask_edit::delete_mask(stack, i)));
                    if mask.selected == Some(i) {
                        mask.selected = None;
                    }
                }
            });
        });
    }

    if la.layers.is_empty() {
        ui.label(egui::RichText::new("No masks yet").color(theme::TEXT_FAINT).size(11.0));
    }

    ui.add_space(6.0);
    // Selected-mask section (component tools + Light+Color) — Task 8.
    if let Some(idx) = mask.selected {
        if idx < la.layers.len() {
            if let Some(o) = super::mask_panel::selected_section(ui, stack, mask, idx) {
                out = Some(o);
            }
        }
    }

    out
}

/// Placeholder until Task 8 fills it in. Returns None so the list works standalone.
pub(crate) fn selected_section(
    _ui: &mut egui::Ui,
    _stack: &OpStack,
    _mask: &mut MaskUiState,
    _idx: usize,
) -> Option<EditOutcome> {
    None
}
```

- [ ] **Step 2: Wire the section into the panel.** In `adjustment_panel.rs`, add a new `CollapsingHeader` (place it after HSL / before Detail, matching the op order Hsl→LocalAdjustments→Sharpen):

```rust
    // ── Masks ── (design §9): unified Masking tool. Open => mask overlay + tool
    // affordances on the canvas (mirrors the Geometry section's crop_active).
    egui::CollapsingHeader::new("Masks").show(ui, |ui| {
        if let Some(v) = state.viewer.as_mut() {
            v.mask.active = true;
            let stack = v.op_stack.clone();
            if let Some(o) = crate::develop::mask_panel::show(ui, &stack, &mut v.mask) {
                out = Some(o);
            }
        }
    });
```

Set `v.mask.active = false` at the top of the frame where `crop_active` is reset (`app.rs:3422` sets `v.crop_active = false;` — add `v.mask.active = false;` beside it, so the flag re-arms only while the Masks section is open this frame).

- [ ] **Step 3: Build + clippy.**

Run: `cargo build -p ferrolite-app` then `cargo clippy -p ferrolite-app -- -D warnings`
Expected: compiles; the list renders (verified in the final visual test).

- [ ] **Step 4: Declare module + commit.** Add `pub mod mask_panel;` to `mod.rs`.

```bash
cargo fmt -p ferrolite-app
git add ferrolite-app/src/develop/mask_panel.rs ferrolite-app/src/develop/mod.rs ferrolite-app/src/develop/adjustment_panel.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): Masks panel section — list rows (create/visibility/invert/rename/delete/select)"
```

---

## Task 8: Masks panel — component tools + Light+Color adjustments

**Files:**
- Modify: `ferrolite-app/src/develop/mask_panel.rs` (implement `selected_section`)

**Interfaces:** Consumes `EguiSlider`, `mask_edit::{set_adjustments, add_component}`, `MaskUiState`, `ferrolite_mask::{MaskComponent, CompositeMode}`. Produces the selected-mask UI: a component-add row (tool picker + Add/Subtract/Intersect) and the Light+Color slider block (each with per-control reset, plus greyed reserved controls). Non-canvas components (Luma-range with current slider values) can be added straight from the panel; Brush/Linear/Radial/Color-range are captured on the canvas (Tasks 10–12) — the panel selects the tool + mode so the canvas knows what to create.

- [ ] **Step 1: Replace the `selected_section` placeholder.** In `mask_panel.rs`:

```rust
use crate::widgets::slider::EguiSlider;
use ferrolite_mask::{CompositeMode, MaskComponent};
use ferrolite_pipeline::AdjustmentSet;
use crate::develop::mask_ui::MaskTool;

pub(crate) fn selected_section(
    ui: &mut egui::Ui,
    stack: &OpStack,
    mask: &mut MaskUiState,
    idx: usize,
) -> Option<EditOutcome> {
    let la = mask_edit::layers(stack);
    let layer = &la.layers[idx];
    let mut out: Option<EditOutcome> = None;
    let commit = |s: OpStack| EditOutcome { stack: s, kind: OpKind::LocalAdjustments, commit: true };

    ui.separator();

    // ── Component tool picker + composite mode ──
    ui.horizontal(|ui| {
        for (tool, label) in [
            (MaskTool::Brush, "Brush"), (MaskTool::Linear, "Linear"),
            (MaskTool::Radial, "Radial"), (MaskTool::LumaRange, "Luma"),
            (MaskTool::ColorRange, "Color"),
        ] {
            if ui.selectable_label(mask.tool == tool, label).clicked() {
                mask.tool = tool;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Add mode").size(11.0).color(theme::TEXT_DIM));
        for (m, label) in [
            (CompositeMode::Add, "Add"), (CompositeMode::Subtract, "Subtract"),
            (CompositeMode::Intersect, "Intersect"),
        ] {
            if ui.selectable_label(mask.next_mode == m, label).clicked() {
                mask.next_mode = m;
            }
        }
    });

    // Luma-range can be added directly from the panel with the current slider
    // values (it needs no canvas gesture). The other tools are captured on the
    // canvas (Tasks 10–12); the tool+mode selection above tells the overlay what
    // to create. Show the range params + an "Add component" button when Luma is
    // the active tool.
    if mask.tool == MaskTool::LumaRange {
        ui.add(EguiSlider { label: "Lo", value: &mut mask.range_lo, min: 0.0, max: 1.0,
            default: 0.3, step: 0.01, decimals: 2, unit: "", bipolar: false, signed: false });
        ui.add(EguiSlider { label: "Hi", value: &mut mask.range_hi, min: 0.0, max: 1.0,
            default: 0.7, step: 0.01, decimals: 2, unit: "", bipolar: false, signed: false });
        ui.add(EguiSlider { label: "Softness", value: &mut mask.range_softness, min: 0.0, max: 0.5,
            default: 0.1, step: 0.01, decimals: 2, unit: "", bipolar: false, signed: false });
        if ui.button("Add Luma range").clicked() {
            let c = MaskComponent::LumaRange { lo: mask.range_lo, hi: mask.range_hi, softness: mask.range_softness };
            out = Some(commit(mask_edit::add_component(stack, idx, c, mask.next_mode)));
        }
    }

    ui.label(egui::RichText::new(format!("{} components", layer.mask.components.len()))
        .size(11.0).color(theme::TEXT_FAINT));

    ui.separator();

    // ── Light + Color adjustments (each slider carries its own reset column) ──
    let mut a = layer.adjustments;
    let mut changed = false;
    let mut commit_now = false;
    let mut slider = |ui: &mut egui::Ui, label: &str, v: &mut f32, min: f32, max: f32, bip: bool,
                      changed: &mut bool, commit_now: &mut bool| {
        let r = ui.add(EguiSlider { label, value: v, min, max, default: 0.0, step: 0.01,
            decimals: 2, unit: "", bipolar: bip, signed: bip });
        if r.changed() {
            *changed = true;
            if r.drag_stopped() || !r.dragged() { *commit_now = true; }
        }
    };

    ui.label(egui::RichText::new("Light").size(11.0).color(theme::TEXT_DIM));
    slider(ui, "Exposure", &mut a.exposure, -5.0, 5.0, true, &mut changed, &mut commit_now);
    slider(ui, "Contrast", &mut a.contrast, -1.0, 1.0, true, &mut changed, &mut commit_now);
    slider(ui, "Highlights", &mut a.highlights, -1.0, 1.0, true, &mut changed, &mut commit_now);
    slider(ui, "Shadows", &mut a.shadows, -1.0, 1.0, true, &mut changed, &mut commit_now);
    slider(ui, "Whites", &mut a.whites, -1.0, 1.0, true, &mut changed, &mut commit_now);
    slider(ui, "Blacks", &mut a.blacks, -1.0, 1.0, true, &mut changed, &mut commit_now);

    ui.label(egui::RichText::new("Color").size(11.0).color(theme::TEXT_DIM));
    slider(ui, "Temp", &mut a.temp, -1.0, 1.0, true, &mut changed, &mut commit_now);
    slider(ui, "Tint", &mut a.tint, -1.0, 1.0, true, &mut changed, &mut commit_now);
    slider(ui, "Saturation", &mut a.saturation, -1.0, 1.0, true, &mut changed, &mut commit_now);
    slider(ui, "Hue", &mut a.hue, -1.0, 1.0, true, &mut changed, &mut commit_now);
    // "Color" swatch amount (RGB picked via the swatch below).
    let mut amt = a.color.amount;
    let r = ui.add(EguiSlider { label: "Color", value: &mut amt, min: 0.0, max: 1.0, default: 0.0,
        step: 0.01, decimals: 2, unit: "", bipolar: false, signed: false });
    if r.changed() { a.color.amount = amt; changed = true; if r.drag_stopped() || !r.dragged() { commit_now = true; } }
    let mut rgb = [a.color.r, a.color.g, a.color.b];
    if ui.color_edit_button_rgb(&mut rgb).changed() {
        a.color.r = rgb[0]; a.color.g = rgb[1]; a.color.b = rgb[2];
        changed = true; commit_now = true;
    }

    // ── Reserved neighborhood controls: greyed, hover reason (design §9.2) ──
    ui.add_space(4.0);
    ui.label(egui::RichText::new("Effects").size(11.0).color(theme::TEXT_DIM));
    for name in ["Texture", "Clarity", "Dehaze", "Sharpness", "Noise"] {
        let mut dummy = 0.0f32;
        ui.add_enabled_ui(false, |ui| {
            ui.add(EguiSlider { label: name, value: &mut dummy, min: -1.0, max: 1.0, default: 0.0,
                step: 0.01, decimals: 2, unit: "", bipolar: true, signed: true });
        })
        .response
        .on_hover_text("Coming in a later phase (needs neighborhood processing)");
    }

    if changed {
        out = Some(EditOutcome {
            stack: mask_edit::set_adjustments(stack, idx, a),
            kind: OpKind::LocalAdjustments,
            commit: commit_now,
        });
    }
    out
}
```

- [ ] **Step 2: Build + clippy.**

Run: `cargo build -p ferrolite-app` then `cargo clippy -p ferrolite-app -- -D warnings`
Expected: compiles. (`ui.add_enabled_ui(false, ...)` greys the reserved sliders; the `EguiSlider` still renders its reset column so the per-control-reset invariant holds structurally, and the hover reason is attached.)

- [ ] **Step 3: fmt + commit.**

```bash
cargo fmt -p ferrolite-app
git add ferrolite-app/src/develop/mask_panel.rs
git commit -m "feat(develop): Masks panel — component tools + per-mask Light+Color sliders (per-control reset, greyed neighborhood)"
```

---

## Task 9: Canvas mask overlay — fill paint + rebuild cache

**Files:**
- Create: `ferrolite-app/src/develop/mask_overlay.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod mask_overlay;`)
- Modify: `ferrolite-app/src/app.rs` (call `mask_overlay::show` on the canvas gated on `v.mask.active`; own the overlay egui texture + bounded input; rebuild on key change)

**Interfaces:** Produces `mask_overlay::show(ui, image_rect, stack, mask, overlay_tex) -> Option<EditOutcome>` (this task paints the fill + routes nothing yet; affordances land in Tasks 10–12). Consumes the app-owned overlay texture handle. The app builds/updates the egui texture from `MaskOverlayCompositor::coverage` + `overlay_rgba` when `mask.overlay_key` changes.

**Design (read first):**
- The **fill** is an egui-painted image over `image_rect` (the displayed image), tinted red where the selected mask covers. Painted only when `mask.active && mask.overlay_on` and a selected mask exists.
- The **overlay texture** is app-owned (`Option<egui::TextureHandle>` in a new `AppState`/frame field, or on `ViewerState`). It is rebuilt when `overlay_key` (a hash of the selected `MaskDefinition` + the preview generation + `OVERLAY_MAX_EDGE`) changes — never unconditionally.
- The **bounded input** for range shapes: CPU-downscale the viewer's `preview_source` (`Arc<LinearRgbaF32>`) to ≤ `OVERLAY_MAX_EDGE` once (cached, rebuilt when the source changes), upload via `ferrolite_pipeline::upload_source`. Cache the uploaded `PipelineImage` on `ViewerState` (`mask_overlay_input`).
- Rebuild happens where the app already has the `GpuContext` and `MaskOverlayCompositor` (built once at viewer open, stored on `ViewerState` as `mask_overlay: Option<MaskOverlayCompositor>`). This is bounded (≤512² readback) and only-on-change → safe on the UI thread even mid-stroke.
- **Overlay geometry:** the coverage is source-space; paint it stretched over `image_rect`. Exact under identity/no-crop; documented pragmatic limitation under crop/rotate (parallels Plan 3). Handles (Tasks 10–12) use `display_to_source`/`source_to_display` and ARE correct under crop.

- [ ] **Step 1: Add the overlay state to `ViewerState`.** In `viewer/mod.rs`, add fields:
```rust
    /// Overlay compositor (built once at open) + cached bounded input + egui tex.
    pub mask_overlay: Option<ferrolite_pipeline::MaskOverlayCompositor>,
    pub mask_overlay_input: Option<ferrolite_pipeline::PipelineImage>,
    pub mask_overlay_tex: Option<egui::TextureHandle>,
```
Initialize all to `None` in the constructor. (Build `mask_overlay` lazily on first overlay use — see Step 3 — to avoid touching the open path.)

- [ ] **Step 2: Write the overlay module (fill paint only).** Create `ferrolite-app/src/develop/mask_overlay.rs`:

```rust
//! Canvas mask overlay: paints the composited coverage as a red tint over the
//! displayed image, then routes tool affordances (Tasks 10–12). Pure math lives
//! in `mask_affordance`; this layer only paints + routes pointer events (same
//! discipline as `crop_overlay`). Visual-tested.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::mask_ui::MaskUiState;
use ferrolite_pipeline::OpStack;

/// Paint the coverage fill (if a texture is ready + overlay is on) and route tool
/// affordances. `overlay_tex` is the app-built red-RGBA coverage texture (None
/// until first built / when no mask is selected).
pub fn show(
    ui: &mut egui::Ui,
    image_rect: egui::Rect,
    stack: &OpStack,
    mask: &mut MaskUiState,
    overlay_tex: Option<&egui::TextureHandle>,
) -> Option<EditOutcome> {
    // Fill: stretch the coverage texture over the image rect with alpha blend.
    if mask.overlay_on {
        if let Some(tex) = overlay_tex {
            ui.painter().image(
                tex.id(),
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE, // the texture already carries red + per-texel alpha
            );
        }
    }
    // Affordance routing is added in Tasks 10–12.
    let _ = (stack, mask);
    None
}
```

- [ ] **Step 3: Rebuild + paint from the app.** In `app.rs`, in the canvas region where `crop_overlay::show` is called (around line 3560–3585, gated on `crop_active`), add a parallel `mask_active` branch. Before painting, rebuild the overlay texture when its key changed:

```rust
// Mask overlay (shown while the Masks section is open). Rebuild the coverage
// texture only when the selected mask / preview generation changed.
let mask_active = self.state.viewer.as_ref().map(|v| v.mask.active).unwrap_or(false);
if mask_active {
    self.rebuild_mask_overlay_if_needed(ctx);
    let (stack, tex_present) = {
        let v = self.state.viewer.as_ref().unwrap();
        (v.op_stack.clone(), v.mask_overlay_tex.is_some())
    };
    let tex = self.state.viewer.as_ref().and_then(|v| v.mask_overlay_tex.clone());
    let mut mask_out = None;
    if let Some(v) = self.state.viewer.as_mut() {
        mask_out = crate::develop::mask_overlay::show(ui, image_rect, &stack, &mut v.mask, tex.as_ref());
    }
    let _ = tex_present;
    if let Some(o) = mask_out {
        self.apply_edit(ctx, frame, o.kind, o.stack, o.commit);
    }
}
```

Add the helper method on the app:

```rust
/// Rebuild the mask-overlay egui texture iff the selected mask definition or the
/// preview generation changed. Bounded (≤ OVERLAY_MAX_EDGE) + only-on-change, so
/// it is safe on the UI thread even mid-stroke (CLAUDE.md §1).
fn rebuild_mask_overlay_if_needed(&mut self, ctx: &egui::Context) {
    use crate::develop::mask_edit;
    use crate::develop::mask_overlay_color::{overlay_rgba, OVERLAY_MAX_EDGE};
    use std::hash::{Hash, Hasher};

    let Some(v) = self.state.viewer.as_mut() else { return };
    let la = mask_edit::layers(&v.op_stack);
    let Some(sel) = v.mask.selected.filter(|&i| i < la.layers.len()) else {
        v.mask_overlay_tex = None;
        v.mask.overlay_key = None;
        return;
    };
    let def = la.layers[sel].mask.clone();
    // Key: which mask def + preview generation. serde-hash the def (small).
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&def).unwrap_or_default().hash(&mut h);
    v.opstack_version.hash(&mut h); // preview regen bumps this
    let key = h.finish();
    if v.mask.overlay_key == Some(key) && v.mask_overlay_tex.is_some() {
        return;
    }

    // Ensure the compositor + bounded input exist.
    let Some(pipe) = v.preview_edit.as_ref() else { return };
    let gpu_ctx = pipe.gpu_context(); // Task 9 Step 4 adds this accessor (Arc<GpuContext>)
    if v.mask_overlay.is_none() {
        v.mask_overlay = Some(ferrolite_pipeline::MaskOverlayCompositor::new(gpu_ctx.clone()));
    }
    if v.mask_overlay_input.is_none() {
        if let Some(src) = v.preview_source.as_ref() {
            let small = downscale_linear(src, OVERLAY_MAX_EDGE);
            v.mask_overlay_input = Some(ferrolite_pipeline::upload_source(&gpu_ctx, &small));
        }
    }
    let (Some(oc), Some(input)) = (v.mask_overlay.as_ref(), v.mask_overlay_input.as_ref()) else { return };
    let (w, h2, cov) = oc.coverage(&gpu_ctx, &def, input);
    let rgba = overlay_rgba(&cov, 0.5); // 50% red tint
    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h2 as usize], &rgba);
    v.mask_overlay_tex = Some(ctx.load_texture("mask-overlay", img, egui::TextureOptions::LINEAR));
    v.mask.overlay_key = Some(key);
}
```

And a small CPU downscale helper (near the method, or in `mask_overlay.rs`):

```rust
/// Nearest-neighbor CPU downscale of a LinearRgbaF32 to ≤ max_edge longest side.
fn downscale_linear(src: &ferrolite_image::LinearRgbaF32, max_edge: u32) -> ferrolite_image::LinearRgbaF32 {
    let (sw, sh) = (src.width, src.height);
    let scale = (max_edge as f32 / sw.max(sh) as f32).min(1.0);
    let (dw, dh) = (((sw as f32 * scale) as u32).max(1), ((sh as f32 * scale) as u32).max(1));
    if (dw, dh) == (sw, sh) {
        return src.clone();
    }
    let mut px = Vec::with_capacity((dw * dh * 4) as usize);
    for y in 0..dh {
        let sy = (y as f32 / dh as f32 * sh as f32) as u32;
        for x in 0..dw {
            let sx = (x as f32 / dw as f32 * sw as f32) as u32;
            let i = ((sy * sw + sx) * 4) as usize;
            px.extend_from_slice(&src.pixels[i..i + 4]);
        }
    }
    ferrolite_image::LinearRgbaF32::new(dw, dh, px).expect("downscale length")
}
```

- [ ] **Step 4: Expose the GpuContext from `EditPipeline`.** In `ferrolite-pipeline/src/pipeline.rs`, add a public accessor (the pipeline already holds `ctx: Arc<GpuContext>`):
```rust
    /// The shared GPU context (for building overlay compositors, etc.).
    pub fn gpu_context(&self) -> std::sync::Arc<GpuContext> {
        self.ctx.clone()
    }
```
Re-export nothing new (it's a method). Also confirm `LinearRgbaF32` exposes `pixels`, `width`, `height` (public) and derives `Clone`.

- [ ] **Step 5: Invalidate cached input on preview change.** Where the app rebuilds/sets `preview_source` (viewer load path), also clear `v.mask_overlay_input = None;` and `v.mask_overlay_tex = None;` so the overlay re-derives against the new preview. (Search for where `preview_source` is assigned; add the two clears alongside.)

- [ ] **Step 6: Build + clippy + declare module.** Add `pub mod mask_overlay;` to `mod.rs`.

Run: `cargo build -p ferrolite-app` then `cargo clippy -p ferrolite-app --all-targets -- -D warnings` and `cargo build -p ferrolite-pipeline`.
Expected: compiles.

- [ ] **Step 7: fmt + commit.**

```bash
cargo fmt -p ferrolite-app -p ferrolite-pipeline
git add ferrolite-app/src/develop/mask_overlay.rs ferrolite-app/src/develop/mod.rs ferrolite-app/src/viewer/mod.rs ferrolite-app/src/app.rs ferrolite-pipeline/src/pipeline.rs
git commit -m "feat(develop): canvas mask overlay fill (bounded readback, on-change rebuild)"
```

---

## Task 10: Linear + Radial gradient affordances

**Files:**
- Create: `ferrolite-app/src/develop/mask_affordance.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod mask_affordance;`)
- Modify: `ferrolite-app/src/develop/mask_overlay.rs` (route linear/radial handle drags → `EditOutcome`)

**Interfaces:** Produces the pure hit-test + drag math for linear and radial gradients (egui-free, in normalized SOURCE coords). Consumes `ferrolite_mask::{MaskComponent, Vec2}`, `display_to_source`/`source_to_display`. The overlay routes: pointer-down hit-tests handles; drag updates the component's params (preview `commit=false`); pointer-up commits.

- [ ] **Step 1: Write the pure affordance tests.** Create `mask_affordance.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_hit_test_finds_endpoints() {
        let (s, e) = ((0.2f32, 0.5f32), (0.8f32, 0.5f32));
        assert_eq!(linear_hit_test(s, e, (0.2, 0.5), 0.04), Some(LinHandle::Start));
        assert_eq!(linear_hit_test(s, e, (0.8, 0.5), 0.04), Some(LinHandle::End));
        // Near the line but between endpoints => Body (move whole).
        assert_eq!(linear_hit_test(s, e, (0.5, 0.5), 0.04), Some(LinHandle::Body));
        // Far away => None.
        assert_eq!(linear_hit_test(s, e, (0.5, 0.9), 0.04), None);
    }

    #[test]
    fn linear_drag_moves_the_targeted_handle() {
        let (s, e) = ((0.2f32, 0.5f32), (0.8f32, 0.5f32));
        let (ns, ne) = linear_drag(s, e, LinHandle::End, (0.9, 0.6));
        assert_eq!(ns, s, "start unchanged");
        assert!((ne.0 - 0.9).abs() < 1e-6 && (ne.1 - 0.6).abs() < 1e-6);
    }

    #[test]
    fn linear_drag_body_translates_both() {
        let (s, e) = ((0.2f32, 0.5f32), (0.8f32, 0.5f32));
        // Body drag carries a delta (dx,dy); model as pointer delta from grab.
        let (ns, ne) = linear_drag_body(s, e, (0.1, -0.05));
        assert!((ns.0 - 0.3).abs() < 1e-6 && (ne.0 - 0.9).abs() < 1e-6);
        assert!((ns.1 - 0.45).abs() < 1e-6 && (ne.1 - 0.45).abs() < 1e-6);
    }

    #[test]
    fn radial_hit_test_center_and_axes() {
        let c = (0.5f32, 0.5f32);
        let rad = (0.3f32, 0.2f32);
        assert_eq!(radial_hit_test(c, rad, 0.0, (0.5, 0.5), 0.04), Some(RadHandle::Center));
        // +x axis edge at center + (rx, 0) = (0.8, 0.5).
        assert_eq!(radial_hit_test(c, rad, 0.0, (0.8, 0.5), 0.04), Some(RadHandle::RadiusX));
        // +y axis edge at (0.5, 0.7).
        assert_eq!(radial_hit_test(c, rad, 0.0, (0.5, 0.7), 0.04), Some(RadHandle::RadiusY));
        assert_eq!(radial_hit_test(c, rad, 0.0, (0.1, 0.1), 0.04), None);
    }

    #[test]
    fn radial_drag_center_moves_center_only() {
        let (c, r) = radial_drag((0.5, 0.5), (0.3, 0.2), 0.0, RadHandle::Center, (0.4, 0.45));
        assert!((c.0 - 0.4).abs() < 1e-6 && (c.1 - 0.45).abs() < 1e-6);
        assert_eq!(r, (0.3, 0.2), "radius unchanged when moving center");
    }

    #[test]
    fn radial_drag_radius_x_sets_x_extent() {
        let (c, r) = radial_drag((0.5, 0.5), (0.3, 0.2), 0.0, RadHandle::RadiusX, (0.9, 0.5));
        assert_eq!(c, (0.5, 0.5));
        assert!((r.0 - 0.4).abs() < 1e-6, "rx = |px - cx| = 0.4");
        assert!((r.1 - 0.2).abs() < 1e-6, "ry unchanged");
    }
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p ferrolite-app --lib develop::mask_affordance::tests`
Expected: FAIL — nothing defined.

- [ ] **Step 3: Implement the pure math.** Prepend to `mask_affordance.rs`:

```rust
//! Pure hit-test / handle-drag / stroke-capture / eyedropper math for the mask
//! tools, in normalized SOURCE coordinates ([0,1]²). No egui, no GPU — the
//! canvas overlay only routes pointer events into these (crop-overlay discipline,
//! Spec 2 §8). `p`/handles are already inverse-mapped to source coords by the
//! caller via `display_to_source`.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinHandle { Start, End, Body }

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Distance from point `p` to segment `a→b`.
fn point_seg_dist(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let len2 = abx * abx + aby * aby;
    if len2 < 1e-12 { return dist(a, p); }
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0);
    dist((a.0 + t * abx, a.1 + t * aby), p)
}

/// Which linear-gradient handle (if any) is within `r` of `p`. Endpoints win over
/// the body; the body matches anywhere within `r` of the axis segment.
pub fn linear_hit_test(start: (f32, f32), end: (f32, f32), p: (f32, f32), r: f32) -> Option<LinHandle> {
    if dist(start, p) <= r { return Some(LinHandle::Start); }
    if dist(end, p) <= r { return Some(LinHandle::End); }
    if point_seg_dist(start, end, p) <= r { return Some(LinHandle::Body); }
    None
}

/// Move the targeted endpoint to `p` (Start/End). Body is handled by `linear_drag_body`.
pub fn linear_drag(start: (f32, f32), end: (f32, f32), h: LinHandle, p: (f32, f32))
    -> ((f32, f32), (f32, f32))
{
    match h {
        LinHandle::Start => (p, end),
        LinHandle::End => (start, p),
        LinHandle::Body => (start, end),
    }
}

/// Translate the whole axis by a source-space delta.
pub fn linear_drag_body(start: (f32, f32), end: (f32, f32), d: (f32, f32))
    -> ((f32, f32), (f32, f32))
{
    ((start.0 + d.0, start.1 + d.1), (end.0 + d.0, end.1 + d.1))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RadHandle { Center, RadiusX, RadiusY }

/// Which radial handle is within `r` of `p`. Rotation is ignored for hit-testing
/// the axis endpoints in P1 (axis-aligned handles; rotation via a later handle).
pub fn radial_hit_test(center: (f32, f32), radius: (f32, f32), _rot: f32, p: (f32, f32), r: f32)
    -> Option<RadHandle>
{
    if dist(center, p) <= r { return Some(RadHandle::Center); }
    if dist((center.0 + radius.0, center.1), p) <= r { return Some(RadHandle::RadiusX); }
    if dist((center.0, center.1 + radius.1), p) <= r { return Some(RadHandle::RadiusY); }
    None
}

/// Apply a radial drag: Center moves the center; RadiusX/Y set the extent to
/// `|p − center|` on that axis (clamped ≥ a tiny epsilon so the ellipse stays valid).
pub fn radial_drag(center: (f32, f32), radius: (f32, f32), _rot: f32, h: RadHandle, p: (f32, f32))
    -> ((f32, f32), (f32, f32))
{
    match h {
        RadHandle::Center => (p, radius),
        RadHandle::RadiusX => (center, ((p.0 - center.0).abs().max(1e-3), radius.1)),
        RadHandle::RadiusY => (center, (radius.0, (p.1 - center.1).abs().max(1e-3))),
    }
}
```

- [ ] **Step 4: Run tests.**

Run: `cargo test -p ferrolite-app --lib develop::mask_affordance::tests`
Expected: PASS (6 tests).

- [ ] **Step 5: Route in the overlay.** In `mask_overlay::show`, after the fill paint, add linear/radial routing when `mask.tool == Linear|Radial`. On pointer-down over the image, if no such component exists in the selected mask yet, create one (drag to define); else hit-test its handles. Use `display_to_source`/`source_to_display` for the mapping and `image_rect` for screen↔normalized-display. Emit `EditOutcome` (commit=false while dragging, commit=true on release). Paint the handles via `ui.painter()` (endpoints as circles, axis as a line; ellipse for radial), exactly like `crop_overlay`. (Full egui routing code follows the `crop_overlay.rs` structure: `ui.interact(image_rect, id, click_and_drag)`, store the active handle in `ui.memory`, update on `dragged()`, commit on `drag_stopped()`.) Keep the geometry mapping through `display_to_source` so it is correct under crop.

Implementer note: model the linear/radial component you edit as "the FIRST LinearGradient/RadialGradient component of the selected mask, or a newly-added one." Store the component index in `MaskGesture::DragHandle { component, handle, origin_src }`.

- [ ] **Step 6: Build + clippy.**

Run: `cargo build -p ferrolite-app` then `cargo clippy -p ferrolite-app --all-targets -- -D warnings`.

- [ ] **Step 7: fmt + declare module + commit.** Add `pub mod mask_affordance;` to `mod.rs`.

```bash
cargo fmt -p ferrolite-app
git add ferrolite-app/src/develop/mask_affordance.rs ferrolite-app/src/develop/mask_overlay.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(develop): linear + radial gradient affordances (pure hit-test/drag) + canvas routing"
```

---

## Task 11: Brush affordance + cursor

**Files:**
- Modify: `ferrolite-app/src/develop/mask_affordance.rs` (add stroke-capture math + tests)
- Modify: `ferrolite-app/src/develop/mask_overlay.rs` (brush routing + cursor)

**Interfaces:** Produces `append_brush_node` (pure incremental stroke capture) + tests. Consumes `ferrolite_mask::BrushNode`, `MaskGesture::Stroke`. The overlay: on pointer-down starts a `Stroke`; each drag sample appends a node (min-distance gated); pointer-up commits a `MaskComponent::Brush { strokes: vec![Stroke{ nodes, erase }] }` via `add_component`. Live preview during the stroke updates the overlay (the overlay rebuilds from the in-progress definition — implementer wires the in-progress stroke into the overlay coverage rebuild).

- [ ] **Step 1: Write the pure test.** Add to `mask_affordance.rs` tests:

```rust
#[test]
fn append_brush_node_gates_on_min_distance() {
    use ferrolite_mask::BrushNode;
    let mut nodes: Vec<BrushNode> = vec![];
    let params = BrushParams { radius: 0.05, hardness: 0.5, flow: 1.0 };
    // First sample always appends.
    assert!(append_brush_node(&mut nodes, (0.1, 0.1), params));
    assert_eq!(nodes.len(), 1);
    // A sample closer than spacing*radius does NOT append.
    assert!(!append_brush_node(&mut nodes, (0.101, 0.1), params));
    assert_eq!(nodes.len(), 1);
    // A sample far enough appends.
    assert!(append_brush_node(&mut nodes, (0.3, 0.1), params));
    assert_eq!(nodes.len(), 2);
    assert!((nodes[1].radius - 0.05).abs() < 1e-6 && (nodes[1].flow - 1.0).abs() < 1e-6);
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p ferrolite-app --lib develop::mask_affordance::tests::append_brush_node_gates_on_min_distance`
Expected: FAIL — not defined.

- [ ] **Step 3: Implement.** Add to `mask_affordance.rs`:

```rust
use ferrolite_mask::{BrushNode, Vec2};

#[derive(Clone, Copy)]
pub struct BrushParams { pub radius: f32, pub hardness: f32, pub flow: f32 }

/// Minimum spacing between captured dab nodes, as a fraction of the brush radius.
/// Matches the engine's dab spacing philosophy (`ferrolite_mask::SPACING_FRAC`)
/// so captured nodes aren't denser than the rasterizer needs.
const CAPTURE_SPACING_FRAC: f32 = 0.25;

/// Append a dab node at `p` (source coords) iff it is at least
/// `CAPTURE_SPACING_FRAC · radius` from the last node (or the list is empty).
/// Returns whether a node was appended.
pub fn append_brush_node(nodes: &mut Vec<BrushNode>, p: (f32, f32), params: BrushParams) -> bool {
    let min_d = (CAPTURE_SPACING_FRAC * params.radius).max(1e-4);
    if let Some(last) = nodes.last() {
        if dist((last.pos.x, last.pos.y), p) < min_d {
            return false;
        }
    }
    nodes.push(BrushNode {
        pos: Vec2::new(p.0, p.1),
        radius: params.radius,
        hardness: params.hardness,
        flow: params.flow,
    });
    true
}
```

- [ ] **Step 4: Run tests.**

Run: `cargo test -p ferrolite-app --lib develop::mask_affordance::tests`
Expected: PASS.

- [ ] **Step 5: Route brush in the overlay + draw the cursor.** In `mask_overlay::show`, when `mask.tool == Brush`:
  - Draw a cursor ring at the pointer (radius = `brush_radius` mapped source→display→screen; a fainter inner ring for `hardness`), via `ui.painter().circle_stroke`.
  - On `drag_started`, set `mask.gesture = Some(MaskGesture::Stroke(vec![]))`.
  - On `dragged`, inverse-map the pointer via `display_to_source` and call `append_brush_node`; emit `EditOutcome` with a definition that includes the in-progress stroke (commit=false) so the preview + overlay update live.
  - On `drag_stopped`, commit the finished stroke as `MaskComponent::Brush` via `mask_edit::add_component(stack, sel, Brush{strokes:vec![Stroke{nodes, erase: mask.brush_erase}]}, mask.next_mode)` with commit=true, and clear `mask.gesture`.
  - Add brush param sliders (radius/hardness/flow/erase) — either here as a small canvas HUD or in the panel's brush section (put them in the panel `selected_section` under `MaskTool::Brush`, mirroring the Luma block). Each slider carries its reset column.

Implementer note: to make the live stroke preview show, the app's `rebuild_mask_overlay_if_needed` must include the in-progress `mask.gesture` stroke when hashing/compositing. Fold the in-progress stroke into a temporary `MaskDefinition` for the overlay + the `commit=false` `EditOutcome` so both preview tiers and the overlay reflect the growing stroke.

- [ ] **Step 6: Build + clippy.**

Run: `cargo build -p ferrolite-app` then `cargo clippy -p ferrolite-app --all-targets -- -D warnings`.

- [ ] **Step 7: fmt + commit.**

```bash
cargo fmt -p ferrolite-app
git add ferrolite-app/src/develop/mask_affordance.rs ferrolite-app/src/develop/mask_overlay.rs ferrolite-app/src/develop/mask_panel.rs
git commit -m "feat(develop): brush affordance (stroke capture) + cursor + brush params"
```

---

## Task 12: Range tools — eyedropper + threshold/softness

**Files:**
- Modify: `ferrolite-app/src/develop/mask_affordance.rs` (eyedropper sample + range component builders + tests)
- Modify: `ferrolite-app/src/develop/mask_overlay.rs` (color-range eyedropper routing)
- Modify: `ferrolite-app/src/develop/mask_panel.rs` (color-range params + Add)

**Interfaces:** Produces `sample_source(&LinearRgbaF32, src_norm) -> Rgb` (pure) + tests. Consumes `ferrolite_mask::Rgb`. Luma-range is already addable from the panel (Task 8); this task adds the **color-range** eyedropper (click the canvas to add a sample color) + its threshold/softness params + "Add Color range" button.

- [ ] **Step 1: Write the pure test.** Add to `mask_affordance.rs` tests:

```rust
#[test]
fn sample_source_reads_the_nearest_pixel() {
    use ferrolite_image::LinearRgbaF32;
    // 2x1: left = red, right = green.
    let img = LinearRgbaF32::new(2, 1, vec![1.0,0.0,0.0,1.0, 0.0,1.0,0.0,1.0]).unwrap();
    let left = sample_source(&img, (0.0, 0.5));
    assert!((left.r - 1.0).abs() < 1e-6 && left.g < 1e-6, "left pixel is red");
    let right = sample_source(&img, (0.99, 0.5));
    assert!(right.r < 1e-6 && (right.g - 1.0).abs() < 1e-6, "right pixel is green");
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p ferrolite-app --lib develop::mask_affordance::tests::sample_source_reads_the_nearest_pixel`
Expected: FAIL — not defined.

- [ ] **Step 3: Implement.** Add to `mask_affordance.rs`:

```rust
use ferrolite_mask::Rgb;

/// Sample the source image at a normalized source point (nearest pixel). Used by
/// the color-range eyedropper. Coords are clamped into range.
pub fn sample_source(img: &ferrolite_image::LinearRgbaF32, src_norm: (f32, f32)) -> Rgb {
    let x = ((src_norm.0.clamp(0.0, 1.0) * img.width as f32) as u32).min(img.width - 1);
    let y = ((src_norm.1.clamp(0.0, 1.0) * img.height as f32) as u32).min(img.height - 1);
    let i = ((y * img.width + x) * 4) as usize;
    Rgb::new(img.pixels[i], img.pixels[i + 1], img.pixels[i + 2])
}
```

- [ ] **Step 4: Run tests.**

Run: `cargo test -p ferrolite-app --lib develop::mask_affordance::tests`
Expected: PASS.

- [ ] **Step 5: Route color-range in the overlay + panel.**
  - In `mask_overlay::show`, when `mask.tool == ColorRange`, a click on the image inverse-maps the pointer (`display_to_source`), calls `sample_source` against the viewer's `preview_source`, and pushes the `Rgb` into `mask.color_samples` (no OpStack edit yet — samples accumulate in UI state). Draw small swatches for the collected samples.
  - In `mask_panel::selected_section`, under `MaskTool::ColorRange`, show the collected sample swatches + `Tolerance`/`Softness` sliders (each with reset) + an "Add Color range" button that emits `add_component(stack, idx, MaskComponent::ColorRange { samples: mask.color_samples.clone(), tolerance, softness }, mask.next_mode)` (commit=true), then clears `mask.color_samples`.

- [ ] **Step 6: Build + clippy.**

Run: `cargo build -p ferrolite-app` then `cargo clippy -p ferrolite-app --all-targets -- -D warnings`.

- [ ] **Step 7: fmt + commit.**

```bash
cargo fmt -p ferrolite-app
git add ferrolite-app/src/develop/mask_affordance.rs ferrolite-app/src/develop/mask_overlay.rs ferrolite-app/src/develop/mask_panel.rs
git commit -m "feat(develop): color-range eyedropper + range params (pure sample unit) + routing"
```

---

## Task 13: Undo/redo integration, mask lifecycle, gate + visual test plan

**Files:**
- Modify: `ferrolite-app/src/app.rs` (verify undo/redo clears in-progress mask gesture + overlay cache; ensure `mask_active` false disables affordances)
- Verification only otherwise.

- [ ] **Step 1: Ensure undo/redo resets transient mask state.** In `apply_undo_redo` (app.rs ~1115), after applying the undone/redone stack, clear transient mask UI state so a stale in-progress gesture or overlay can't apply to the new stack:
```rust
        if let Some(v) = self.state.viewer.as_mut() {
            v.mask.gesture = None;
            v.mask.overlay_key = None; // force overlay rebuild against the new stack
            v.mask.clamp_selection(crate::develop::mask_edit::layers(&v.op_stack).layers.len());
        }
```

- [ ] **Step 2: Add an undo/redo integration test for masks.** Add to `history.rs` tests (or a small app-level unit): a create-mask then add-component sequence produces two undo steps (this is already covered structurally by Task 3's `break_coalesce` test — confirm a create + an adjustment commit yield two steps):
```rust
#[test]
fn create_then_adjust_are_two_undo_steps() {
    use ferrolite_pipeline::{LocalAdjustments, MaskLayer, Op, AdjustmentSet};
    let with = |vis_adj: f32| {
        let d = LocalAdjustments { layers: vec![MaskLayer {
            name: "m".into(), visible: true, mask: Default::default(),
            adjustments: AdjustmentSet { exposure: vis_adj, ..Default::default() } }] };
        OpStack::default().set_op(Op::LocalAdjustments(d))
    };
    let mut h = History::new(OpStack::default(), 50);
    h.push(OpKind::LocalAdjustments, with(0.0)); h.break_coalesce(); // create
    h.push(OpKind::LocalAdjustments, with(0.5)); h.break_coalesce(); // adjust
    assert_eq!(h.undo(), Some(with(0.0)), "undo the adjustment");
    assert_eq!(h.undo(), Some(OpStack::default()), "undo the create");
}
```

Run: `cargo test -p ferrolite-app --lib develop::history::tests` → PASS.

- [ ] **Step 3: Workspace gate.**

Run: `cargo fmt --all --check`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Run: `cargo test --workspace`
Expected: all green (existing goldens unchanged after the Task-5 extraction; all new pure units pass; GPU overlay/compositor tests run on the dev GPU, skip headless).

- [ ] **Step 4: Commit.**

```bash
cargo fmt --all
git add ferrolite-app/src/app.rs ferrolite-app/src/develop/history.rs
git commit -m "feat(develop): reset transient mask state on undo/redo + integration test"
```

- [ ] **Step 5: Hand the author the visual test plan (below) and STOP.** This is 1 of 5 plans — do NOT merge/PR/finish.

**Visual test plan (hand to the author) — this plan DOES have hands-on UI to test:**

Prereqs: open an image in Develop. Expand the new **Masks** section in the right (296px) panel.

1. **Create + list.** Click **Create New Mask** → a "Mask 1" row appears and is selected. Create a second → "Mask 2". *Fail signature:* no row appears, or the panel errors.
2. **Row controls.** Toggle a row's visibility checkbox → the mask overlay for that layer shows/hides. Toggle **Inv** → coverage inverts. Double-click a name → rename field; type + Enter → name updates. Click the trash icon → row removed (and selection updates). *Fail:* toggles don't reflect on canvas; delete leaves a stale selection.
3. **Brush.** Select the **Brush** tool; adjust radius/hardness/flow. Paint a stroke on the canvas → a red overlay tint follows the stroke live; on release it persists. Toggle **erase** and paint over it → coverage removed. *Fail:* no cursor ring; overlay doesn't update during the stroke; UI hitches while painting (violates "nothing slow on the UI thread").
4. **Linear / Radial.** Select **Linear**, drag on the canvas → a gradient axis with start/end handles; drag an endpoint → the red ramp updates. Select **Radial**, drag → an ellipse with center + X/Y handles; resize/move it. *Fail:* handles not grabbable; overlay misaligned (note: with a crop applied, handles should still track content; the fill tint is exact only without crop/rotate — a documented limitation).
5. **Range tools.** Select **Luma**, set Lo/Hi/Softness, **Add Luma range** → coverage selects that tonal band. Select **Color**, click a color on the image (eyedropper) → a sample swatch; set Tolerance/Softness, **Add Color range** → coverage selects similar colors. *Fail:* eyedropper picks the wrong color; range doesn't track the sliders.
6. **Add/Subtract/Intersect.** With a mask that has one component, switch **Add mode** to Subtract, paint/add a second component → it carves out; Intersect → it narrows. *Fail:* mode ignored.
7. **Light+Color + per-control reset.** With a mask selected + coverage painted, drag **Exposure/Contrast/Highlights/Shadows/Whites/Blacks/Temp/Tint/Saturation/Hue/Color** → the masked region changes accordingly on the preview. Click each slider's **reset arrow** → that one control returns to default without touching its neighbors. *Fail:* an adjustment affects the whole image (mask not applied); a reset clears more than its own control.
8. **Greyed neighborhood controls.** Texture/Clarity/Dehaze/Sharpness/Noise appear greyed; hovering shows the "coming in a later phase" reason. *Fail:* they're editable, or have no hover reason.
9. **Overlay toggle.** Toggle the colored overlay off/on. *Fail:* overlay stuck on/off.
10. **Undo/redo (Ctrl+Z / Ctrl+Y or Edit menu).** A brush stroke and a subsequent slider edit must be **two separate** undo steps (undo once → slider reverts, mask stays; undo again → stroke reverts). Redo restores each. *Fail:* one undo reverts both (coalescing regression).
11. **Persistence.** Make mask edits, navigate away + back (or reopen) → masks + adjustments reload from the sidecar. *Fail:* masks lost on reopen.
12. **Responsiveness.** Painting, dragging handles, and slider drags stay smooth at 1:1 and fit; no multi-second freeze on open or first mask edit. *Fail:* any stall.

Confirm the gate is green, then report Plan 4 complete and wait.

---

## Self-Review

**1. Spec coverage (§9, §10, §12 plan 4):**
- §9.1 unified Masking tool (Mask+Grad folded; Heal absent) — Tasks 1, 7, 8 (tool picker; linear/radial are component types). ✔
- §9.2 masks list (visibility/invert/rename/delete/create; add-subtract-intersect a component with Brush/Linear/Radial/Luma/Color) — Tasks 7, 8, 10–12. ✔
- §9.2 per-mask Light+Color with per-control reset; greyed neighborhood controls with hover reason — Task 8. ✔
- §9.3 colored overlay from the composited buffer (default red) — Tasks 6, 9; tool affordances (brush cursor size/feather/flow; linear drag handles; radial resize/handles/feather; range eyedropper + threshold/softness) — Tasks 10–12; all hit-test/handle-drag/threshold math pure + tested — Tasks 10–12 (`mask_affordance.rs`). ✔
- §9.4 undo/redo through OpStack history; stroke = one entry on commit; per-gesture coalescing — Tasks 3, 13. ✔
- §10 nothing slow on the UI thread — bounded overlay (≤512px, on-change) Task 9; short masks list; per-gesture commit (mid-drag previews don't push history). ✔
- §5.2 source-anchored input mapping (display→source) + handle placement (source→display) — Tasks 4, 10–12. ✔
- §13 decisions honored (unified tool; source-anchored; full point-op LR set incl. reserved greyed; DAG-composited overlay; per-control reset). ✔

**2. Placeholder scan:** No "TBD"/"add error handling"/"similar to Task N". Pure-unit tasks (1–6, 10–12 math) carry complete code + tests. egui tasks (7–9, routing halves of 10–12) carry complete widget/paint code following `adjustment_panel.rs`/`crop_overlay.rs` patterns verbatim, with the exact wiring points named (`adjustment_panel` CollapsingHeader, `app.rs` canvas branch, `mask_active` reset beside `crop_active`). The `radial feather/rotation` handle is scoped to axis-aligned resize + center in P1 (rotation hit-test ignored, documented) — an intentional P1 bound, not a placeholder.

**3. Type consistency:** `MaskUiState`/`MaskTool`/`MaskGesture` (Task 1) consumed consistently by Tasks 7–12. `mask_edit::{create_mask,delete_mask,set_visible,set_invert,rename,add_component,set_adjustments,layers}` (Task 2) match all call sites. `EditOutcome { stack, kind, commit }` reused unchanged. `MaskCompositor::{new,composite}` + `read_mask_r32f` (Task 5) match `MaskOverlayCompositor` (Task 6) and the node refactor. `source_to_display`/`display_to_source` signatures stable (Task 4). `overlay_rgba`/`OVERLAY_MAX_EDGE` (Task 6) used by Task 9. `EditPipeline::gpu_context()` (Task 9 Step 4) consumed by the overlay rebuild.

**Known scope boundaries (per approved decisions):** the red coverage FILL is exact under identity/crop-translation geometry and is output-frame-approximate under rotation (documented, parallels Plan 3's tile output-space note); handles/affordances use the coord mapping and are correct under crop. Radial rotation handle is deferred (axis-aligned resize + center + feather-via-panel in P1). The overlay reflects the preview source for range shapes (bounded downscaled), not the post-Hsl graded image — a pragmatic visualization choice; the actual applied mask (edit DAG) is unaffected and remains faithful per Plan 3.
