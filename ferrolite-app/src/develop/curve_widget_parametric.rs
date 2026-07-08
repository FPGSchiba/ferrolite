//! Parametric region sub-panel for the Curve tab (Task 7 fills this in).
use crate::develop::adjustment_panel::EditOutcome;
use ferrolite_pipeline::{OpStack, ToneCurve};

/// Draw the parametric region controls. Task 6 stub: renders nothing, emits none.
pub fn show(_ui: &mut egui::Ui, _stack: &OpStack, _tc: &ToneCurve) -> Option<EditOutcome> {
    None
}
