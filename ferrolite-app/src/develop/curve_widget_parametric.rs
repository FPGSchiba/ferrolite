//! Parametric region sub-panel for the Curve tab: Highlights/Lights/Darks/
//! Shadows region sliders + three split sliders (each with the `EguiSlider`
//! per-control reset), plus a small read-only plot of the baked parametric
//! shape. Edits route through `ops_edit::set_tone_curve` (identity-eliding).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::ops_edit::set_tone_curve;
use crate::theme;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{parametric_curve_lut, OpKind, OpStack, ParametricCurve, ToneCurve};

const OVERLAY_W: f32 = 200.0;
const OVERLAY_H: f32 = 60.0;

/// True when any region value OR split point differs (drives the emit gate).
pub(crate) fn param_changed(a: &ParametricCurve, b: &ParametricCurve) -> bool {
    a != b
}

pub fn show(ui: &mut egui::Ui, stack: &OpStack, tc: &ToneCurve) -> Option<EditOutcome> {
    let mut p = tc.parametric;
    let before = p;

    ui.separator();
    ui.label(egui::RichText::new("Parametric").color(theme::TEXT_FAINT));

    // Read-only preview of the baked parametric shape.
    draw_overlay(ui, &p);

    let mut dragged = false;
    let mut drag_stopped = false;
    // ONE closure (not one per slider group) so `dragged`/`drag_stopped` are
    // borrowed mutably by a single closure — two closures each capturing them
    // would fail the borrow checker. The `EguiSlider` (owning its `&mut f32`) is
    // built at the call site and moved in.
    let mut add = |ui: &mut egui::Ui, s: EguiSlider| {
        let r = ui.add(s);
        if r.changed() {
            if r.drag_stopped() {
                drag_stopped = true;
            } else if r.dragged() {
                dragged = true;
            } else {
                drag_stopped = true; // click / typed / double-click-reset commits now
            }
        }
    };
    // Region sliders, light→dark (design §3.3 order). `EguiSlider` is built
    // inline: a helper returning one would have to borrow its `&mut f32` arg,
    // which closure lifetime inference can't express.
    add(
        ui,
        EguiSlider {
            label: "Highlights",
            value: &mut p.highlights,
            min: -1.0,
            max: 1.0,
            default: 0.0,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: true,
            signed: true,
        },
    );
    add(
        ui,
        EguiSlider {
            label: "Lights",
            value: &mut p.lights,
            min: -1.0,
            max: 1.0,
            default: 0.0,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: true,
            signed: true,
        },
    );
    add(
        ui,
        EguiSlider {
            label: "Darks",
            value: &mut p.darks,
            min: -1.0,
            max: 1.0,
            default: 0.0,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: true,
            signed: true,
        },
    );
    add(
        ui,
        EguiSlider {
            label: "Shadows",
            value: &mut p.shadows,
            min: -1.0,
            max: 1.0,
            default: 0.0,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: true,
            signed: true,
        },
    );
    // Split sliders (defaults 0.25 / 0.50 / 0.75).
    add(
        ui,
        EguiSlider {
            label: "Shadow split",
            value: &mut p.shadow_split,
            min: 0.0,
            max: 1.0,
            default: 0.25,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: false,
            signed: false,
        },
    );
    add(
        ui,
        EguiSlider {
            label: "Midtone split",
            value: &mut p.midtone_split,
            min: 0.0,
            max: 1.0,
            default: 0.50,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: false,
            signed: false,
        },
    );
    add(
        ui,
        EguiSlider {
            label: "Highlight split",
            value: &mut p.highlight_split,
            min: 0.0,
            max: 1.0,
            default: 0.75,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: false,
            signed: false,
        },
    );

    if !param_changed(&before, &p) {
        return None;
    }
    let new_tc = ToneCurve {
        parametric: p,
        ..tc.clone()
    };
    Some(EditOutcome {
        stack: set_tone_curve(stack, new_tc),
        kind: OpKind::ToneCurve,
        commit: drag_stopped || !dragged,
    })
}

/// Draw a small read-only plot of the baked parametric LUT (diagonal reference +
/// the parametric shape), so the region/split effect is visible at a glance.
fn draw_overlay(ui: &mut egui::Ui, p: &ParametricCurve) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(OVERLAY_W, OVERLAY_H), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::BG_BASE);
    // Identity reference diagonal.
    painter.line_segment(
        [
            egui::pos2(rect.left(), rect.bottom()),
            egui::pos2(rect.right(), rect.top()),
        ],
        egui::Stroke::new(1.0_f32, theme::BORDER_STRONG),
    );
    // Baked parametric curve.
    let lut = parametric_curve_lut(p);
    let poly: Vec<egui::Pos2> = lut
        .iter()
        .enumerate()
        .map(|(i, &y)| {
            egui::pos2(
                rect.left() + (i as f32 / 255.0) * OVERLAY_W,
                rect.bottom() - y * OVERLAY_H,
            )
        })
        .collect();
    painter.add(egui::Shape::line(
        poly,
        egui::Stroke::new(1.5_f32, theme::ACCENT),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::ParametricCurve;

    #[test]
    fn param_changed_detects_a_region_edit() {
        let a = ParametricCurve::default();
        let b = ParametricCurve {
            shadows: 0.3,
            ..Default::default()
        };
        assert!(param_changed(&a, &b));
        assert!(!param_changed(&a, &ParametricCurve::default()));
    }

    #[test]
    fn param_changed_detects_a_split_edit() {
        let a = ParametricCurve::default();
        let b = ParametricCurve {
            midtone_split: 0.6,
            ..Default::default()
        };
        assert!(param_changed(&a, &b));
    }
}
