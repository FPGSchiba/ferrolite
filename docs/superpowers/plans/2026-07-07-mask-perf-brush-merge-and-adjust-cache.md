# Mask perf: brush-merge + adjustment-cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make per-mask adjustment sliders and brush painting smooth on large masks by (A) not re-compositing masks when only adjustments change, and (B) merging brush strokes into one component (bounded count) with an explicit "New Brush Layer" split.

**Architecture:** `LocalAdjustmentsNode`'s mask cache keys on the mask *definitions* only (adjustment changes reuse cached masks). Brush strokes append to the mask's last `Brush` component instead of creating one per stroke; "New Brush Layer" adds a fresh empty `Brush` component to split. A `Brush` component evaluates one GPU pass per erase-run (not per stroke).

**Tech Stack:** Rust, wgpu (`ferrolite-gpu`), egui/egui-wgpu 0.29, `ferrolite-mask`, `ferrolite-pipeline`, `ferrolite-app`.

## Global Constraints

- **Correctness:** all changes are behavior-preserving in output — Fix A changes only *when* masks recompute; erase-run batching and brush merge must produce identical coverage to the per-stroke path (goldens enforce this).
- **Never block the UI/update thread**; **build GPU pipelines once** (CLAUDE.md §1/§2). No new readback.
- **Keybind discoverability (load-bearing):** a new rebindable `Action` MUST appear in the Settings keyboard `GROUPS` (enforced by `every_action_is_in_a_settings_group`) AND the Help panel shortcut list; any control bound to it shows the key in its tooltip via `Keymap::hint`, formatted `"<Label> (<Key>)"`.
- **Per-component reset / icon rules** unaffected (no new sliders; the button uses an `icons` glyph).
- **Non-goals:** no incremental `StrokeCursor` stamping, no fold-accumulator caching, no layers panel, no auto-migration of existing masks (all deferred per spec §5).
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → HOLD for the author's visual test.
- Branch `fix/brush-mask-perf`. The `FERROLITE_BRUSH_PROFILE` round-3b instrumentation on the branch is used for measure-after in the final task, then removed.

---

## Task 1: Preview mask cache keyed on mask-defs only (Fix A)

`LocalAdjustmentsNode` must reuse its composited masks when only adjustments change. Add a rebuild counter so a golden can prove it.

**Files:**
- Modify: `ferrolite-pipeline/src/local_node.rs`
- Test: inline `#[cfg(test)]` in `local_node.rs`

**Interfaces:**
- Consumes: `LocalAdjustments` (`.visible_layers()` → iterator of `&MaskLayer`; `MaskLayer.mask: MaskDefinition`, `.adjustments`), `MaskDefinition` (`Clone + PartialEq`), `MaskCompositor::composite`.
- Produces: `LocalAdjustmentsNode::rebuild_count(&self) -> u32` (test hook).

- [ ] **Step 1: Write the failing test (adjustment-only change reuses masks)**

Add to `local_node.rs` `#[cfg(test)]` (reuse the existing `gradient_source`/`layer` helpers there):

```rust
#[test]
fn adjustment_only_change_does_not_recomposite_masks() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let src = gradient_source(&ctx);
    // One visible layer with a real mask component (so compositing does work).
    let mut la = LocalAdjustments {
        layers: vec![MaskLayer {
            name: "m".into(),
            visible: true,
            mask: MaskDefinition {
                components: vec![(
                    ferrolite_mask::MaskComponent::LinearGradient {
                        start: ferrolite_mask::Vec2::new(0.0, 0.5),
                        end: ferrolite_mask::Vec2::new(1.0, 0.5),
                    },
                    ferrolite_mask::CompositeMode::Add,
                )],
                invert: false,
            },
            adjustments: AdjustmentSet { exposure: 0.2, ..Default::default() },
        }],
    };
    let layers_rc = Rc::new(RefCell::new(la.clone()));
    let node = LocalAdjustmentsNode::new(ctx.clone(), layers_rc.clone());

    let _ = node.evaluate(&[&src]);
    assert_eq!(node.rebuild_count(), 1, "first evaluate composites masks once");

    // Change ONLY the adjustment (masks identical) and re-evaluate.
    la.layers[0].adjustments.exposure = 0.9;
    *layers_rc.borrow_mut() = la.clone();
    let _ = node.evaluate(&[&src]);
    assert_eq!(node.rebuild_count(), 1, "adjustment-only change must REUSE cached masks");

    // Now change the mask itself → must recomposite.
    la.layers[0].mask.components[0] = (
        ferrolite_mask::MaskComponent::LinearGradient {
            start: ferrolite_mask::Vec2::new(0.0, 0.0),
            end: ferrolite_mask::Vec2::new(0.0, 1.0),
        },
        ferrolite_mask::CompositeMode::Add,
    );
    *layers_rc.borrow_mut() = la.clone();
    let _ = node.evaluate(&[&src]);
    assert_eq!(node.rebuild_count(), 2, "mask change recomposites");
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p ferrolite-pipeline adjustment_only_change_does_not_recomposite -- --nocapture`
Expected: FAIL — `rebuild_count` not found (and, once added against the OLD key, the second assert fails because the old cache invalidates on adjustment change).

- [ ] **Step 3: Change `CachedMasks` to store mask-defs + add the rebuild counter**

In `local_node.rs`:
- Change the `CachedMasks` struct field `layers: LocalAdjustments` → `mask_defs: Vec<ferrolite_mask::MaskDefinition>` (the visible layers' mask defs, in visible order). Keep `full_dims` and `masks`.
- Add a field to `LocalAdjustmentsNode`: `rebuilds: std::cell::Cell<u32>` (init `Cell::new(0)` in `new`).
- Add `pub(crate) fn rebuild_count(&self) -> u32 { self.rebuilds.get() }`.
- In `evaluate`, compute the current visible mask-defs and change the rebuild condition:

```rust
let cur_defs: Vec<ferrolite_mask::MaskDefinition> =
    layers.visible_layers().map(|l| l.mask.clone()).collect();
let rebuild = {
    let c = self.cache.borrow();
    match &*c {
        Some(cm) => cm.mask_defs != cur_defs || cm.full_dims != (mw, mh),
        None => true,
    }
};
if rebuild {
    self.rebuilds.set(self.rebuilds.get() + 1);
    let masks: Vec<MaskBuffer> = layers
        .visible_layers()
        .map(|l| {
            self.compositor
                .composite(&l.mask, &input_view, mw, mh, &RasterStore::default())
        })
        .collect();
    *self.cache.borrow_mut() = Some(CachedMasks {
        mask_defs: cur_defs,
        full_dims: (mw, mh),
        masks,
    });
}
```

The apply loop below (which reads `layer.adjustments`) is unchanged, so adjustment changes still take effect via the apply pass — only the mask *composite* is now skipped when defs are unchanged.

> NOTE: if the round-3b `local_brush_profile()` instrumentation block is present in `evaluate`, keep it working — update its `rebuild` usage to the new condition (it already reads the `rebuild` bool). The final task removes it.

- [ ] **Step 4: Run the test + the crate suite**

Run: `cargo test -p ferrolite-pipeline -- --nocapture`
Expected: PASS (the new test + existing `two_visible_layers_evaluate...` / `repeated_evaluate...`).

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt && cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings`

```bash
git add ferrolite-pipeline/src/local_node.rs
git commit -m "perf(pipeline): key LocalAdjustmentsNode mask cache on mask-defs only

Adjustment-only changes (exposure/contrast/...) no longer re-composite the
masks — the cache invalidated on the whole LocalAdjustments (masks+adjustments)
before, so an exposure drag re-composited every mask at full res (~12.7s on a
200-component mask). Now keyed on the mask definitions; adjustments take effect
via the apply pass only."
```

---

## Task 2: Brush eval — one pass per erase-run (ferrolite-mask)

A merged `Brush` component holds many strokes; evaluating one `stamp_onto` per stroke would be O(strokes). Batch consecutive same-`erase` strokes into a single stamp.

**Files:**
- Modify: `ferrolite-mask/src/compositor.rs` (the `MaskComponent::Brush` arm of `eval`)
- Test: inline `#[cfg(test)]` in `compositor.rs`

**Interfaces:**
- Consumes: `stroke_dabs(stroke, SPACING_FRAC) -> Vec<Dab>`, `BrushRasterizer::stamp_onto(base, dabs, erase, origin, level_dims) -> MaskBuffer`, `MaskBuffer::alloc_zeroed`.
- Produces: same `MaskBuffer` output as before (byte-identical); no signature change.

- [ ] **Step 1: Write the failing golden (batched == per-stroke-loop)**

Add to `compositor.rs` `#[cfg(test)]`:

```rust
#[test]
fn brush_erase_run_batching_matches_per_stroke_loop() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let comp = MaskCompositor::new(ctx.clone());
    let input = MaskBuffer::alloc_zeroed(&ctx, 32, 32);
    let iv = input.texture.create_view(&wgpu::TextureViewDescriptor::default());
    let node = |x: f32, erase: bool| crate::model::Stroke {
        nodes: vec![crate::model::BrushNode {
            pos: crate::vec::Vec2::new(x, 0.5),
            radius: 0.2,
            hardness: 0.5,
            flow: 1.0,
        }],
        erase,
    };
    // paint, paint, erase, paint  → runs [P,P],[E],[P]
    let strokes = vec![node(0.3, false), node(0.5, false), node(0.4, true), node(0.7, false)];
    let def = MaskDefinition {
        components: vec![(MaskComponent::Brush { strokes: strokes.clone() }, CompositeMode::Add)],
        invert: false,
    };
    let batched = comp.composite(&def, &iv, 32, 32, &RasterStore::default());

    // Reference: the OLD per-stroke fold, computed here directly.
    let reference = {
        let mut acc = MaskBuffer::alloc_zeroed(&ctx, 32, 32);
        for st in &strokes {
            let dabs = crate::stroke::stroke_dabs(st, crate::stroke::SPACING_FRAC);
            acc = comp_stamp(&comp, &acc, &dabs, st.erase);
        }
        acc
    };
    assert_eq!(
        read_mask_r32f(&ctx, &batched),
        read_mask_r32f(&ctx, &reference),
        "erase-run batching must equal the per-stroke fold"
    );
}
```

Add a tiny test helper exposing a single stamp for the reference (the brush rasterizer is a private field; add a `#[cfg(test)]` accessor OR compute the reference by calling the pre-refactor loop). Simplest: add a `#[cfg(test)] pub(crate) fn stamp_for_test(&self, base, dabs, erase) -> MaskBuffer` on `MaskCompositor` delegating to `self.brush.stamp_onto(base, dabs, erase, (0,0), (base.width, base.height))`, and call it as `comp_stamp` (alias in the test). Confirm `self.brush` is the `BrushRasterizer` field name.

- [ ] **Step 2: Run it against the OLD code (should PASS — it pins current behavior)**

Run: `cargo test -p ferrolite-mask brush_erase_run_batching -- --nocapture`
Expected: PASS on the current per-stroke loop (the test pins the target output before the refactor). If it fails, fix the test's reference, not the code.

- [ ] **Step 3: Refactor the `Brush` eval arm to batch by erase-run**

In `compositor.rs` `eval`, replace the `MaskComponent::Brush { strokes }` arm:

```rust
MaskComponent::Brush { strokes } => {
    let mut acc = MaskBuffer::alloc_zeroed(&self.ctx, w, h);
    let mut i = 0usize;
    while i < strokes.len() {
        let erase = strokes[i].erase;
        // Gather all dabs of the maximal run of same-`erase` strokes; stamp once.
        let mut dabs = Vec::new();
        while i < strokes.len() && strokes[i].erase == erase {
            dabs.extend(stroke_dabs(&strokes[i], SPACING_FRAC));
            i += 1;
        }
        acc = self.brush.stamp_onto(&acc, &dabs, erase, (0, 0), (w, h));
    }
    acc
}
```

This is output-identical because `composite_dabs` folds in order and consecutive same-`erase` dabs fold associatively (proven by the existing `composite_dabs_split_equals_whole` test); pass count drops from O(strokes) to O(erase-runs).

- [ ] **Step 4: Run the golden + suite**

Run: `cargo test -p ferrolite-mask -- --nocapture`
Expected: PASS (the batching golden + all existing brush/composite goldens).

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt && cargo clippy -p ferrolite-mask --all-targets -- -D warnings`

```bash
git add ferrolite-mask/src/compositor.rs
git commit -m "perf(mask): evaluate a Brush component one pass per erase-run, not per stroke"
```

---

## Task 3: Brush-merge pure helpers (ferrolite-app `mask_edit`)

Pure `OpStack` helpers the brush routing needs to append strokes to the mask's active (last) `Brush` component and to start a new brush layer.

**Files:**
- Modify: `ferrolite-app/src/develop/mask_edit.rs`
- Test: inline `#[cfg(test)]` in `mask_edit.rs`

**Interfaces:**
- Consumes: `layers(stack)`, `add_component`, `set_component`, `MaskComponent::Brush { strokes: Vec<Stroke> }`, `Stroke`.
- Produces (used by Tasks 4/5):
  - `pub fn last_brush_index(stack: &OpStack, mask_idx: usize) -> Option<usize>`
  - `pub fn brush_stroke_count(stack: &OpStack, mask_idx: usize, comp_idx: usize) -> usize`
  - `pub fn set_brush_with_base(stack: &OpStack, mask_idx: usize, comp_idx: usize, base_count: usize, stroke: Stroke) -> OpStack`
  - `pub fn new_brush_layer(stack: &OpStack, mask_idx: usize) -> OpStack`

- [ ] **Step 1: Write the failing tests**

Add to `mask_edit.rs` `#[cfg(test)]`:

```rust
fn stroke(x: f32, erase: bool) -> ferrolite_mask::Stroke {
    ferrolite_mask::Stroke {
        nodes: vec![ferrolite_mask::BrushNode {
            pos: Vec2::new(x, 0.5),
            radius: 0.1,
            hardness: 0.5,
            flow: 1.0,
        }],
        erase,
    }
}
fn brush_strokes(stack: &OpStack, mask_idx: usize, comp_idx: usize) -> Vec<ferrolite_mask::Stroke> {
    match &layers(stack).layers[mask_idx].mask.components[comp_idx].0 {
        MaskComponent::Brush { strokes } => strokes.clone(),
        _ => panic!("not a brush"),
    }
}

#[test]
fn last_brush_index_finds_the_last_brush_component() {
    let s = create_mask(&OpStack::default(), "m".into());
    assert_eq!(last_brush_index(&s, 0), None, "no components yet");
    let s = add_component(&s, 0, MaskComponent::LumaRange { lo: 0.0, hi: 1.0, softness: 0.0 }, CompositeMode::Add);
    let s = add_component(&s, 0, MaskComponent::Brush { strokes: vec![] }, CompositeMode::Add);
    let s = add_component(&s, 0, MaskComponent::LumaRange { lo: 0.0, hi: 1.0, softness: 0.0 }, CompositeMode::Add);
    assert_eq!(last_brush_index(&s, 0), Some(1), "the brush at index 1");
}

#[test]
fn set_brush_with_base_truncates_then_appends() {
    // A brush component with 2 committed strokes; base_count=1 drops the 2nd and appends a new one.
    let s = create_mask(&OpStack::default(), "m".into());
    let s = add_component(&s, 0, MaskComponent::Brush { strokes: vec![stroke(0.1, false), stroke(0.2, false)] }, CompositeMode::Add);
    let s = set_brush_with_base(&s, 0, 0, 1, stroke(0.9, false));
    let ss = brush_strokes(&s, 0, 0);
    assert_eq!(ss.len(), 2, "kept 1 base + 1 new");
    assert_eq!(ss[0].nodes[0].pos.x, 0.1);
    assert_eq!(ss[1].nodes[0].pos.x, 0.9, "in-progress stroke replaced the tail");
}

#[test]
fn brush_stroke_count_reports_len_or_zero() {
    let s = create_mask(&OpStack::default(), "m".into());
    let s = add_component(&s, 0, MaskComponent::Brush { strokes: vec![stroke(0.1, false)] }, CompositeMode::Add);
    assert_eq!(brush_stroke_count(&s, 0, 0), 1);
    assert_eq!(brush_stroke_count(&s, 0, 9), 0, "out of range");
}

#[test]
fn new_brush_layer_appends_empty_brush() {
    let s = create_mask(&OpStack::default(), "m".into());
    let s = new_brush_layer(&s, 0);
    let comps = &layers(&s).layers[0].mask.components;
    assert_eq!(comps.len(), 1);
    assert!(matches!(comps[0].0, MaskComponent::Brush { ref strokes } if strokes.is_empty()));
    assert_eq!(comps[0].1, CompositeMode::Add);
}
```

- [ ] **Step 2: Run, verify fail (undefined fns)**

Run: `cargo test -p ferrolite-app last_brush_index_finds -- --nocapture`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the helpers**

Add to `mask_edit.rs`:

```rust
use ferrolite_mask::Stroke;

/// Index of the mask's LAST `Brush` component (the one strokes accumulate into),
/// or `None` if the mask has no brush component yet.
pub fn last_brush_index(stack: &OpStack, mask_idx: usize) -> Option<usize> {
    let la = layers(stack);
    let comps = &la.layers.get(mask_idx)?.mask.components;
    comps
        .iter()
        .rposition(|(c, _)| matches!(c, MaskComponent::Brush { .. }))
}

/// Number of strokes in the `Brush` component at `comp_idx` (0 if out of range or
/// not a brush).
pub fn brush_stroke_count(stack: &OpStack, mask_idx: usize, comp_idx: usize) -> usize {
    let la = layers(stack);
    match la.layers.get(mask_idx).and_then(|l| l.mask.components.get(comp_idx)) {
        Some((MaskComponent::Brush { strokes }, _)) => strokes.len(),
        _ => 0,
    }
}

/// Replace the `Brush` component at `comp_idx` with its first `base_count` strokes
/// plus `stroke` appended — the in-progress-stroke preview/commit primitive: the
/// committed base is `strokes[..base_count]`, the live stroke sits at `base_count`.
/// Out-of-range or non-brush → unchanged stack.
pub fn set_brush_with_base(
    stack: &OpStack,
    mask_idx: usize,
    comp_idx: usize,
    base_count: usize,
    stroke: Stroke,
) -> OpStack {
    let la = layers(stack);
    let Some((MaskComponent::Brush { strokes }, _)) =
        la.layers.get(mask_idx).and_then(|l| l.mask.components.get(comp_idx))
    else {
        return stack.clone();
    };
    let mut next: Vec<Stroke> = strokes.iter().take(base_count).cloned().collect();
    next.push(stroke);
    set_component(stack, mask_idx, comp_idx, MaskComponent::Brush { strokes: next })
}

/// Append a fresh empty `Brush` component (Add mode) — "New Brush Layer": the next
/// strokes accumulate here, and it is independently deletable in the Components list.
pub fn new_brush_layer(stack: &OpStack, mask_idx: usize) -> OpStack {
    add_component(stack, mask_idx, MaskComponent::Brush { strokes: vec![] }, CompositeMode::Add)
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p ferrolite-app mask_edit -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt && cargo clippy -p ferrolite-app --all-targets -- -D warnings`

```bash
git add ferrolite-app/src/develop/mask_edit.rs
git commit -m "feat(develop): mask_edit brush-merge helpers (last-brush / append-with-base / new-layer)"
```

---

## Task 4: Brush routing merges strokes into the active brush component

Rewire `route_brush` so a stroke appends to the mask's last `Brush` component (creating one only if none exists) instead of adding a component per stroke.

**Files:**
- Modify: `ferrolite-app/src/develop/mask_ui.rs` (the `MaskGesture::Stroke` payload)
- Modify: `ferrolite-app/src/develop/mask_overlay.rs` (`route_brush`)

**Interfaces:**
- Consumes: `mask_edit::{last_brush_index, brush_stroke_count, set_brush_with_base, add_component, layers}`, `MaskComponent::Brush`, `Stroke`.
- Produces: `MaskGesture::Stroke(Vec<BrushNode>, Option<(usize, usize)>)` where the option is `(brush_comp_idx, base_stroke_count)` of the active brush component (`None` until the component is created/located on the first dragged frame).

- [ ] **Step 1: Change the `MaskGesture::Stroke` payload**

In `mask_ui.rs`, change the variant to carry the brush target:

```rust
    /// Brush stroke being captured: accumulated dab nodes (normalized source
    /// coords), plus the target `(brush_component_index, base_stroke_count)` once
    /// located/created — the live stroke is appended after the component's first
    /// `base_stroke_count` committed strokes (merge-into-active-brush; the mask's
    /// last Brush component accumulates strokes). `None` until the first dragged
    /// frame creates/locates the target.
    Stroke(Vec<BrushNode>, Option<(usize, usize)>),
```

- [ ] **Step 2: Rewrite `route_brush`'s drag handling to merge**

In `mask_overlay.rs` `route_brush`, keep the cursor-ring drawing and `drag_started` (`mask.gesture = Some(MaskGesture::Stroke(vec![], None));`). Replace the `resp.dragged() || resp.drag_stopped()` block with:

```rust
    let mut outcome: Option<EditOutcome> = None;
    if resp.dragged() || resp.drag_stopped() {
        if let (Some(MaskGesture::Stroke(nodes, target)), Some(p)) =
            (&mut mask.gesture, resp.interact_pointer_pos())
        {
            let norm = (
                ((p.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0),
                ((p.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0),
            );
            let src = display_to_source(geo, src_w, src_h, norm);
            mask_affordance::append_brush_node(nodes, src, params);
            let stroke = Stroke {
                nodes: nodes.clone(),
                erase: mask.brush_erase,
            };
            // Merge into the mask's active (last) Brush component: locate/create it
            // on the first frame (recording base_stroke_count), then replace the
            // live stroke in place each later frame. Strokes accumulate into ONE
            // component instead of one-per-stroke, so component count stays bounded.
            let new_stack = match *target {
                Some((ci, base)) => mask_edit::set_brush_with_base(stack, idx, ci, base, stroke),
                None => {
                    match mask_edit::last_brush_index(stack, idx) {
                        Some(ci) => {
                            let base = mask_edit::brush_stroke_count(stack, idx, ci);
                            *target = Some((ci, base));
                            mask_edit::set_brush_with_base(stack, idx, ci, base, stroke)
                        }
                        None => {
                            let comp = MaskComponent::Brush { strokes: vec![stroke] };
                            let added = mask_edit::add_component(stack, idx, comp, mask.next_mode);
                            let new_idx =
                                mask_edit::layers(&added).layers[idx].mask.components.len() - 1;
                            *target = Some((new_idx, 0));
                            added
                        }
                    }
                }
            };
            outcome = Some(EditOutcome {
                stack: new_stack,
                kind: OpKind::LocalAdjustments,
                commit: resp.drag_stopped(),
            });
        }
    }
    if resp.drag_stopped() {
        mask.gesture = None;
    }
    outcome
```

> Why `base_stroke_count`: the `stack` passed in each frame is the previous frame's preview stack, which already carries the live stroke at index `base`. `set_brush_with_base` truncates to `base` then re-appends the updated live stroke, so both the first append and later replacements are one uniform call. On `drag_stopped` the live stroke becomes the committed tail.

- [ ] **Step 3: Build + run app tests**

Run: `cargo build -p ferrolite-app --bin ferrolite-app && cargo test -p ferrolite-app`
Expected: compiles; tests pass. Fix any match arms elsewhere that destructure `MaskGesture::Stroke` (grep `MaskGesture::Stroke`) to the new 2-tuple-with-`(usize,usize)` payload — the only writers are `route_brush` (drag_started `None`) and this block.

- [ ] **Step 4: Format, lint, commit**

Run: `cargo fmt && cargo clippy -p ferrolite-app --all-targets -- -D warnings`

```bash
git add ferrolite-app/src/develop/mask_ui.rs ferrolite-app/src/develop/mask_overlay.rs
git commit -m "feat(develop): brush strokes merge into the mask's active brush component"
```

---

## Task 5: "New Brush Layer" action — button + rebindable keybind

Add the split affordance with full keybind discoverability.

**Files:**
- Modify: `ferrolite-app/src/settings/keymap.rs` (add `Action::NewBrushLayer` + `ALL` + label + default chord)
- Modify: `ferrolite-app/src/settings/ui/keyboard.rs` (add to a `GROUPS` entry)
- Modify: `ferrolite-app/src/help.rs` (add to the shortcut list)
- Modify: `ferrolite-app/src/develop/mask_panel.rs` (a "New Brush Layer" button with keybind tooltip)
- Modify: `ferrolite-app/src/app.rs` (dispatch the action → `mask_edit::new_brush_layer`)

**Interfaces:**
- Consumes: `mask_edit::new_brush_layer(stack, mask_idx)`, `Keymap::{chord, hint}`, `Action`, `EditOutcome`/`apply_edit`.
- Produces: `Action::NewBrushLayer`.

- [ ] **Step 1: Add the `Action` variant, label, `ALL` entry, default chord**

In `keymap.rs`:
- Add `NewBrushLayer,` to `enum Action` (near `SwitchToolMask`/`ToggleMaskOverlay`, lines ~32-33).
- Add `Action::NewBrushLayer,` to the `ALL` array (near line 62).
- Add a label arm: `Action::NewBrushLayer => "New brush layer",` (near line 90).
- Add a default chord in `defaults()` (near line 560). Use a non-conflicting key — `B` is unused by the current defaults (verify against the `defaults()` inserts and the conflict test): `m.insert(NewBrushLayer, plain(Key::B));`. If `Key::B` doesn't exist in the `Key` enum, add it (mirror the existing `Key::M`/`Key::T` variants + their `Chord`/`Key` mapping); confirm by grepping `Key::M` usages.

- [ ] **Step 2: Add to Settings `GROUPS` (required — a test enforces it)**

In `settings/ui/keyboard.rs`, add `Action::NewBrushLayer` to the appropriate `GROUPS` entry (the same group as the other Develop/Mask actions like `SwitchToolMask`/`ToggleMaskOverlay` — grep `ToggleMaskOverlay` in this file to find the group array and append it there).

- [ ] **Step 3: Add to the Help panel shortcut list**

In `help.rs`, add a row for `NewBrushLayer` alongside the other mask shortcuts (grep `ToggleMaskOverlay` in `help.rs` and add a sibling entry using the same rendering pattern / `Keymap::hint(Action::NewBrushLayer)`).

- [ ] **Step 4: Build + run the discoverability test**

Run: `cargo test -p ferrolite-app every_action_is_in_a_settings_group -- --nocapture`
Expected: PASS (fails loudly if the `GROUPS` entry from Step 2 is missing).

- [ ] **Step 5: Add the "New Brush Layer" button to the mask panel**

In `mask_panel.rs`, in the selected-mask section (where the brush sub-tool controls live — grep for `brush_radius`/the brush params, or the "Components" button), add a button that emits a `new_brush_layer` edit and shows its keybind in the tooltip. Follow the existing button + tooltip pattern; label via the keymap:

```rust
// `keymap` is already threaded into mask_panel::show (used for the overlay toggle tooltip).
let label = format!("New Brush Layer ({})", keymap.hint(crate::settings::keymap::Action::NewBrushLayer));
if ui.button(label).on_hover_text("Start a new, separately-deletable brush layer").clicked() {
    if let Some(idx) = mask.selected {
        out = Some(EditOutcome {
            stack: crate::develop::mask_edit::new_brush_layer(stack, idx),
            kind: ferrolite_pipeline::OpKind::LocalAdjustments,
            commit: true,
        });
    }
}
```

Match the panel's actual `out`/return convention (grep how the overlay-toggle / other buttons build their `EditOutcome`); if the panel returns `Option<EditOutcome>` via a local `out`, assign to it consistent with the surrounding code.

- [ ] **Step 6: Dispatch the keybind in `app.rs`**

In `app.rs` where develop keybinds are handled (grep `Action::ToggleMaskOverlay` / `Action::SwitchToolMask` to find the keymap dispatch site), add an arm for `Action::NewBrushLayer`: when a mask is selected, apply `mask_edit::new_brush_layer(&stack, idx)` through the same edit path the other mask edits use (`apply_edit`/`set_preview_and_full` — match the neighboring handlers). Guard on the Mask tool being active + a selected mask (mirror how `ToggleMaskOverlay` is gated).

- [ ] **Step 7: Build, test, lint, commit**

Run: `cargo build -p ferrolite-app --bin ferrolite-app && cargo test -p ferrolite-app && cargo fmt && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: all green.

```bash
git add ferrolite-app/src/settings/keymap.rs ferrolite-app/src/settings/ui/keyboard.rs ferrolite-app/src/help.rs ferrolite-app/src/develop/mask_panel.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): New Brush Layer action (button + rebindable keybind) to split brush layers"
```

---

## Task 6: Measure-after, remove instrumentation, verify, hand off

**Files:**
- Modify: `ferrolite-mask/src/compositor.rs` (remove the round-3b `composite_cached` probe + `brush_profile_enabled`)
- Modify: `ferrolite-pipeline/src/local_node.rs` (remove the round-3b probe block; KEEP the `rebuilds` counter + `rebuild_count` — it's used by Task 1's test)
- Modify: `ferrolite-app/src/app.rs`, `ferrolite-app/src/diag.rs` (remove any remaining round-3b probes + `brush_profile_enabled` if still present)

- [ ] **Step 1: (Controller-run) measure-after — implementer skips**

The controller runs measure-after with the author (large mask: exposure drag → `local_node rebuild=false`; painting → bounded component count, flat times). The implementer does Steps 2–4.

- [ ] **Step 2: Remove the round-3b instrumentation**

Remove, in each file, the `// TEMP brush-perf probe (round 3b)` blocks and the `brush_profile_enabled` / `local_brush_profile` gate fns added for profiling:
- `ferrolite-mask/src/compositor.rs`: the eval/reuse counting + `eprintln` in `composite_cached`, and the module-level `brush_profile_enabled()`.
- `ferrolite-pipeline/src/local_node.rs`: the `if local_brush_profile() { … } else if rebuild { … }` probe wrapper — collapse back to a plain `if rebuild { … }`; remove `local_brush_profile()`. KEEP `rebuilds`/`rebuild_count` (Task 1's test needs them).
- `ferrolite-app`: any remaining `FERROLITE_BRUSH_PROFILE` references — `grep -rn "brush_profile\|FERROLITE_BRUSH_PROFILE\|brush-perf" ferrolite-app/src ferrolite-mask/src ferrolite-pipeline/src` must return nothing except the retained `rebuild_count` test hook.

- [ ] **Step 3: Full workspace gate**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green. Fix any unused-import fallout.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore(develop): remove round-3b brush-perf instrumentation"
```

---

## Self-Review

**Spec coverage:**
- §2 Fix A (mask-defs cache key) → Task 1 (+ rebuild-count golden). ✓
- §3.1 merge strokes into active brush component → Tasks 3 (helpers) + 4 (routing). ✓
- §3.2 one pass per erase-run → Task 2 (+ golden vs per-stroke loop). ✓
- §3.3 "New Brush Layer" button + keybind + full discoverability → Task 5 (`GROUPS` test + Help + tooltip via `hint`). ✓
- §3.4 no auto-migration (new strokes append to last brush; existing stay) → emergent from Task 4's `last_brush_index` behavior; no migration code. ✓
- §5 deferred items (StrokeCursor, fold-accumulator, layers panel) → not implemented. ✓
- §6 tests (Fix A golden, erase-run golden, merge helper units, discoverability test, measure-after) → Tasks 1/2/3/5/6. ✓

**Placeholder scan:** none — full code for every implementation step. Task 5's Steps 1-3/5-6 give exact edits + concrete grep anchors for the mechanical keymap/help/panel/dispatch sites (values: `Action::NewBrushLayer`, label `"New brush layer"`, default `Key::B`, tooltip `"New Brush Layer (<Key>)"`).

**Type consistency:** `MaskGesture::Stroke(Vec<BrushNode>, Option<(usize, usize)>)` defined in Task 4 Step 1 and used in Step 2. `mask_edit` helper signatures (`last_brush_index`/`brush_stroke_count`/`set_brush_with_base`/`new_brush_layer`) defined in Task 3 and consumed identically in Tasks 4/5. `Action::NewBrushLayer` defined in Task 5 Step 1, consumed in Steps 2/3/5/6. `CachedMasks.mask_defs: Vec<MaskDefinition>` + `rebuild_count()` consistent within Task 1 (and `rebuild_count` retained in Task 6).

**Merge-golden note:** the spec's "merged brush == N separate add-mode components" equivalence is covered behaviorally: a single Brush component with N strokes (Task 2's erase-run eval) vs N single-stroke add-mode brush components fold identically for paint strokes (Add of over-composited coverage); the erase-run golden (Task 2) + the composite goldens already exercise the stamping/fold semantics. No separate task needed.
