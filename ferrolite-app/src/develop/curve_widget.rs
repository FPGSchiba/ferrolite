//! Thin tone-curve adapter over the reusable `widgets::curve::curve_editor`.
//! Reads the current `ToneCurve` op (or identity + Smooth default for a
//! not-yet-created curve) and maps the widget's `CurveEdit` back onto an
//! `OpStack` edit. All interaction/paint logic lives in `widgets::curve`.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::curve_math;
use crate::theme;
use crate::widgets::curve::{curve_editor, CurveStyle};
use ferrolite_pipeline::{CurveMode, Op, OpKind, OpStack, ToneCurve};

pub fn show(ui: &mut egui::Ui, stack: &OpStack) -> Option<EditOutcome> {
    let tc = stack.tone_curve();
    let points = tc
        .as_ref()
        .map(|t| t.points.clone())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(curve_math::identity_points);
    // No curve op yet → a user's first edit should be Smooth (the new-curve
    // default), not Linear (which only exists for pre-feature sidecars).
    let mode = tc.as_ref().map(|t| t.mode).unwrap_or(CurveMode::Smooth);

    let edit = curve_editor(
        ui,
        "tone_curve",
        &points,
        mode,
        &CurveStyle {
            curve_color: theme::ACCENT,
            point_color: theme::ACCENT_BRIGHT,
        },
    )?;

    if edit.reset {
        return Some(EditOutcome {
            stack: stack.reset(OpKind::ToneCurve),
            kind: OpKind::ToneCurve,
            commit: true,
        });
    }

    if curve_math::is_identity(&edit.points) {
        return Some(EditOutcome {
            stack: stack.reset(OpKind::ToneCurve),
            kind: OpKind::ToneCurve,
            commit: edit.commit,
        });
    }

    Some(EditOutcome {
        stack: stack.set_op(Op::ToneCurve(ToneCurve {
            points: edit.points,
            mode: edit.mode,
            ..Default::default()
        })),
        kind: OpKind::ToneCurve,
        commit: edit.commit,
    })
}
