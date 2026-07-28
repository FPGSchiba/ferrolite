//! Pure helpers mapping a Masking-UI action to a new immutable `OpStack`. A
//! `LocalAdjustments` with zero layers REMOVES the op (reset) so
//! `is_identity()`/`has_edits` stay correct — mirroring `ops_edit`. All edits
//! carry `OpKind::LocalAdjustments`; the app pushes one history entry per gesture.

use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition, Stroke};
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

/// Replace the component at `comp_idx` within the mask layer at `mask_idx`,
/// keeping its composite mode. Out-of-range `mask_idx` or `comp_idx` → unchanged
/// stack (mirrors `edit_layer`'s out-of-range behavior).
pub fn set_component(
    stack: &OpStack,
    mask_idx: usize,
    comp_idx: usize,
    c: MaskComponent,
) -> OpStack {
    let mut la = layers(stack);
    let Some(layer) = la.layers.get_mut(mask_idx) else {
        return stack.clone();
    };
    let Some(entry) = layer.mask.components.get_mut(comp_idx) else {
        return stack.clone();
    };
    entry.0 = c;
    write(stack, la)
}

pub fn set_adjustments(stack: &OpStack, idx: usize, a: AdjustmentSet) -> OpStack {
    stack.with_layer_adjustments(idx, a)
}

/// The mask definition AS IT WOULD BE with `tentative` folded in at `mode` after the
/// existing `base` components — used to preview an in-progress "add component"
/// (Task 6) without touching the committed `OpStack`.
pub fn prospective_def(
    base: &MaskDefinition,
    tentative: MaskComponent,
    mode: CompositeMode,
) -> MaskDefinition {
    let mut def = base.clone();
    def.components.push((tentative, mode));
    def
}

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
    match la
        .layers
        .get(mask_idx)
        .and_then(|l| l.mask.components.get(comp_idx))
    {
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
    let Some((MaskComponent::Brush { strokes }, _)) = la
        .layers
        .get(mask_idx)
        .and_then(|l| l.mask.components.get(comp_idx))
    else {
        return stack.clone();
    };
    let mut next: Vec<Stroke> = strokes.iter().take(base_count).cloned().collect();
    next.push(stroke);
    set_component(
        stack,
        mask_idx,
        comp_idx,
        MaskComponent::Brush { strokes: next },
    )
}

/// Append a fresh empty `Brush` component — "New Brush Layer": the next strokes
/// accumulate here, and it is independently deletable in the Components list.
/// Wired into the "New Brush Layer" button (`mask_panel.rs`) and its rebindable
/// keybind (`Action::NewBrushLayer`, dispatched in `app.rs`).
///
/// Mode is deliberately `Add` (mode-neutral): an explicit new layer starts as a
/// plain additive brush regardless of the Components-modal `next_mode` picker
/// (which governs the *paint* path's first-create + non-brush component adds).
pub fn new_brush_layer(stack: &OpStack, mask_idx: usize) -> OpStack {
    add_component(
        stack,
        mask_idx,
        MaskComponent::Brush { strokes: vec![] },
        CompositeMode::Add,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_mask::{CompositeMode, MaskComponent, Vec2};
    use ferrolite_pipeline::{AdjustmentSet, OpKind, OpStack};

    fn brush() -> MaskComponent {
        MaskComponent::Brush { strokes: vec![] }
    }

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
        assert!(
            s2.local_adjustments().is_none(),
            "empty layers => op removed"
        );
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
        let s = add_component(
            &s,
            0,
            MaskComponent::LinearGradient {
                start: Vec2::new(0.0, 0.0),
                end: Vec2::new(0.0, 1.0),
            },
            CompositeMode::Subtract,
        );
        let comps = &layers(&s).layers[0].mask.components;
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].1, CompositeMode::Subtract);
    }

    #[test]
    fn set_component_replaces_params_in_place() {
        let s = create_mask(&OpStack::default(), "m".into());
        let s = add_component(
            &s,
            0,
            MaskComponent::LinearGradient {
                start: Vec2::new(0.0, 0.0),
                end: Vec2::new(0.0, 1.0),
            },
            CompositeMode::Subtract,
        );
        let s = set_component(
            &s,
            0,
            0,
            MaskComponent::LinearGradient {
                start: Vec2::new(0.2, 0.3),
                end: Vec2::new(0.8, 0.9),
            },
        );
        let comps = &layers(&s).layers[0].mask.components;
        assert_eq!(comps.len(), 1, "replaces in place, does not append");
        assert_eq!(comps[0].1, CompositeMode::Subtract, "mode is preserved");
        match comps[0].0 {
            MaskComponent::LinearGradient { start, end } => {
                assert_eq!(start, Vec2::new(0.2, 0.3));
                assert_eq!(end, Vec2::new(0.8, 0.9));
            }
            _ => panic!("expected LinearGradient"),
        }
    }

    #[test]
    fn set_component_out_of_range_is_a_noop() {
        let s = create_mask(&OpStack::default(), "m".into());
        let s = add_component(&s, 0, brush(), CompositeMode::Add);
        let same = set_component(&s, 0, 9, brush()); // comp_idx 9 doesn't exist
        assert_eq!(same, s, "out-of-range comp_idx returns the stack unchanged");
        let same2 = set_component(&s, 9, 0, brush()); // mask_idx 9 doesn't exist
        assert_eq!(
            same2, s,
            "out-of-range mask_idx returns the stack unchanged"
        );
    }

    #[test]
    fn set_adjustments_replaces_the_layers_set() {
        let s = create_mask(&OpStack::default(), "m".into());
        let a = AdjustmentSet {
            exposure: 0.5,
            ..Default::default()
        };
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
        let s = add_component(
            &create_mask(&OpStack::default(), "m".into()),
            0,
            brush(),
            CompositeMode::Add,
        );
        assert_eq!(
            s.local_adjustments().unwrap().layers[0]
                .mask
                .components
                .len(),
            1
        );
        let _ = OpKind::LocalAdjustments; // kind used by the app when pushing history
    }

    #[test]
    fn remove_component_removes_the_indexed_component() {
        let luma = |lo| MaskComponent::LumaRange {
            lo,
            hi: 1.0,
            softness: 0.1,
        };
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
        let s = create_mask(&OpStack::default(), "m".into());
        let s = add_component(
            &s,
            0,
            MaskComponent::LumaRange {
                lo: 0.1,
                hi: 1.0,
                softness: 0.1,
            },
            CompositeMode::Add,
        );
        assert_eq!(remove_component(&s, 0, 9), s, "bad comp idx -> unchanged");
        assert_eq!(remove_component(&s, 9, 0), s, "bad mask idx -> unchanged");
    }

    #[test]
    fn prospective_def_appends_tentative() {
        use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition};
        let base = MaskDefinition {
            components: vec![(
                MaskComponent::LumaRange {
                    lo: 0.0,
                    hi: 1.0,
                    softness: 0.0,
                },
                CompositeMode::Add,
            )],
            invert: false,
        };
        let t = MaskComponent::LumaRange {
            lo: 0.2,
            hi: 0.7,
            softness: 0.1,
        };
        let out = prospective_def(&base, t.clone(), CompositeMode::Subtract);
        assert_eq!(out.components.len(), 2);
        assert_eq!(out.components[1], (t, CompositeMode::Subtract));
        assert_eq!(out.components[0], base.components[0], "base preserved");
    }

    #[test]
    fn remove_last_component_keeps_the_layer() {
        let s = create_mask(&OpStack::default(), "m".into());
        let s = add_component(
            &s,
            0,
            MaskComponent::LumaRange {
                lo: 0.1,
                hi: 1.0,
                softness: 0.1,
            },
            CompositeMode::Add,
        );
        let out = remove_component(&s, 0, 0);
        assert_eq!(layers(&out).layers.len(), 1, "layer stays");
        assert!(layers(&out).layers[0].mask.components.is_empty());
    }

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
    fn brush_strokes(
        stack: &OpStack,
        mask_idx: usize,
        comp_idx: usize,
    ) -> Vec<ferrolite_mask::Stroke> {
        match &layers(stack).layers[mask_idx].mask.components[comp_idx].0 {
            MaskComponent::Brush { strokes } => strokes.clone(),
            _ => panic!("not a brush"),
        }
    }

    #[test]
    fn last_brush_index_finds_the_last_brush_component() {
        let s = create_mask(&OpStack::default(), "m".into());
        assert_eq!(last_brush_index(&s, 0), None, "no components yet");
        let s = add_component(
            &s,
            0,
            MaskComponent::LumaRange {
                lo: 0.0,
                hi: 1.0,
                softness: 0.0,
            },
            CompositeMode::Add,
        );
        let s = add_component(
            &s,
            0,
            MaskComponent::Brush { strokes: vec![] },
            CompositeMode::Add,
        );
        let s = add_component(
            &s,
            0,
            MaskComponent::LumaRange {
                lo: 0.0,
                hi: 1.0,
                softness: 0.0,
            },
            CompositeMode::Add,
        );
        assert_eq!(last_brush_index(&s, 0), Some(1), "the brush at index 1");
    }

    #[test]
    fn set_brush_with_base_truncates_then_appends() {
        // A brush component with 2 committed strokes; base_count=1 drops the 2nd and appends a new one.
        let s = create_mask(&OpStack::default(), "m".into());
        let s = add_component(
            &s,
            0,
            MaskComponent::Brush {
                strokes: vec![stroke(0.1, false), stroke(0.2, false)],
            },
            CompositeMode::Add,
        );
        let s = set_brush_with_base(&s, 0, 0, 1, stroke(0.9, false));
        let ss = brush_strokes(&s, 0, 0);
        assert_eq!(ss.len(), 2, "kept 1 base + 1 new");
        assert_eq!(ss[0].nodes[0].pos.x, 0.1);
        assert_eq!(
            ss[1].nodes[0].pos.x, 0.9,
            "in-progress stroke replaced the tail"
        );
    }

    #[test]
    fn brush_stroke_count_reports_len_or_zero() {
        let s = create_mask(&OpStack::default(), "m".into());
        let s = add_component(
            &s,
            0,
            MaskComponent::Brush {
                strokes: vec![stroke(0.1, false)],
            },
            CompositeMode::Add,
        );
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

    #[test]
    fn paint_into_a_fresh_new_brush_layer_appends_the_first_stroke() {
        // The headline flow: "New Brush Layer" → paint. The first dragged frame
        // sees an empty brush (base_count 0) and set_brush_with_base(base=0) must
        // yield exactly one stroke (no loss, no panic on the empty component).
        let s = create_mask(&OpStack::default(), "m".into());
        let s = new_brush_layer(&s, 0);
        let ci = last_brush_index(&s, 0).expect("the new brush layer");
        assert_eq!(brush_stroke_count(&s, 0, ci), 0, "starts empty");
        let s = set_brush_with_base(&s, 0, ci, 0, stroke(0.5, false));
        let ss = brush_strokes(&s, 0, ci);
        assert_eq!(ss.len(), 1, "first stroke appended onto the empty layer");
        assert_eq!(ss[0].nodes[0].pos.x, 0.5);
    }
}
