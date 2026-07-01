//! Develop right adjustment panel (design-system §6, 296px). CollapsingHeader
//! sections; one EguiSlider per op param; per-section + global reset. Emits a new
//! OpStack via develop::ops_edit; the app applies it to both render tiers.

use crate::develop::{curve_widget, hsl_widget, ops_edit};
use crate::state::AppState;
use crate::theme;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{Aspect, Geometry, Op, OpKind, OpStack};

pub struct EditOutcome {
    pub stack: OpStack,
    pub kind: OpKind,
    pub commit: bool,
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
    let stack = match state.viewer.as_ref() {
        Some(v) => v.op_stack.clone(),
        None => return None,
    };
    let mut out: Option<EditOutcome> = None;

    // ── Save-state indicator ──
    // Edits auto-save: each commit calls persist_ops → spawn_ops_write off-thread.
    // This compact line surfaces the current save state so the author can confirm
    // that edits are being persisted (there is no manual Ctrl+S).
    {
        let image_id = state.viewer.as_ref().map(|v| v.image_id);
        let has_edits = image_id
            .and_then(|id| state.images.iter().find(|r| r.id == id))
            .map(|r| r.has_edits)
            .unwrap_or(false);

        let (label, color) = if state.ops_save_inflight > 0 {
            ("Saving\u{2026}", theme::TEXT_DIM)
        } else if state.ops_save_failed {
            ("Save failed", theme::SEMANTIC_RED)
        } else if has_edits {
            ("Saved", theme::SEMANTIC_GREEN)
        } else {
            ("No edits", theme::TEXT_FAINT)
        };

        ui.add_space(2.0);
        ui.label(egui::RichText::new(label).color(color).size(11.0));
        ui.add_space(4.0);
    }

    // ── Basic ──
    egui::CollapsingHeader::new("Basic")
        .default_open(true)
        .show(ui, |ui| {
            // Exposure (bipolar EV).
            let mut ev = stack.exposure().map(|e| e.ev).unwrap_or(0.0);
            let r = ui.add(EguiSlider {
                label: "Exposure",
                value: &mut ev,
                min: -5.0,
                max: 5.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: " EV",
                bipolar: true,
                signed: true,
            });
            if r.changed() {
                out = Some(EditOutcome {
                    stack: ops_edit::set_exposure(&stack, ev),
                    kind: OpKind::Exposure,
                    commit: r.drag_stopped() || !r.dragged(),
                });
            }
            // Contrast (bipolar).
            let mut c = stack.contrast().map(|c| c.amount).unwrap_or(0.0);
            let r = ui.add(EguiSlider {
                label: "Contrast",
                value: &mut c,
                min: -1.0,
                max: 1.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: true,
                signed: true,
            });
            if r.changed() {
                out = Some(EditOutcome {
                    stack: ops_edit::set_contrast(&stack, c),
                    kind: OpKind::Contrast,
                    commit: r.drag_stopped() || !r.dragged(),
                });
            }
            // White balance Temp + Tint.
            let wb = stack.white_balance();
            let (mut temp, mut tint) = wb.map(|w| (w.temp, w.tint)).unwrap_or((0.0, 0.0));
            let rt = ui.add(EguiSlider {
                label: "Temp",
                value: &mut temp,
                min: -1.0,
                max: 1.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: true,
                signed: true,
            });
            let rn = ui.add(EguiSlider {
                label: "Tint",
                value: &mut tint,
                min: -1.0,
                max: 1.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: true,
                signed: true,
            });
            if rt.changed() || rn.changed() {
                out = Some(EditOutcome {
                    stack: ops_edit::set_white_balance(&stack, temp, tint),
                    kind: OpKind::WhiteBalance,
                    commit: (rt.drag_stopped() || rn.drag_stopped())
                        || !(rt.dragged() || rn.dragged()),
                });
            }
            if ui.small_button("Reset").clicked() {
                let s = stack
                    .reset(OpKind::Exposure)
                    .reset(OpKind::Contrast)
                    .reset(OpKind::WhiteBalance);
                out = Some(EditOutcome {
                    stack: s,
                    kind: OpKind::Exposure,
                    commit: true,
                });
            }
        });

    // ── Tone Curve ── (interactive widget, Task 11)
    egui::CollapsingHeader::new("Tone Curve").show(ui, |ui| {
        if let Some(o) = curve_widget::show(ui, &stack) {
            out = Some(o);
        }
    });

    // ── HSL ── (swatch row + per-band sliders, Task 12)
    egui::CollapsingHeader::new("HSL").show(ui, |ui| {
        if let Some(v) = state.viewer.as_mut() {
            if let Some(o) = hsl_widget::show(ui, &stack, &mut v.hsl_band) {
                out = Some(o);
            }
        }
    });

    // ── Detail ──
    egui::CollapsingHeader::new("Detail").show(ui, |ui| {
        let sh = stack.sharpen();
        let (mut amount, mut radius) = sh
            .map(|s| (s.amount, s.radius as f32))
            .unwrap_or((0.0, 1.0));
        let ra = ui.add(EguiSlider {
            label: "Amount",
            value: &mut amount,
            min: 0.0,
            max: 2.0,
            default: 0.0,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: false,
            signed: false,
        });
        let rr = ui.add(EguiSlider {
            label: "Radius",
            value: &mut radius,
            min: 1.0,
            max: 8.0,
            default: 1.0,
            step: 1.0,
            decimals: 0,
            unit: " px",
            bipolar: false,
            signed: false,
        });
        if ra.changed() || rr.changed() {
            out = Some(EditOutcome {
                stack: ops_edit::set_sharpen(&stack, amount, radius.round() as u32),
                kind: OpKind::Sharpen,
                commit: (ra.drag_stopped() || rr.drag_stopped()) || !(ra.dragged() || rr.dragged()),
            });
        }
    });

    // ── Geometry ── (angle + aspect; the crop overlay lives on the canvas, Task 13)
    egui::CollapsingHeader::new("Geometry").show(ui, |ui| {
        if let Some(v) = state.viewer.as_mut() {
            v.crop_active = true; // overlay shown while this section is expanded
        }
        let geo = stack.geometry().unwrap_or(Geometry {
            crop: ferrolite_pipeline::CropRect::full(),
            angle_deg: 0.0,
            aspect: Aspect::Original,
        });
        let mut angle = geo.angle_deg;
        let r = ui.add(EguiSlider {
            label: "Angle",
            value: &mut angle,
            min: -45.0,
            max: 45.0,
            default: 0.0,
            step: 0.1,
            decimals: 1,
            unit: "\u{b0}",
            bipolar: true,
            signed: true,
        });
        let mut aspect = geo.aspect;
        egui::ComboBox::from_label("Aspect")
            .selected_text(format!("{aspect:?}"))
            .show_ui(ui, |ui| {
                for a in [
                    Aspect::Original,
                    Aspect::Free,
                    Aspect::Square,
                    Aspect::ThreeTwo,
                    Aspect::FourThree,
                    Aspect::SixteenNine,
                ] {
                    ui.selectable_value(&mut aspect, a, format!("{a:?}"));
                }
            });
        if r.changed() || aspect != geo.aspect {
            let new_geo = Geometry {
                crop: geo.crop,
                angle_deg: angle,
                aspect,
            };
            let s = if new_geo.angle_deg == 0.0
                && new_geo.aspect == Aspect::Original
                && new_geo.crop == ferrolite_pipeline::CropRect::full()
            {
                stack.reset(OpKind::Geometry)
            } else {
                stack.set_op(Op::Geometry(new_geo))
            };
            out = Some(EditOutcome {
                stack: s,
                kind: OpKind::Geometry,
                commit: r.drag_stopped() || !r.dragged() || aspect != geo.aspect,
            });
        }
        if ui.small_button("Reset crop").clicked() {
            out = Some(EditOutcome {
                stack: stack.reset(OpKind::Geometry),
                kind: OpKind::Geometry,
                commit: true,
            });
        }
    });
    // Geometry section collapsed → clear crop_active (overlay hidden) handled by
    // app.rs based on whether this section reported open; simplest: reset to false
    // at the top of the frame and set true inside the open section (above).

    ui.separator();
    if ui.button("Reset all").clicked() {
        out = Some(EditOutcome {
            stack: OpStack::default(),
            kind: OpKind::Exposure,
            commit: true,
        });
    }

    out
}
