//! Develop right adjustment panel (design-system §6, 296px). CollapsingHeader
//! sections; one EguiSlider per op param; per-section + global reset. Emits a new
//! OpStack via develop::ops_edit; the app applies it to both render tiers.

use crate::develop::{curve_widget, hsl_widget, lens_picker, ops_edit, vignette_mode};
use crate::state::AppState;
use crate::theme;
use crate::widgets::slider::EguiSlider;
use ferrolite_color::WorkingSpace;
use ferrolite_pipeline::{Aspect, Correction, Geometry, LensCorrection, Op, OpKind, OpStack};

/// Fallback focal length seeded for a brand-new `LensCorrection` op ONLY when
/// EXIF has no focal length (`ViewerState::meta` is `None`, still loading, or
/// the decoded `Metadata.focal_length` is itself absent) — a real shot's
/// focal length is preferred whenever it's available (Spec 4.4, U9). The
/// advanced sub-area lets the author correct it immediately either way.
const DEFAULT_FOCAL_LEN: f32 = 50.0;
/// Fallback aperture seeded for a brand-new op; mirrors `query_from_metadata`'s
/// own f/8 fallback for the same "EXIF absent" case.
const DEFAULT_APERTURE: f32 = 8.0;
/// Fallback crop factor seeded for a brand-new op when no auto-match candidate
/// is available yet (unmatched EXIF, DB unavailable, or the match hasn't
/// resolved this frame) — 1.0 (full-frame) is the same neutral default
/// `find_lenses`/`match_by_id` fall back to elsewhere in the lens pipeline.
const DEFAULT_CROP_FACTOR: f32 = 1.0;

pub struct EditOutcome {
    pub stack: OpStack,
    pub kind: OpKind,
    pub commit: bool,
}

/// What the adjustment panel produced this frame: an op edit and/or a working-space change.
pub struct PanelOutcome {
    pub edit: Option<EditOutcome>,
    pub working_space: Option<WorkingSpace>,
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState, working_space: WorkingSpace) -> PanelOutcome {
    let stack = match state.viewer.as_ref() {
        Some(v) => v.op_stack.clone(),
        None => {
            return PanelOutcome {
                edit: None,
                working_space: None,
            }
        }
    };
    let mut out: Option<EditOutcome> = None;
    let mut ws_change: Option<WorkingSpace> = None;

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

    // ── Working space (spec §4.1) ── global preference; not an editable op, so no
    // per-control reset. Recomposes the ColorMatrixNode + display tail on change.
    {
        let mut ws = working_space;
        egui::ComboBox::from_label("Working space")
            .selected_text(format!("{ws:?}"))
            .show_ui(ui, |ui| {
                for w in WorkingSpace::ALL {
                    ui.selectable_value(&mut ws, w, format!("{w:?}"));
                }
            });
        if ws != working_space {
            ws_change = Some(ws);
        }
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

    // ── Lens Corrections ── (Spec 4.4, U8): distortion/TCA/vignetting toggles +
    // Amount, a matched-lens label + picker, and an advanced focal/aperture area.
    egui::CollapsingHeader::new("Lens Corrections").show(ui, |ui| {
        let Some(db) = state.lens_db.clone() else {
            ui.weak("Lens database unavailable — corrections disabled.");
            return;
        };
        // Seed a brand-new op (no `LensCorrection` in the stack yet) from real
        // EXIF + the auto-match candidate (Spec 4.4, U9) rather than the
        // hardcoded 50mm/f8/1.0 placeholders: focal/aperture come straight
        // from the decoded `Metadata` when present, and lens_id/crop_factor
        // come from the auto-match candidate `try_auto_match_lens` stored on
        // the viewer (opt-in: reading it here does NOT enable anything — the
        // corrections below all start `enabled: false`, same as before). Any
        // EXIF/candidate field that's still missing (loading, decode failure,
        // or genuinely absent) falls back to the same constants as before.
        // Copied out as owned values up front (not held as borrows) so the
        // rest of this closure can freely re-borrow `state.viewer` mutably.
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
        let mut changed = false; // toggle/lens/focal/aperture: routes through the bake path
        let mut amount_dragged = false; // an Amount slider moved mid-drag (preview only)
        let mut amount_drag_stopped = false; // an Amount slider drag just released (commit)

        // Matched-lens label + picker launcher + clear. When no op exists yet
        // (no `lens_id` picked/persisted), fall back to showing the Task-14
        // auto-match candidate's name (`auto_match_name`, hoisted above) so
        // the panel reflects the real EXIF match immediately on open, before
        // the user has touched anything — still without creating an op
        // (opt-in preserved).
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
            ui.label(label);
            if ui.small_button("Choose lens\u{2026}").clicked() {
                if let Some(v) = state.viewer.as_mut() {
                    v.lens_picker_open = true;
                    v.lens_picker_query.clear();
                }
            }
            if lc.lens_id.is_some() && ui.small_button("Clear").clicked() {
                new_lc.lens_id = None;
                if let Some(v) = state.viewer.as_mut() {
                    v.lens_resolved_name = None;
                }
                changed = true;
            }
        });

        // The picker modal itself (drawn as a separate egui::Window; only
        // shown while `lens_picker_open`). camera_hint is the real EXIF
        // make/model (Spec 4.4, U9, `seed_camera_hint` hoisted above) when
        // available; empty until `meta` loads or when the decode failed —
        // `find_lenses` degrades gracefully to the lens's own calibration
        // crop in that case (unchanged behavior).
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

        // ── Layout C (MV2) ── A compact "chips" row of toggles for the three
        // corrections, then a detail area below with a full-width Amount slider
        // for each ENABLED correction.
        //
        // Distortion + TCA need lens (Lensfun) data: their chips are DISABLED
        // (greyed) with a hint when no lens is matched — a matched lens_id
        // (persisted) OR the resolved name means we have profile data.
        // Vignetting is ALWAYS enabled: it works lens-free via the parametric
        // manual gain (MV1), so it can be toggled + adjusted with no lens.
        let has_lens = lc.lens_id.is_some()
            || state
                .viewer
                .as_ref()
                .and_then(|v| v.lens_resolved_name.as_ref())
                .is_some();
        // Whether a PROFILE vignette LUT is currently bound decides the
        // Vignetting slider's mode (profile vs. manual) below.
        let has_vignette_lut = state
            .viewer
            .as_ref()
            .map(|v| v.lens_vignette.is_some())
            .unwrap_or(false);

        let mut chips_changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Corrections:");
            // A lens-gated chip: greyed + hinted when no lens is matched.
            let chip = |ui: &mut egui::Ui, label: &str, c: &mut Correction, changed: &mut bool| {
                let resp = ui
                    .add_enabled(has_lens, egui::SelectableLabel::new(c.enabled, label))
                    .on_disabled_hover_text("Select a lens to enable");
                if resp.clicked() {
                    c.enabled = !c.enabled;
                    *changed = true;
                }
            };
            chip(ui, "Distortion", &mut new_lc.distortion, &mut chips_changed);
            chip(ui, "Transverse CA", &mut new_lc.tca, &mut chips_changed);
            // Vignetting chip is always enabled (manual works lens-free).
            let vresp = ui.selectable_label(new_lc.vignetting.enabled, "Vignetting");
            if vresp.clicked() {
                let now_on = !new_lc.vignetting.enabled;
                new_lc.vignetting.enabled = now_on;
                // On ENABLE in manual mode, seed the neutral 0.0 gain instead of
                // the profile default 1.0 (which would be full brightening).
                // Doing it at the toggle (not every frame) means a user CAN dial
                // manual amount up to +1.0 without it snapping back (MV2).
                if now_on
                    && !has_vignette_lut
                    && (new_lc.vignetting.amount - 1.0).abs() < f32::EPSILON
                {
                    new_lc.vignetting.amount = vignette_mode::MANUAL_PARAMS.default;
                }
                chips_changed = true;
            }
        });
        if chips_changed {
            changed = true;
        }
        ui.add_space(4.0);

        // Detail area: a full-width Amount slider for each ENABLED correction.
        // Each slider keeps its OWN reset arrow (CLAUDE.md: per-control reset is
        // load-bearing — `EguiSlider` always renders + wires `draw_reset_arrow`
        // in its reset column, so every visible slider is independently
        // resettable). Distortion/TCA are unchanged (0..2, reset 1). The
        // Vignetting slider is MODE-AWARE (MV2): profile-correction strength
        // (0..2, reset 1, unipolar) when a profile LUT is bound, else a bipolar
        // lens-free manual gain (-1..1, reset 0).
        let amount_row = |ui: &mut egui::Ui,
                          label: &str,
                          value: &mut f32,
                          params: vignette_mode::VigSliderParams,
                          dragged: &mut bool,
                          drag_stopped: &mut bool| {
            let r = ui.add(EguiSlider {
                label,
                value,
                min: params.min,
                max: params.max,
                default: params.default,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: params.bipolar,
                signed: params.bipolar,
            });
            if r.changed() {
                if r.drag_stopped() {
                    *drag_stopped = true;
                } else if r.dragged() {
                    *dragged = true;
                } else {
                    // Click / double-click-reset / typed entry: commit immediately.
                    *drag_stopped = true;
                }
            }
        };

        let mut any_detail = false;
        if new_lc.distortion.enabled {
            any_detail = true;
            amount_row(
                ui,
                "Distortion",
                &mut new_lc.distortion.amount,
                vignette_mode::PROFILE_PARAMS,
                &mut amount_dragged,
                &mut amount_drag_stopped,
            );
        }
        if new_lc.tca.enabled {
            any_detail = true;
            amount_row(
                ui,
                "Transverse CA",
                &mut new_lc.tca.amount,
                vignette_mode::PROFILE_PARAMS,
                &mut amount_dragged,
                &mut amount_drag_stopped,
            );
        }
        if new_lc.vignetting.enabled {
            any_detail = true;
            amount_row(
                ui,
                "Vignetting",
                &mut new_lc.vignetting.amount,
                vignette_mode::slider_params(has_vignette_lut),
                &mut amount_dragged,
                &mut amount_drag_stopped,
            );
        }
        if !any_detail {
            ui.weak("Enable a correction above to adjust it.");
        }

        // Advanced: focal length + aperture used by the bake (EXIF-seeded once
        // that plumbing exists; editable now so the author can correct them).
        // A focal/aperture edit changes the bake inputs, so it routes through
        // the same `changed` (bake-triggering) path as a toggle/lens pick,
        // never the Amount-only uniform path.
        let mut advanced_dragged = false;
        let mut advanced_drag_stopped = false;
        egui::CollapsingHeader::new("Advanced")
            .id_salt("lens_corrections_advanced")
            .show(ui, |ui| {
                let rf = ui.add(EguiSlider {
                    label: "Focal",
                    value: &mut new_lc.focal_len,
                    min: 8.0,
                    max: 800.0,
                    default: DEFAULT_FOCAL_LEN,
                    step: 1.0,
                    decimals: 0,
                    unit: " mm",
                    bipolar: false,
                    signed: false,
                });
                let ra = ui.add(EguiSlider {
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
                });
                if rf.changed() || ra.changed() {
                    if rf.drag_stopped() || ra.drag_stopped() {
                        advanced_drag_stopped = true;
                    } else if rf.dragged() || ra.dragged() {
                        advanced_dragged = true;
                    } else {
                        advanced_drag_stopped = true;
                    }
                }
            });
        let advanced_changed = advanced_dragged || advanced_drag_stopped;

        // Emit: a toggle/lens-pick/focal/aperture change (`changed` or an
        // advanced-field edit) always routes through `set_lens_correction`
        // with `kind: LensCorrection`, so `apply_edit`'s existing
        // `maybe_spawn_lens_bake` gate (keyed on `lens_rebuild_key`, U7)
        // decides bake-vs-not — this panel never calls the bake directly. An
        // Amount-only drag emits the SAME kind but never changes
        // `lens_rebuild_key`, so `maybe_spawn_lens_bake` naturally no-ops for
        // it (uniform-only update via `set_preview_and_full`). Mid-drag frames
        // (focal/aperture/Amount) commit=false, same convention as every
        // other slider in this panel (`drag_stopped() || !dragged()`).
        if changed || advanced_changed || amount_dragged || amount_drag_stopped {
            let s = ops_edit::set_lens_correction(&stack, new_lc);
            let any_dragging = advanced_dragged || amount_dragged;
            let any_drag_stopped = advanced_drag_stopped || amount_drag_stopped;
            let commit = changed || any_drag_stopped || !any_dragging;
            out = Some(EditOutcome {
                stack: s,
                kind: OpKind::LensCorrection,
                commit,
            });
        }

        if (lc.lens_id.is_some()
            || lc.distortion.enabled
            || lc.tca.enabled
            || lc.vignetting.enabled)
            && ui.small_button("Reset").clicked()
        {
            if let Some(v) = state.viewer.as_mut() {
                v.lens_resolved_name = None;
            }
            out = Some(EditOutcome {
                stack: stack.reset(OpKind::LensCorrection),
                kind: OpKind::LensCorrection,
                commit: true,
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

    PanelOutcome {
        edit: out,
        working_space: ws_change,
    }
}
