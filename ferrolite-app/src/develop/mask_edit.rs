//! Pure helpers mapping a Masking-UI action to a new immutable `OpStack`. A
//! `LocalAdjustments` with zero layers REMOVES the op (reset) so
//! `is_identity()`/`has_edits` stay correct — mirroring `ops_edit`. All edits
//! carry `OpKind::LocalAdjustments`; the app pushes one history entry per gesture.
//!
//! NOTE: consumed by the mask panel tasks later in Plan 4; the module-level allow
//! is REMOVED at the Plan-4 gate (Task 13).
#![allow(dead_code)]

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
}
