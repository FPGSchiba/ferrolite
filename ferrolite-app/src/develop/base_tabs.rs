//! The always-present global adjustment base tabs (design §7/§8).
//! Consolidated into LightTab, ColorTab, and EffectsTab.
//! `base_tabs()` is registered once as the registry's base.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::tool::{PanelTab, TabId};
use crate::develop::{
    curve_widget, grade_widget, hsl_widget, lens_caps_ui, lens_picker, ops_edit, vignette_mode,
};
use crate::state::AppState;
use crate::theme;
use crate::widgets::section_header;
use crate::widgets::slider::EguiSlider;
use ferrolite_lens::LensDb;
use ferrolite_pipeline::{Correction, LensCorrection, OpKind};

/// Fallback focal length seeded for a brand-new `LensCorrection` op ONLY when
/// EXIF has no focal length (`ViewerState::meta` is `None`, still loading, or
/// the decoded `Metadata.focal_length` is itself absent).
const DEFAULT_FOCAL_LEN: f32 = 50.0;
/// Fallback aperture seeded for a brand-new op; mirrors `query_from_metadata`'s
/// own f/8 fallback for the same "EXIF absent" case.
const DEFAULT_APERTURE: f32 = 8.0;
/// Fallback crop factor seeded for a brand-new op when no auto-match candidate
/// is available yet (unmatched EXIF, DB unavailable, or the match hasn't
/// resolved this frame) — 1.0 (full-frame).
const DEFAULT_CROP_FACTOR: f32 = 1.0;

pub struct LightTab;
impl PanelTab for LightTab {
    fn id(&self) -> TabId {
        TabId("light")
    }
    fn label(&self) -> &str {
        "Light"
    }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        let stack = state.viewer.as_ref()?.op_stack.clone();
        let mut out: Option<EditOutcome> = None;

        let mut basic_open = true;
        section_header(ui, "BASIC SLIDERS", &mut basic_open);
        if basic_open {
            // Exposure (bipolar EV).
            let mut ev = stack.exposure().map(|e| e.ev).unwrap_or(0.0);
            let r_ev = ui.add(EguiSlider {
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
                custom_label_w: None,
            });
            if r_ev.changed() {
                out = Some(EditOutcome {
                    stack: ops_edit::set_exposure(&stack, ev),
                    kind: OpKind::Exposure,
                    commit: r_ev.drag_stopped() || !r_ev.dragged(),
                });
            }
            // Contrast (bipolar).
            let mut c = stack.contrast().map(|c| c.amount).unwrap_or(0.0);
            let r_c = ui.add(EguiSlider {
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
                custom_label_w: None,
            });
            if r_c.changed() {
                out = Some(EditOutcome {
                    stack: ops_edit::set_contrast(&stack, c),
                    kind: OpKind::Contrast,
                    commit: r_c.drag_stopped() || !r_c.dragged(),
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
                custom_label_w: None,
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
                custom_label_w: None,
            });
            if rt.changed() || rn.changed() {
                out = Some(EditOutcome {
                    stack: ops_edit::set_white_balance(&stack, temp, tint),
                    kind: OpKind::WhiteBalance,
                    commit: (rt.drag_stopped() || rn.drag_stopped())
                        || !(rt.dragged() || rn.dragged()),
                });
            }

            // Highlights, Shadows, Whites, Blacks
            let mut tc = stack.tone_curve().unwrap_or_default();
            let mut p = tc.parametric;

            let rh = ui.add(EguiSlider {
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
                custom_label_w: None,
            });
            let rs = ui.add(EguiSlider {
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
                custom_label_w: None,
            });
            let rw = ui.add(EguiSlider {
                label: "Whites",
                value: &mut p.lights,
                min: -1.0,
                max: 1.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: true,
                signed: true,
                custom_label_w: None,
            });
            let rb = ui.add(EguiSlider {
                label: "Blacks",
                value: &mut p.darks,
                min: -1.0,
                max: 1.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: true,
                signed: true,
                custom_label_w: None,
            });
            if rh.changed() || rs.changed() || rw.changed() || rb.changed() {
                tc.parametric = p;
                let commit = (rh.drag_stopped()
                    || rs.drag_stopped()
                    || rw.drag_stopped()
                    || rb.drag_stopped())
                    || !(rh.dragged() || rs.dragged() || rw.dragged() || rb.dragged());
                out = Some(EditOutcome {
                    stack: ops_edit::set_tone_curve(&stack, tc),
                    kind: OpKind::ToneCurve,
                    commit,
                });
            }
        }

        ui.separator();
        let mut tone_curve_open = state.settings.tone_curve_open;
        if section_header(ui, "TONE CURVE", &mut tone_curve_open).changed() {
            state.settings.tone_curve_open = tone_curve_open;
        }
        if tone_curve_open {
            if let Some(curve_out) = curve_widget::show(ui, &stack) {
                out = Some(curve_out);
            }
        }

        out
    }
}

pub struct ColorTab;
impl PanelTab for ColorTab {
    fn id(&self) -> TabId {
        TabId("color")
    }
    fn label(&self) -> &str {
        "Color"
    }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        let stack = state.viewer.as_ref()?.op_stack.clone();
        let mut out: Option<EditOutcome> = None;

        let mut hsl_open = true;
        section_header(ui, "COLOR (HSL)", &mut hsl_open);
        if hsl_open {
            if let Some(v) = state.viewer.as_mut() {
                if let Some(o) = hsl_widget::show(ui, &stack, &mut v.hsl_band) {
                    out = Some(o);
                }
            }
        }

        ui.separator();

        let mut color_grading_open = state.settings.color_grading_open;
        if section_header(ui, "COLOR GRADING", &mut color_grading_open).changed() {
            state.settings.color_grading_open = color_grading_open;
        }
        if color_grading_open {
            if let Some(grade_out) = grade_widget::show(ui, &stack) {
                out = Some(grade_out);
            }
        }

        out
    }
}

pub struct EffectsTab;
impl PanelTab for EffectsTab {
    fn id(&self) -> TabId {
        TabId("effects")
    }
    fn label(&self) -> &str {
        "Effects"
    }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        let stack = state.viewer.as_ref()?.op_stack.clone();
        let mut out: Option<EditOutcome> = None;

        // Sharpening (Amount, Radius, Detail)
        let mut sharpening_open = true;
        section_header(ui, "SHARPENING", &mut sharpening_open);
        if sharpening_open {
            let sh = stack.sharpen();
            let (mut amount, mut radius) = sh
                .map(|s| (s.amount, s.radius as f32))
                .unwrap_or((0.0, 1.0));
            let mut detail = 0.0_f32;

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
                custom_label_w: None,
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
                custom_label_w: None,
            });
            let rd = ui.add(EguiSlider {
                label: "Detail",
                value: &mut detail,
                min: 0.0,
                max: 1.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: false,
                signed: false,
                custom_label_w: None,
            });
            if ra.changed() || rr.changed() || rd.changed() {
                out = Some(EditOutcome {
                    stack: ops_edit::set_sharpen(&stack, amount, radius.round() as u32),
                    kind: OpKind::Sharpen,
                    commit: (ra.drag_stopped() || rr.drag_stopped() || rd.drag_stopped())
                        || !(ra.dragged() || rr.dragged() || rd.dragged()),
                });
            }
        }

        // Noise Reduction (Luminance, Detail, Color, Color Detail)
        ui.separator();
        let mut nr_open = true;
        section_header(ui, "NOISE REDUCTION", &mut nr_open);
        if nr_open {
            let mut nr_lum = 0.0_f32;
            let mut nr_lum_detail = 0.0_f32;
            let mut nr_color = 0.0_f32;
            let mut nr_color_detail = 0.0_f32;

            ui.add(EguiSlider {
                label: "Luminance",
                value: &mut nr_lum,
                min: 0.0,
                max: 1.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: false,
                signed: false,
                custom_label_w: None,
            });
            ui.add(EguiSlider {
                label: "Detail",
                value: &mut nr_lum_detail,
                min: 0.0,
                max: 1.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: false,
                signed: false,
                custom_label_w: None,
            });
            ui.add(EguiSlider {
                label: "Color",
                value: &mut nr_color,
                min: 0.0,
                max: 1.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: false,
                signed: false,
                custom_label_w: None,
            });
            ui.add(EguiSlider {
                label: "Color Detail",
                value: &mut nr_color_detail,
                min: 0.0,
                max: 1.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: false,
                signed: false,
                custom_label_w: None,
            });
        }

        // Optics (lens picker, Distortion, Vignette, etc.)
        ui.separator();
        let mut optics_open = state.settings.optics_open;
        if section_header(ui, "OPTICS", &mut optics_open).changed() {
            state.settings.optics_open = optics_open;
        }
        if optics_open {
            if let Some(optics_out) = show_optics_section(ui, state, &stack) {
                out = Some(optics_out);
            }
        }

        out
    }
}

fn show_optics_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    stack: &ferrolite_pipeline::OpStack,
) -> Option<EditOutcome> {
    let mut out: Option<EditOutcome> = None;

    let Some(db) = state.lens_db.clone() else {
        ui.weak("Lens database unavailable — corrections disabled.");
        return out;
    };
    let seed_camera_hint = state
        .viewer
        .as_ref()
        .and_then(|v| v.meta.as_ref())
        .map(|m| format!("{} {}", m.make, m.model));
    let seed_focal = state
        .viewer
        .as_ref()
        .and_then(|v| v.meta.as_ref())
        .and_then(|m| m.focal_length);
    let seed_aperture = state
        .viewer
        .as_ref()
        .and_then(|v| v.meta.as_ref())
        .and_then(|m| m.aperture);
    let seed_crop_factor = state
        .viewer
        .as_ref()
        .and_then(|v| v.lens_auto_match.as_ref())
        .map(|m| m.crop_factor);
    let auto_match_name = state
        .viewer
        .as_ref()
        .and_then(|v| v.lens_auto_match.as_ref())
        .map(|m| m.display_name.clone());
    let lc = stack.lens_correction().unwrap_or_else(|| LensCorrection {
        lens_id: None,
        focal_len: seed_focal.unwrap_or(DEFAULT_FOCAL_LEN),
        aperture: seed_aperture.unwrap_or(DEFAULT_APERTURE),
        crop_factor: seed_crop_factor.unwrap_or(DEFAULT_CROP_FACTOR),
        distortion: Correction::default(),
        tca: Correction::default(),
        vignetting: Correction::default(),
    });
    let mut new_lc = lc.clone();
    let mut changed = false;
    let mut amount_dragged = false;
    let mut amount_drag_stopped = false;

    ui.horizontal(|ui| {
        let resolved = state
            .viewer
            .as_ref()
            .and_then(|v| v.lens_resolved_name.clone());
        let label = match (&lc.lens_id, &resolved) {
            (Some(_), Some(name)) => name.clone(),
            (Some(id), None) => format!("{id} (unresolved)"),
            (None, _) => auto_match_name
                .clone()
                .map(|name| format!("{name} (suggested)"))
                .unwrap_or_else(|| "No lens matched".to_string()),
        };
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let has_lens = lc.lens_id.is_some();
            if has_lens && ui.small_button("Clear").clicked() {
                new_lc.lens_id = None;
                if let Some(v) = state.viewer.as_mut() {
                    v.lens_resolved_name = None;
                }
                changed = true;
            }
            if ui.small_button("Choose lens\u{2026}").clicked() {
                if let Some(v) = state.viewer.as_mut() {
                    v.lens_picker_open = true;
                    v.lens_picker_query.clear();
                }
            }
            ui.add(egui::Label::new(label).truncate());
        });
    });

    let picker_open = state
        .viewer
        .as_ref()
        .map(|v| v.lens_picker_open)
        .unwrap_or(false);
    if picker_open {
        let mut query = state
            .viewer
            .as_ref()
            .map(|v| v.lens_picker_query.clone())
            .unwrap_or_default();
        if let Some(outcome) = lens_picker::show(
            ui.ctx(),
            db.as_ref(),
            seed_camera_hint.as_deref().unwrap_or(""),
            &mut query,
        ) {
            if let Some(v) = state.viewer.as_mut() {
                v.lens_picker_open = false;
            }
            if let lens_picker::PickerOutcome::Picked(m) = outcome {
                if let Some((min, max)) = db.lens_focal_range(&m.lens_id) {
                    new_lc.focal_len = new_lc.focal_len.clamp(min, max);
                }
                new_lc.lens_id = Some(m.lens_id);
                new_lc.crop_factor = m.crop_factor;
                if let Some(v) = state.viewer.as_mut() {
                    v.lens_resolved_name = Some(m.display_name);
                }
                changed = true;
            }
        }
        if let Some(v) = state.viewer.as_mut() {
            v.lens_picker_query = query;
        }
    }

    const TITLE_TO_CONTROL_GAP: f32 = 2.0;
    const BETWEEN_GROUP_GAP: f32 = 10.0;
    let has_lens = lc.lens_id.is_some()
        || state
            .viewer
            .as_ref()
            .and_then(|v| v.lens_resolved_name.as_ref())
            .is_some();
    let has_vignette_lut = state
        .viewer
        .as_ref()
        .map(|v| v.lens_vignette.is_some())
        .unwrap_or(false);

    let caps = new_lc
        .lens_id
        .as_deref()
        .and_then(|id| db.lens_caps(id, new_lc.focal_len, new_lc.aperture));

    let correction_row = |ui: &mut egui::Ui,
                          name: &str,
                          c: &mut Correction,
                          params: vignette_mode::VigSliderParams,
                          row_enabled: bool,
                          hover_text: Option<&str>,
                          dragged: &mut bool,
                          drag_stopped: &mut bool,
                          toggled: &mut bool| {
        let prev_spacing_y = ui.spacing().item_spacing.y;
        ui.spacing_mut().item_spacing.y = 0.0;

        let title = lens_caps_ui::correction_title(name, row_enabled, hover_text);
        let title_text = if row_enabled {
            egui::RichText::new(title)
        } else {
            egui::RichText::new(title).color(theme::TEXT_DIM)
        };
        ui.label(title_text);
        ui.add_space(TITLE_TO_CONTROL_GAP);

        ui.horizontal(|ui| {
            let cb = ui.add_enabled(row_enabled, egui::Checkbox::new(&mut c.enabled, ""));
            let cb = match hover_text {
                Some(text) if !row_enabled => cb.on_disabled_hover_text(text),
                _ => cb,
            };
            if cb.changed() {
                *toggled = true;
            }
            ui.add_enabled_ui(row_enabled && c.enabled, |ui| {
                let r = ui.add(EguiSlider {
                    label: "",
                    value: &mut c.amount,
                    min: params.min,
                    max: params.max,
                    default: params.default,
                    step: 0.01,
                    decimals: 2,
                    unit: "",
                    bipolar: params.bipolar,
                    signed: params.bipolar,
                    custom_label_w: None,
                });
                if r.changed() {
                    if r.drag_stopped() {
                        *drag_stopped = true;
                    } else if r.dragged() {
                        *dragged = true;
                    } else {
                        *drag_stopped = true;
                    }
                }
            });
        });

        ui.spacing_mut().item_spacing.y = prev_spacing_y;
        ui.add_space(BETWEEN_GROUP_GAP);
    };

    let mut adjust_dragged = false;
    let mut adjust_drag_stopped = false;

    let distortion_gate = lens_caps_ui::correction_row_gate(
        has_lens,
        caps,
        lens_caps_ui::GatedCorrection::Distortion,
    );
    let mut distortion_toggled = false;
    correction_row(
        ui,
        "Distortion",
        &mut new_lc.distortion,
        vignette_mode::PROFILE_PARAMS,
        distortion_gate.enabled,
        distortion_gate.hover_text,
        &mut amount_dragged,
        &mut amount_drag_stopped,
        &mut distortion_toggled,
    );

    if has_lens {
        let focal_range = new_lc
            .lens_id
            .as_deref()
            .and_then(|id| db.lens_focal_range(id));
        let (focal_min, focal_max) = focal_range.unwrap_or((8.0, 800.0));
        egui::CollapsingHeader::new(format!("Adjust \u{b7} Focal {:.0} mm", new_lc.focal_len))
            .id_salt("lens_corrections_adjust_focal")
            .show(ui, |ui| {
                let rf = ui
                    .add(EguiSlider {
                        label: "Focal",
                        value: &mut new_lc.focal_len,
                        min: focal_min,
                        max: focal_max,
                        default: DEFAULT_FOCAL_LEN,
                        step: 1.0,
                        decimals: 0,
                        unit: " mm",
                        bipolar: false,
                        signed: false,
                        custom_label_w: None,
                    })
                    .on_hover_text("Affects Distortion and Transverse CA");
                if rf.changed() {
                    if rf.drag_stopped() {
                        adjust_drag_stopped = true;
                    } else if rf.dragged() {
                        adjust_dragged = true;
                    } else {
                        adjust_drag_stopped = true;
                    }
                }
            });
    }
    let tca_gate =
        lens_caps_ui::correction_row_gate(has_lens, caps, lens_caps_ui::GatedCorrection::Tca);
    let mut tca_toggled = false;
    correction_row(
        ui,
        "Transverse CA",
        &mut new_lc.tca,
        vignette_mode::PROFILE_PARAMS,
        tca_gate.enabled,
        tca_gate.hover_text,
        &mut amount_dragged,
        &mut amount_drag_stopped,
        &mut tca_toggled,
    );
    let vignette_was_enabled = new_lc.vignetting.enabled;
    let mut vignetting_toggled = false;
    correction_row(
        ui,
        lens_caps_ui::vignette_row_label(caps),
        &mut new_lc.vignetting,
        vignette_mode::slider_params(has_vignette_lut),
        true,
        None,
        &mut amount_dragged,
        &mut amount_drag_stopped,
        &mut vignetting_toggled,
    );
    if vignetting_toggled
        && new_lc.vignetting.enabled
        && !vignette_was_enabled
        && !has_vignette_lut
        && (new_lc.vignetting.amount - 1.0).abs() < f32::EPSILON
    {
        new_lc.vignetting.amount = vignette_mode::MANUAL_PARAMS.default;
    }
    let has_vignette_profile = caps.map(|c| c.vignetting).unwrap_or(false);
    if has_vignette_profile {
        egui::CollapsingHeader::new(format!("Adjust \u{b7} Aperture f/{:.1}", new_lc.aperture))
            .id_salt("lens_corrections_adjust_aperture")
            .show(ui, |ui| {
                let ra = ui
                    .add(EguiSlider {
                        label: "Aperture",
                        value: &mut new_lc.aperture,
                        min: 1.0,
                        max: 32.0,
                        default: DEFAULT_APERTURE,
                        step: 0.1,
                        decimals: 1,
                        unit: " f",
                        bipolar: false,
                        signed: false,
                        custom_label_w: None,
                    })
                    .on_hover_text("Affects profile Vignette strength only");
                if ra.changed() {
                    if ra.drag_stopped() {
                        adjust_drag_stopped = true;
                    } else if ra.dragged() {
                        adjust_dragged = true;
                    } else {
                        adjust_drag_stopped = true;
                    }
                }
            });
    }
    if distortion_toggled || tca_toggled || vignetting_toggled {
        changed = true;
    }

    let adjust_changed = adjust_dragged || adjust_drag_stopped;
    if changed || adjust_changed || amount_dragged || amount_drag_stopped {
        let s = ops_edit::set_lens_correction(stack, new_lc);
        let any_dragging = adjust_dragged || amount_dragged;
        let any_drag_stopped = adjust_drag_stopped || amount_drag_stopped;
        let commit = changed || any_drag_stopped || !any_dragging;
        out = Some(EditOutcome {
            stack: s,
            kind: OpKind::LensCorrection,
            commit,
        });
    }

    out
}

pub fn base_tabs() -> Vec<Box<dyn PanelTab>> {
    vec![Box::new(LightTab), Box::new(ColorTab), Box::new(EffectsTab)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_tabs_returns_three_tabs_with_correct_ids_and_labels() {
        let tabs = base_tabs();
        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[0].id(), TabId("light"));
        assert_eq!(tabs[0].label(), "Light");
        assert_eq!(tabs[1].id(), TabId("color"));
        assert_eq!(tabs[1].label(), "Color");
        assert_eq!(tabs[2].id(), TabId("effects"));
        assert_eq!(tabs[2].label(), "Effects");
    }

    #[test]
    fn test_light_tab_collapsible_sections() {
        let ctx = egui::Context::default();
        let mut state = AppState::new().unwrap();
        state.settings.tone_curve_open = false;

        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let tab = LightTab;
                let res = tab.show(ui, &mut state);
                assert!(res.is_none());
            });
        });
        assert!(!state.settings.tone_curve_open);
    }

    #[test]
    fn test_color_tab_collapsible_sections() {
        let ctx = egui::Context::default();
        let mut state = AppState::new().unwrap();
        state.settings.color_grading_open = false;

        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let tab = ColorTab;
                let res = tab.show(ui, &mut state);
                assert!(res.is_none());
            });
        });
        assert!(!state.settings.color_grading_open);
    }

    #[test]
    fn test_effects_tab_collapsible_sections() {
        let ctx = egui::Context::default();
        let mut state = AppState::new().unwrap();
        state.settings.optics_open = false;

        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let tab = EffectsTab;
                let res = tab.show(ui, &mut state);
                assert!(res.is_none());
            });
        });
        assert!(!state.settings.optics_open);
    }
}
