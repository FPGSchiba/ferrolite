//! Scoped editing (design 2026-07-28 §2): resolves whether the currently
//! active adjustment controls read/write the document's global
//! `AdjustmentSet` or a selected mask layer's, and provides the single write
//! path (`ScopedEdit::write`) that keeps reads and writes consistent with
//! that resolution within a frame. Consumed by `adjustments::scoped_slider`
//! and (Tasks 3-6) the three base tabs.

use crate::develop::adjustment_panel::EditOutcome;
use crate::state::AppState;

/// Which `AdjustmentSet` the currently active controls read/write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditScope {
    Global,
    Mask(usize),
    /// Mask tool active but no mask selected/existing — controls render disabled.
    MaskNone,
}

/// Resolve the current edit scope from the active tool and mask selection.
///
/// The Mask tool with a `mask.selected` index that is stale (out of range for
/// the current layer count — e.g. racing a delete) resolves to `MaskNone`,
/// same as no selection at all.
pub fn current(state: &AppState) -> EditScope {
    if state.tool_state.active != crate::develop::tool::ToolId::Mask {
        return EditScope::Global;
    }
    let Some(viewer) = state.viewer.as_ref() else {
        return EditScope::MaskNone;
    };
    let layer_count = crate::develop::mask_edit::layers(&viewer.op_stack)
        .layers
        .len();
    match viewer.mask.selected {
        Some(idx) if idx < layer_count => EditScope::Mask(idx),
        _ => EditScope::MaskNone,
    }
}

/// A borrowed doc plus its resolved scope: the scoped read (`set`) and write
/// (`write`) path shared by every registry-driven slider, plus the per-frame
/// "adjusting" flag `tool_panel` folds into `mask.adjusting` (mask-overlay
/// suppression while a scoped control is being dragged).
pub struct ScopedEdit<'a> {
    pub scope: EditScope,
    pub doc: &'a ferrolite_pipeline::OpStack,
    /// Set true by any slider being dragged this frame (tool_panel folds it
    /// into `mask.adjusting` when the scope is a mask — overlay suppression).
    pub adjusting: std::cell::Cell<bool>,
}

impl<'a> ScopedEdit<'a> {
    pub fn new(scope: EditScope, doc: &'a ferrolite_pipeline::OpStack) -> Self {
        Self {
            scope,
            doc,
            adjusting: std::cell::Cell::new(false),
        }
    }

    /// The scope's adjustment set. `None` for `MaskNone` or a stale `Mask` index.
    pub fn set(&self) -> Option<&ferrolite_pipeline::AdjustmentSet> {
        match self.scope {
            EditScope::Global => Some(&self.doc.global),
            EditScope::Mask(idx) => self.doc.layers.get(idx).map(|l| &l.adjustments),
            EditScope::MaskNone => None,
        }
    }

    /// Write a full set back to the scope. Global keeps `kind`; Mask forces
    /// `OpKind::LocalAdjustments`. `None` for `MaskNone`/stale index.
    pub fn write(
        &self,
        new: ferrolite_pipeline::AdjustmentSet,
        kind: ferrolite_pipeline::OpKind,
        commit: bool,
    ) -> Option<EditOutcome> {
        match self.scope {
            EditScope::Global => Some(EditOutcome {
                stack: self.doc.with_global(new),
                kind,
                commit,
            }),
            EditScope::Mask(idx) => {
                if idx >= self.doc.layers.len() {
                    return None;
                }
                Some(EditOutcome {
                    stack: self.doc.with_layer_adjustments(idx, new),
                    kind: ferrolite_pipeline::OpKind::LocalAdjustments,
                    commit,
                })
            }
            EditScope::MaskNone => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_resolution_follows_tool_and_selection() {
        let mut state = AppState::new().unwrap();
        assert_eq!(current(&state), EditScope::Global, "Adjust tool ⇒ Global");
        state.tool_state.active = crate::develop::tool::ToolId::Mask;
        assert_eq!(
            current(&state),
            EditScope::MaskNone,
            "Mask tool, no viewer ⇒ MaskNone"
        );
    }

    #[test]
    fn scoped_write_targets_the_right_set() {
        use ferrolite_pipeline::{Op, OpKind, OpStack};
        let doc =
            OpStack::default().set_op(Op::LocalAdjustments(ferrolite_pipeline::LocalAdjustments {
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
        assert_eq!(
            out.kind,
            OpKind::LocalAdjustments,
            "mask writes coerce kind"
        );
        // MaskNone writes nothing.
        let s = ScopedEdit::new(EditScope::MaskNone, &doc);
        assert!(s.set().is_none());
    }

    #[test]
    fn stale_mask_index_reads_and_writes_none() {
        let doc = ferrolite_pipeline::OpStack::default(); // zero layers
        let s = ScopedEdit::new(EditScope::Mask(2), &doc);
        assert!(s.set().is_none());
        assert!(s
            .write(
                Default::default(),
                ferrolite_pipeline::OpKind::Exposure,
                true
            )
            .is_none());
    }
}
