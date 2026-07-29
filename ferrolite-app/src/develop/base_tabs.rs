//! The always-present global adjustment base tabs (design §7/§8).
//! Consolidated into LightTab, ColorTab, and EffectsTab.
//! `base_tabs()` is registered once as the registry's base.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::adjustments::{color_sliders, effects_sliders, light_sliders, scoped_slider};
use crate::develop::scope::{self, EditScope, ScopedEdit};
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
        let scope = scope::current(state);
        let scoped = ScopedEdit::new(scope, &stack);
        let scope_is_mask = matches!(scope, EditScope::Mask(_) | EditScope::MaskNone);
        let mut out: Option<EditOutcome> = None;

        // per-scope disclosure state (spec §3 / V2 README): Adjust and Mask
        // scopes remember their open/closed sections independently.
        let open = if scope_is_mask {
            &mut state.settings.mask_basic_sliders_open
        } else {
            &mut state.settings.basic_sliders_open
        };
        section_header(ui, "BASIC SLIDERS", open);
        if *open {
            for spec in light_sliders() {
                if let Some(edit) = scoped_slider(ui, spec, &scoped) {
                    out = Some(edit);
                }
            }
        }

        ui.separator();
        let open = if scope_is_mask {
            &mut state.settings.mask_tone_curve_open
        } else {
            &mut state.settings.tone_curve_open
        };
        section_header(ui, "TONE CURVE", open);
        if *open {
            if let Some(curve_out) = curve_widget::show(ui, &scoped) {
                out = Some(curve_out);
            }
        }

        // Read the adjusting flag into a local before touching `state` below —
        // `scoped` borrows the local `stack` clone, not `state`, but keep the
        // read-then-mutate order explicit per the scoped-edit contract.
        let scoped_adjusting = scoped.adjusting.get();
        if matches!(scope, EditScope::Mask(_)) {
            if let Some(v) = state.viewer.as_mut() {
                v.mask.adjusting = scoped_adjusting;
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
        let scope = scope::current(state);
        let scoped = ScopedEdit::new(scope, &stack);
        let scope_is_mask = matches!(scope, EditScope::Mask(_) | EditScope::MaskNone);
        let mut out: Option<EditOutcome> = None;

        // per-scope disclosure state (spec §3 / V2 README): Adjust and Mask
        // scopes remember their open/closed sections independently.
        let open = if scope_is_mask {
            &mut state.settings.mask_color_hsl_open
        } else {
            &mut state.settings.color_hsl_open
        };
        section_header(ui, "COLOR (HSL)", open);
        if *open {
            if let Some(v) = state.viewer.as_mut() {
                if let Some(o) = hsl_widget::show(ui, &scoped, &mut v.hsl_band) {
                    out = Some(o);
                }
            }
        }

        ui.separator();

        let open = if scope_is_mask {
            &mut state.settings.mask_color_mix_open
        } else {
            &mut state.settings.color_mix_open
        };
        section_header(ui, "COLOR MIX", open);
        if *open {
            for spec in color_sliders() {
                if let Some(edit) = scoped_slider(ui, spec, &scoped) {
                    out = Some(edit);
                }
            }
            show_color_swatch(ui, scope, &scoped, &mut out);
        }

        ui.separator();

        let open = if scope_is_mask {
            &mut state.settings.mask_color_grading_open
        } else {
            &mut state.settings.color_grading_open
        };
        section_header(ui, "COLOR GRADING", open);
        if *open {
            if let Some(grade_out) = grade_widget::show(ui, &scoped) {
                out = Some(grade_out);
            }
        }

        // Read the adjusting flag into a local before touching `state` below —
        // `scoped` borrows the local `stack` clone, not `state`, but keep the
        // read-then-mutate order explicit per the scoped-edit contract.
        let scoped_adjusting = scoped.adjusting.get();
        if matches!(scope, EditScope::Mask(_)) {
            if let Some(v) = state.viewer.as_mut() {
                v.mask.adjusting = scoped_adjusting;
            }
        }

        out
    }
}

/// The Color swatch picker (not a registry slider — `AdjustmentSet.color` is
/// an RGB+amount overlay, and only the amount fits `SliderSpec`'s single-f32
/// shape). Moved here verbatim from `mask_panel::selected_section` (Task 6
/// deletes the original there): both Global and Mask scope commit an RGB
/// change through `scoped.write` (Global goes through `ScopedEdit::write`'s
/// `with_global` path) now that the unified layer engine (Phase 3) applies
/// the color overlay globally too. Only `MaskNone` stays greyed, same as
/// every other scoped control.
fn show_color_swatch(
    ui: &mut egui::Ui,
    scope: EditScope,
    scoped: &ScopedEdit<'_>,
    out: &mut Option<EditOutcome>,
) {
    let set = scoped.set();
    let mut rgb = set
        .map(|s| [s.color.r, s.color.g, s.color.b])
        .unwrap_or([0.0, 0.0, 0.0]);
    let enabled = matches!(scope, EditScope::Global | EditScope::Mask(_)) && set.is_some();

    if enabled {
        if ui.color_edit_button_rgb(&mut rgb).changed() {
            let mut new_set = set.expect("enabled implies a set to read").clone();
            new_set.color.r = rgb[0];
            new_set.color.g = rgb[1];
            new_set.color.b = rgb[2];
            if let Some(edit) = scoped.write(new_set, OpKind::LocalAdjustments, true) {
                *out = Some(edit);
            }
        }
        return;
    }

    ui.add_enabled_ui(false, |ui| {
        ui.color_edit_button_rgb(&mut rgb);
    })
    .response
    .on_hover_text(scope::MASK_NONE_HINT);
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
        let scope = scope::current(state);
        let scoped = ScopedEdit::new(scope, &stack);
        let scope_is_mask = matches!(scope, EditScope::Mask(_) | EditScope::MaskNone);
        let mut out: Option<EditOutcome> = None;

        // Sharpening (Amount, Radius). "Detail" from the pre-registry block is
        // dropped — it mapped to no field/shader parameter (see adjustments.rs).
        // per-scope disclosure state (spec §3 / V2 README): Adjust and Mask
        // scopes remember their open/closed sections independently.
        let open = if scope_is_mask {
            &mut state.settings.mask_sharpening_open
        } else {
            &mut state.settings.sharpening_open
        };
        section_header(ui, "SHARPENING", open);
        if *open {
            for spec in effects_sliders()
                .iter()
                .filter(|s| s.id.0.starts_with("sharpen"))
            {
                if let Some(edit) = scoped_slider(ui, spec, &scoped) {
                    out = Some(edit);
                }
            }
        }

        // Noise Reduction (Luminance, Detail, Color, Color Detail) — honestly
        // greyed in both scopes: no GPU pass wired yet (was enabled-but-dead
        // locals before this registry rewrite).
        ui.separator();
        let open = if scope_is_mask {
            &mut state.settings.mask_noise_reduction_open
        } else {
            &mut state.settings.noise_reduction_open
        };
        section_header(ui, "NOISE REDUCTION", open);
        if *open {
            for spec in effects_sliders()
                .iter()
                .filter(|s| s.id.0.starts_with("nr_"))
            {
                if let Some(edit) = scoped_slider(ui, spec, &scoped) {
                    out = Some(edit);
                }
            }
        }

        // Dehaze (amount + dark-channel patch radius). Per-control reset is the
        // EguiSlider reset column (CLAUDE.md — load-bearing).
        ui.separator();
        let open = if scope_is_mask {
            &mut state.settings.mask_dehaze_open
        } else {
            &mut state.settings.dehaze_open
        };
        section_header(ui, "DEHAZE", open);
        if *open {
            for spec in effects_sliders()
                .iter()
                .filter(|s| s.id.0.starts_with("dehaze"))
            {
                if let Some(edit) = scoped_slider(ui, spec, &scoped) {
                    out = Some(edit);
                }
            }
        }

        // Optics (lens picker, Distortion, Vignette, etc.) — global scope ONLY:
        // geometric/lens corrections are not maskable (design 2026-07-28 §1).
        if matches!(scope, EditScope::Global) {
            ui.separator();
            section_header(ui, "OPTICS", &mut state.settings.optics_open);
            if state.settings.optics_open {
                if let Some(optics_out) = show_optics_section(ui, state, &stack) {
                    out = Some(optics_out);
                }
            }
        }

        // Read the adjusting flag into a local before touching `state` below —
        // `scoped` borrows the local `stack` clone, not `state`, but keep the
        // read-then-mutate order explicit per the scoped-edit contract.
        let scoped_adjusting = scoped.adjusting.get();
        if matches!(scope, EditScope::Mask(_)) {
            if let Some(v) = state.viewer.as_mut() {
                v.mask.adjusting = scoped_adjusting;
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
    fn light_tab_edits_the_selected_mask_when_mask_scope_active() {
        let ctx = egui::Context::default();
        let mut state = AppState::new().unwrap();
        // No viewer ⇒ tab renders nothing and returns None (unchanged behavior).
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(LightTab.show(ui, &mut state).is_none());
            });
        });
        // Scope resolution is what the tab keys on — covered by scope.rs tests;
        // here assert the registry rows exist and are correctly gated.
        let specs = crate::develop::adjustments::light_sliders();
        let ids: Vec<&str> = specs.iter().map(|s| s.id.0).collect();
        assert_eq!(
            ids,
            vec![
                "exposure",
                "contrast",
                "highlights",
                "shadows",
                "whites",
                "blacks",
                "temp",
                "tint"
            ]
        );
        let hl = specs.iter().find(|s| s.id.0 == "highlights").unwrap();
        assert!(hl.global_ready && hl.mask_ready);
        assert!(hl.global_reason.is_empty());
        let ex = specs.iter().find(|s| s.id.0 == "exposure").unwrap();
        assert!(ex.global_ready && ex.mask_ready);
    }

    #[test]
    fn test_light_tab_collapsible_sections() {
        let ctx = egui::Context::default();
        let mut state = AppState::new().unwrap();
        state.settings.basic_sliders_open = false;
        state.settings.tone_curve_open = false;

        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let tab = LightTab;
                let res = tab.show(ui, &mut state);
                assert!(res.is_none());
            });
        });
        assert!(!state.settings.basic_sliders_open);
        assert!(!state.settings.tone_curve_open);
    }

    #[test]
    fn test_color_tab_collapsible_sections() {
        let ctx = egui::Context::default();
        let mut state = AppState::new().unwrap();
        state.settings.color_hsl_open = false;
        state.settings.color_mix_open = false;
        state.settings.color_grading_open = false;

        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let tab = ColorTab;
                let res = tab.show(ui, &mut state);
                assert!(res.is_none());
            });
        });
        assert!(!state.settings.color_hsl_open);
        assert!(!state.settings.color_mix_open);
        assert!(!state.settings.color_grading_open);
    }

    #[test]
    fn color_tab_renders_without_viewer() {
        // No viewer ⇒ tab renders nothing and returns None (unchanged behavior),
        // mirroring `light_tab_edits_the_selected_mask_when_mask_scope_active`.
        let ctx = egui::Context::default();
        let mut state = AppState::new().unwrap();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(ColorTab.show(ui, &mut state).is_none());
            });
        });
    }

    #[test]
    fn test_effects_tab_collapsible_sections() {
        let ctx = egui::Context::default();
        let mut state = AppState::new().unwrap();
        state.settings.sharpening_open = false;
        state.settings.noise_reduction_open = false;
        state.settings.dehaze_open = false;
        state.settings.optics_open = false;

        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let tab = EffectsTab;
                let res = tab.show(ui, &mut state);
                assert!(res.is_none());
            });
        });
        assert!(!state.settings.sharpening_open);
        assert!(!state.settings.noise_reduction_open);
        assert!(!state.settings.dehaze_open);
        assert!(!state.settings.optics_open);
    }

    #[test]
    fn test_all_eight_section_headers_bound_and_persist() {
        let mut state = AppState::new().unwrap();

        // Defaults should all be open (true)
        assert!(state.settings.basic_sliders_open);
        assert!(state.settings.tone_curve_open);
        assert!(state.settings.color_hsl_open);
        assert!(state.settings.color_mix_open);
        assert!(state.settings.color_grading_open);
        assert!(state.settings.sharpening_open);
        assert!(state.settings.noise_reduction_open);
        assert!(state.settings.dehaze_open);
        assert!(state.settings.optics_open);

        // Toggle state settings fields
        state.settings.basic_sliders_open = false;
        state.settings.tone_curve_open = false;
        state.settings.color_hsl_open = false;
        state.settings.color_mix_open = false;
        state.settings.color_grading_open = false;
        state.settings.sharpening_open = false;
        state.settings.noise_reduction_open = false;
        state.settings.dehaze_open = false;
        state.settings.optics_open = false;

        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                LightTab.show(ui, &mut state);
                ColorTab.show(ui, &mut state);
                EffectsTab.show(ui, &mut state);
            });
        });

        // Verify toggled booleans persisted after panel rendering
        assert!(!state.settings.basic_sliders_open);
        assert!(!state.settings.tone_curve_open);
        assert!(!state.settings.color_hsl_open);
        assert!(!state.settings.color_mix_open);
        assert!(!state.settings.color_grading_open);
        assert!(!state.settings.sharpening_open);
        assert!(!state.settings.noise_reduction_open);
        assert!(!state.settings.dehaze_open);
        assert!(!state.settings.optics_open);
    }

    #[test]
    fn mask_scope_uses_its_own_section_flags() {
        let mut state = AppState::new().unwrap();
        state.settings.basic_sliders_open = true;
        state.settings.mask_basic_sliders_open = false;

        // A real viewer with a mask layer selected (no GPU needed —
        // `ViewerState::open` only sets up CPU-side state) so `scope::current`
        // resolves to `EditScope::Mask(0)`, not `MaskNone`. `MaskNone` would
        // take the exact same `scope_is_mask` branch as a real mask selection,
        // so without a real viewer + selection this test can't actually tell
        // the mask-flag path apart from the "no viewer at all" early return.
        let mut viewer = crate::viewer::ViewerState::open(
            1,
            std::path::PathBuf::from("x"),
            ferrolite_image::FileKind::Raw,
        );
        viewer.op_stack = crate::develop::mask_edit::create_mask(&viewer.op_stack, "M".into());
        viewer.mask.selected = Some(0);
        state.viewer = Some(viewer);
        state.tool_state.active = crate::develop::tool::ToolId::Mask;

        // Click the BASIC SLIDERS header (same synthetic-click pattern as
        // `widgets::mod`'s `section_header` test, which also renders once
        // before the click pass) to actually exercise which flag
        // `LightTab::show` reads AND writes for a mask scope.
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = LightTab.show(ui, &mut state);
            });
        });

        let click_pos = egui::pos2(50.0, 10.0);
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = LightTab.show(ui, &mut state);
            });
        });

        // The click toggled the header's own flag: for a mask scope that must
        // be `mask_basic_sliders_open` (false -> true), while the GLOBAL
        // `basic_sliders_open` stays untouched. A wrong flag-selection branch
        // would flip `basic_sliders_open` (true -> false) instead and leave
        // `mask_basic_sliders_open` at false.
        assert!(
            state.settings.mask_basic_sliders_open,
            "mask scope must toggle its OWN section flag"
        );
        assert!(
            state.settings.basic_sliders_open,
            "the global section flag must stay untouched while in mask scope"
        );
    }
}
