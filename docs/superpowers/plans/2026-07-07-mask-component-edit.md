# Editable brush / linear / radial mask components — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user re-edit an existing Brush, Linear, or Radial mask component from the Components modal by clicking "Edit", which makes that specific component the active canvas-edit target (with it highlighted white).

**Architecture:** "Edit" sets `editing_component` + switches `mask.tool` to the component's tool. Two pure helpers drive it: `tool_for_component` (component → tool) and `edit_target_index` (which component the canvas affordance acts on: the edited one if it matches the tool, else the first match). The overlay's gradient-handle finders and `route_brush` consult `edit_target_index`; Radial also gets an inline Feather/Invert editor; the edited component is highlighted white via `highlight_component`.

**Tech Stack:** Rust, egui 0.29, `ferrolite-app` (`develop::mask_ui`, `mask_components_modal`, `mask_overlay`), `ferrolite-mask` types.

## Global Constraints

- **Behavior-preserving for existing flows:** creating a fresh gradient/brush (no `editing_component`) is unchanged; only *which* component an edit targets changes when `editing_component` is set.
- **Bounds-safe:** a stale/out-of-range/wrong-type `editing_component` never panics — it falls back to first-match / no-op.
- **Icons/keybinds/per-control-reset rules** (CLAUDE.md): reuse existing widgets; the inline Feather uses `EguiSlider`; no raw emoji; no new keybind needed.
- **No perf/compositing changes** (that work is done and green); this is a UI-routing feature.
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → HOLD for the author's visual test.
- Branch `fix/brush-mask-perf` (continues the masking work).

---

## Task 1: Pure helpers + radial edit state (`mask_ui.rs`)

Add the pure component→tool mapping, the edit-target selector, and the radial inline-edit state fields. All unit-testable without egui.

**Files:**
- Modify: `ferrolite-app/src/develop/mask_ui.rs`
- Test: inline `#[cfg(test)]` in `mask_ui.rs`

**Interfaces:**
- Consumes: `MaskTool`, `ferrolite_mask::{MaskComponent, CompositeMode}`.
- Produces (used by Tasks 2/3):
  - `pub fn tool_for_component(c: &MaskComponent) -> Option<MaskTool>`
  - `pub fn edit_target_index(components: &[(MaskComponent, CompositeMode)], tool: MaskTool, editing: Option<usize>) -> Option<usize>`
  - `MaskUiState` fields `radial_feather: f32`, `radial_invert: bool` (defaults `0.3`, `false`).

- [ ] **Step 1: Write the failing tests**

Add to `mask_ui.rs` `#[cfg(test)] mod tests`:

```rust
use ferrolite_mask::{CompositeMode, MaskComponent, Vec2};

fn linear() -> MaskComponent {
    MaskComponent::LinearGradient { start: Vec2::new(0.0, 0.0), end: Vec2::new(1.0, 1.0) }
}
fn radial() -> MaskComponent {
    MaskComponent::RadialGradient {
        center: Vec2::new(0.5, 0.5), radius: Vec2::new(0.2, 0.2),
        rotation: 0.0, feather: 0.3, invert: false,
    }
}
fn brush() -> MaskComponent { MaskComponent::Brush { strokes: vec![] } }
fn add(c: MaskComponent) -> (MaskComponent, CompositeMode) { (c, CompositeMode::Add) }

#[test]
fn tool_for_component_maps_every_variant() {
    assert_eq!(tool_for_component(&brush()), Some(MaskTool::Brush));
    assert_eq!(tool_for_component(&linear()), Some(MaskTool::Linear));
    assert_eq!(tool_for_component(&radial()), Some(MaskTool::Radial));
    assert_eq!(
        tool_for_component(&MaskComponent::LumaRange { lo: 0.0, hi: 1.0, softness: 0.0 }),
        Some(MaskTool::LumaRange)
    );
    assert_eq!(
        tool_for_component(&MaskComponent::ColorRange { samples: vec![], tolerance: 0.1, softness: 0.1 }),
        Some(MaskTool::ColorRange)
    );
    assert_eq!(
        tool_for_component(&MaskComponent::Imported {
            handle: ferrolite_mask::RasterHandle(0),
            provenance: ferrolite_mask::MaskProvenance { model_id: "".into(), model_version: "".into(), prompt: "".into() },
        }),
        None
    );
}

#[test]
fn edit_target_prefers_editing_when_type_matches_else_first() {
    // components: [linear#0, radial#1, linear#2]
    let comps = vec![add(linear()), add(radial()), add(linear())];
    // editing #2 (a linear) + Linear tool -> target #2 (not the first linear #0)
    assert_eq!(edit_target_index(&comps, MaskTool::Linear, Some(2)), Some(2));
    // editing #1 (a radial) but Linear tool -> type mismatch -> first linear (#0)
    assert_eq!(edit_target_index(&comps, MaskTool::Linear, Some(1)), Some(0));
    // no editing -> first matching
    assert_eq!(edit_target_index(&comps, MaskTool::Radial, None), Some(1));
    // editing out of range -> first matching
    assert_eq!(edit_target_index(&comps, MaskTool::Linear, Some(99)), Some(0));
    // no matching component -> None
    assert_eq!(edit_target_index(&[add(brush())], MaskTool::Linear, None), None);
}

#[test]
fn radial_edit_state_defaults() {
    let s = MaskUiState::default();
    assert_eq!(s.radial_feather, 0.3);
    assert!(!s.radial_invert);
}
```

- [ ] **Step 2: Run, verify fail (undefined items)**

Run: `cargo test -p ferrolite-app tool_for_component_maps_every_variant -- --nocapture`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the helpers + fields**

Add to `mask_ui.rs` (module level):

```rust
use ferrolite_mask::CompositeMode;

/// The `MaskTool` that authors/edits a given component type (`None` for the
/// non-authorable `Imported` seam).
pub fn tool_for_component(c: &MaskComponent) -> Option<MaskTool> {
    match c {
        MaskComponent::Brush { .. } => Some(MaskTool::Brush),
        MaskComponent::LinearGradient { .. } => Some(MaskTool::Linear),
        MaskComponent::RadialGradient { .. } => Some(MaskTool::Radial),
        MaskComponent::LumaRange { .. } => Some(MaskTool::LumaRange),
        MaskComponent::ColorRange { .. } => Some(MaskTool::ColorRange),
        MaskComponent::Imported { .. } => None,
    }
}

/// Which component the canvas affordance for `tool` should act on: the
/// `editing` component if it exists and matches `tool`, otherwise the first
/// component matching `tool` (the create-a-fresh-one fallback). `None` if no
/// component matches.
pub fn edit_target_index(
    components: &[(MaskComponent, CompositeMode)],
    tool: MaskTool,
    editing: Option<usize>,
) -> Option<usize> {
    if let Some(i) = editing {
        if components.get(i).and_then(|(c, _)| tool_for_component(c)) == Some(tool) {
            return Some(i);
        }
    }
    components
        .iter()
        .position(|(c, _)| tool_for_component(c) == Some(tool))
}
```

Add the fields to `MaskUiState` (near `range_*`/`color_*`): `pub radial_feather: f32,` and `pub radial_invert: bool,`, and initialize them in `Default` (near `range_softness: 0.1,`): `radial_feather: 0.3,` and `radial_invert: false,`.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p ferrolite-app -- tool_for_component_maps_every_variant edit_target_prefers radial_edit_state_defaults`
Expected: PASS.

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt && cargo clippy -p ferrolite-app --all-targets -- -D warnings`

```bash
git add ferrolite-app/src/develop/mask_ui.rs
git commit -m "feat(develop): tool_for_component + edit_target_index helpers + radial edit state"
```

---

## Task 2: Modal — Edit activation for all types + radial inline editor + white highlight

Make Edit appear for Brush/Linear/Radial, switch the tool + target the component on Edit, add a Radial Feather/Invert editor and Brush/Linear hint+Done, and highlight the edited component white.

**Files:**
- Modify: `ferrolite-app/src/develop/mask_components_modal.rs`
- Test: inline `#[cfg(test)]` in `mask_components_modal.rs`

**Interfaces:**
- Consumes: `mask_ui::{tool_for_component, MaskTool}`, `MaskUiState.{radial_feather, radial_invert, editing_component, highlight_component, tool}`, `EguiSlider`, `mask_edit::set_component`.
- Produces (used by Task 3 indirectly): sets `mask.editing_component` + `mask.tool` on Edit; sets `mask.highlight_component = hovered.or(editing_component)`.
  - `pub(crate) fn radial_with_feather_invert(existing: &MaskComponent, feather: f32, invert: bool) -> Option<MaskComponent>` (rebuilds a radial preserving center/radius/rotation).

- [ ] **Step 1: Write the failing test for the radial rebuild helper**

Add to `mask_components_modal.rs` `#[cfg(test)]`:

```rust
#[test]
fn radial_with_feather_invert_preserves_geometry() {
    use ferrolite_mask::{MaskComponent, Vec2};
    let existing = MaskComponent::RadialGradient {
        center: Vec2::new(0.4, 0.6), radius: Vec2::new(0.25, 0.15),
        rotation: 0.5, feather: 0.3, invert: false,
    };
    let out = radial_with_feather_invert(&existing, 0.8, true).unwrap();
    match out {
        MaskComponent::RadialGradient { center, radius, rotation, feather, invert } => {
            assert_eq!(center, Vec2::new(0.4, 0.6), "center preserved");
            assert_eq!(radius, Vec2::new(0.25, 0.15), "radius preserved");
            assert_eq!(rotation, 0.5, "rotation preserved");
            assert_eq!(feather, 0.8, "feather updated");
            assert!(invert, "invert updated");
        }
        _ => panic!("expected radial"),
    }
    // non-radial → None
    assert!(radial_with_feather_invert(&MaskComponent::Brush { strokes: vec![] }, 0.5, false).is_none());
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p ferrolite-app radial_with_feather_invert_preserves_geometry -- --nocapture`
Expected: FAIL to compile.

- [ ] **Step 3: Add `radial_with_feather_invert` + extend `is_editable` + `load_component_into_state`**

In `mask_components_modal.rs`:

```rust
/// Rebuild a radial component preserving its spatial params (center/radius/
/// rotation — those are edited via canvas handles) and applying new scalar
/// `feather`/`invert` from the inline editor. `None` if `existing` isn't radial.
pub(crate) fn radial_with_feather_invert(
    existing: &MaskComponent,
    feather: f32,
    invert: bool,
) -> Option<MaskComponent> {
    match existing {
        MaskComponent::RadialGradient { center, radius, rotation, .. } => {
            Some(MaskComponent::RadialGradient {
                center: *center, radius: *radius, rotation: *rotation, feather, invert,
            })
        }
        _ => None,
    }
}
```

Change `is_editable` to include the new types:

```rust
fn is_editable(c: &MaskComponent) -> bool {
    matches!(
        c,
        MaskComponent::LumaRange { .. }
            | MaskComponent::ColorRange { .. }
            | MaskComponent::Brush { .. }
            | MaskComponent::LinearGradient { .. }
            | MaskComponent::RadialGradient { .. }
    )
}
```

Extend `load_component_into_state` with a `RadialGradient` arm (seed the inline editor):

```rust
        MaskComponent::RadialGradient { feather, invert, .. } => {
            mask.radial_feather = *feather;
            mask.radial_invert = *invert;
        }
```

- [ ] **Step 4: On Edit, switch the tool to the component's tool**

In the Edit-button click handler (where it currently sets `mask.editing_component = Some(i); mask.overlay_on = true; load_component_into_state(comp, mask);`), also set the tool so the canvas routes the right affordance:

```rust
            mask.editing_component = Some(i);
            mask.overlay_on = true;
            if let Some(t) = crate::develop::mask_ui::tool_for_component(comp) {
                mask.tool = t;
            }
            load_component_into_state(comp, mask);
```

- [ ] **Step 5: Add `edit_component_ui` arms for Radial / Brush / Linear**

Extend `edit_component_ui`'s `match comp` with arms for the three new types (the existing Luma/Color arms stay). Radial gets a Feather slider + Invert toggle + Update/Done; Brush/Linear get a hint + Done:

```rust
        MaskComponent::RadialGradient { .. } => {
            ui.add(EguiSlider {
                label: "Feather", value: &mut mask.radial_feather,
                min: 0.0, max: 1.0, default: 0.3, step: 0.01, decimals: 2,
                unit: "", bipolar: false, signed: false,
            });
            ui.checkbox(&mut mask.radial_invert, "Invert");
            ui.label(
                egui::RichText::new("Drag the center / radius handles on the canvas")
                    .size(11.0).color(crate::theme::TEXT_FAINT),
            );
            ui.horizontal(|ui| {
                if ui.button("Update").clicked() {
                    result = radial_with_feather_invert(comp, mask.radial_feather, mask.radial_invert);
                }
                if ui.button("Done").clicked() {
                    mask.editing_component = None;
                }
            });
        }
        MaskComponent::Brush { .. } => {
            ui.label(
                egui::RichText::new("Paint on the canvas to add to this layer")
                    .size(11.0).color(crate::theme::TEXT_FAINT),
            );
            if ui.button("Done").clicked() {
                mask.editing_component = None;
            }
        }
        MaskComponent::LinearGradient { .. } => {
            ui.label(
                egui::RichText::new("Drag the endpoints on the canvas")
                    .size(11.0).color(crate::theme::TEXT_FAINT),
            );
            if ui.button("Done").clicked() {
                mask.editing_component = None;
            }
        }
```

When `result` is `Some(updated)`, the existing caller commits it via `mask_edit::set_component` (the Luma/Color path already does this — Radial reuses it; Brush/Linear return `None` so nothing commits from the modal, they commit via canvas edits). Confirm the arms sit inside the same `match` whose `result` is committed by the caller at the modal's edit block.

- [ ] **Step 6: Highlight the edited component white when not hovering a row**

Where the row loop currently sets `mask.highlight_component = hovered;` (after the `ScrollArea`), change it to fall back to the edited component:

```rust
            // Hovered row wins (transient); otherwise keep the component being
            // edited highlighted white so the user sees what their canvas edits affect.
            mask.highlight_component = hovered.or(mask.editing_component);
```

- [ ] **Step 7: Build + run tests**

Run: `cargo build -p ferrolite-app --bin ferrolite-app && cargo test -p ferrolite-app radial_with_feather_invert_preserves_geometry`
Expected: compiles; test passes.

- [ ] **Step 8: Format, lint, commit**

Run: `cargo fmt && cargo clippy -p ferrolite-app --all-targets -- -D warnings`

```bash
git add ferrolite-app/src/develop/mask_components_modal.rs
git commit -m "feat(develop): edit brush/linear/radial from the modal (tool switch, radial feather/invert, white highlight)"
```

---

## Task 3: Canvas targeting — overlay handles + brush route the edited component

Route the gradient handles and brush strokes to `editing_component` (via `edit_target_index`), so editing the 2nd+ gradient works and "resume painting" hits the chosen brush layer.

**Files:**
- Modify: `ferrolite-app/src/develop/mask_overlay.rs`

**Interfaces:**
- Consumes: `mask_ui::edit_target_index`, `mask.editing_component`, `mask.tool`, `mask_edit::{layers, last_brush_index, brush_stroke_count, set_brush_with_base}`.

- [ ] **Step 1: Target the edited component in the Linear/Radial handle finders**

In `mask_overlay::show`, the `existing` (Linear) and `existing_radial` (Radial) finders currently `find_map` the *first* matching component. Replace each with an `edit_target_index`-based lookup so the edited component is targeted. For `existing`:

```rust
    let la = mask_edit::layers(stack);
    let comps = la.layers.get(idx).map(|l| l.mask.components.as_slice()).unwrap_or(&[]);
    let existing = crate::develop::mask_ui::edit_target_index(comps, tool, mask.editing_component)
        .filter(|_| tool == MaskTool::Linear)
        .and_then(|i| match &comps[i].0 {
            MaskComponent::LinearGradient { start, end } => Some((i, (start.x, start.y), (end.x, end.y))),
            _ => None,
        });
    let existing_radial = crate::develop::mask_ui::edit_target_index(comps, tool, mask.editing_component)
        .filter(|_| tool == MaskTool::Radial)
        .and_then(|i| match &comps[i].0 {
            MaskComponent::RadialGradient { center, radius, .. } => Some((i, (center.x, center.y), (radius.x, radius.y))),
            _ => None,
        });
```

(`edit_target_index` already only returns a component whose type matches `tool`, so the `match` will succeed; the `.filter(tool == …)` keeps `existing`/`existing_radial` mutually exclusive per the active tool, matching the prior structure.)

- [ ] **Step 2: Target the edited brush component in `route_brush`**

In `route_brush`, the `drag_started` currently sets `Stroke(vec![], None)`. Change the target resolution so an edited brush component wins over `last_brush_index`. In the dragged block's `None` branch (where it currently calls `last_brush_index`), prefer the edited component when it's a brush:

```rust
                None => {
                    // Prefer the component being edited (resume-paint), else the
                    // mask's last brush component, else create a new one.
                    let edited_brush = mask.editing_component.filter(|&i| {
                        matches!(
                            mask_edit::layers(stack).layers.get(idx)
                                .and_then(|l| l.mask.components.get(i)),
                            Some((MaskComponent::Brush { .. }, _))
                        )
                    });
                    match edited_brush.or_else(|| mask_edit::last_brush_index(stack, idx)) {
                        Some(ci) => {
                            let base = mask_edit::brush_stroke_count(stack, idx, ci);
                            *target = Some((ci, base));
                            mask_edit::set_brush_with_base(stack, idx, ci, base, stroke)
                        }
                        None => {
                            let comp = MaskComponent::Brush { strokes: vec![stroke] };
                            let added = mask_edit::add_component(stack, idx, comp, mask.next_mode);
                            let new_idx = mask_edit::layers(&added).layers[idx].mask.components.len() - 1;
                            *target = Some((new_idx, 0));
                            added
                        }
                    }
                }
```

(Note: `mask.editing_component` may borrow-conflict with `&mut mask.gesture` held in the enclosing `if let`. If so, read `let editing = mask.editing_component;` into a local BEFORE the `if let (Some(MaskGesture::Stroke(nodes, target)), …) = (&mut mask.gesture, …)` block and use `editing` inside. Adjust as the borrow checker requires — do NOT clone the whole mask.)

- [ ] **Step 3: Build + run app tests**

Run: `cargo build -p ferrolite-app --bin ferrolite-app && cargo test -p ferrolite-app`
Expected: compiles; tests pass. Fix any borrow issue per the Step-2 note.

- [ ] **Step 4: Format, lint, commit**

Run: `cargo fmt && cargo clippy -p ferrolite-app --all-targets -- -D warnings`

```bash
git add ferrolite-app/src/develop/mask_overlay.rs
git commit -m "feat(develop): route gradient handles + brush strokes to the component being edited"
```

---

## Task 4: Full gate + hand off visual test

- [ ] **Step 1: Full workspace gate**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green. Fix any fallout.

- [ ] **Step 2: Commit any gate fixes (if needed), then STOP for the author's visual test**

The controller hands over this checklist (do NOT merge/finish first):
1. **Edit the 2nd gradient of a mask:** add two linear (or two radial) components; Edit the *second* → its handles (not the first's) move on canvas. (This was impossible before.)
2. **Resume painting a brush layer:** with two brush layers, Edit the *first* → paint → the strokes append to the first layer, not the last.
3. **Radial feather/invert inline:** Edit a radial → Feather slider + Invert toggle change it; center/radius handles still work on canvas and don't reset feather.
4. **White highlight:** the component being edited stays highlighted white on canvas (even with the red overlay off); hovering a different row transiently highlights that one; Done clears it.
5. **Done exits** edit mode; deleting the edited component or closing the modal also clears it (no stuck highlight / no panic).
6. **Regression:** creating a *fresh* gradient/brush (no Edit active) still works as before.

---

## Self-Review

**Spec coverage:**
- §2.1 `is_editable` covers Brush/Linear/Radial → Task 2 Step 3. ✓
- §2.2 Edit activates tool + targets component (`tool_for_component`) → Task 1 + Task 2 Step 4. ✓
- §2.3 canvas targeting prefers `editing_component` (overlay + route_brush via `edit_target_index`) → Task 1 (`edit_target_index`) + Task 3. ✓
- §2.4 radial inline Feather/Invert (preserve geometry) → Task 1 (state) + Task 2 (`radial_with_feather_invert`, editor arm). ✓
- §2.5 Brush/Linear hint + Done → Task 2 Step 5. ✓
- §2.6 white highlight = hovered.or(editing_component) → Task 2 Step 6. ✓
- §4 error handling (bounds-safe target, mismatch fallback, stale index) → `edit_target_index` bounds-checks (Task 1) + `radial_with_feather_invert` returns None on non-radial. ✓
- §5 testing (tool_for_component, edit_target_index, radial rebuild, highlight selection) → Tasks 1/2 unit tests; visual → Task 4. ✓

**Placeholder scan:** none — full code for each step. The one adaptive spot (Task 3 Step 2 borrow) is explicitly directed with the exact remedy (hoist `editing` into a local).

**Type consistency:** `tool_for_component(&MaskComponent) -> Option<MaskTool>` and `edit_target_index(&[(MaskComponent, CompositeMode)], MaskTool, Option<usize>) -> Option<usize>` defined in Task 1, consumed identically in Tasks 2/3. `radial_with_feather_invert(&MaskComponent, f32, bool) -> Option<MaskComponent>` defined + consumed in Task 2. `radial_feather: f32`/`radial_invert: bool` consistent across Tasks 1/2. `highlight_component`/`editing_component: Option<usize>` used consistently.
