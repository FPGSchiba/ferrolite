//! The always-present global adjustment tabs (design §7/§8). Each wraps an existing
//! `adjustment_panel` section body verbatim as a `PanelTab`; per-control reset is
//! preserved because each keeps its `EguiSlider` (the reset column is baked into the
//! widget). `base_tabs()` is registered once as the registry's base.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::tool::{PanelTab, TabId};
use crate::develop::{
    curve_widget, hsl_widget, lens_caps_ui, lens_picker, ops_edit, vignette_mode,
};
use crate::state::AppState;
use crate::theme;
use crate::widgets::slider::EguiSlider;
use ferrolite_lens::LensDb;
use ferrolite_pipeline::{Correction, LensCorrection, OpKind};

/// Fallback focal length seeded for a brand-new `LensCorrection` op ONLY when
/// EXIF has no focal length (`ViewerState::meta` is `None`, still loading, or
/// the decoded `Metadata.focal_length` is itself absent) — a real shot's
/// focal length is preferred whenever it's available (Spec 4.4, U9). The
/// per-component "Adjust" expander lets the author correct it immediately
/// either way.
const DEFAULT_FOCAL_LEN: f32 = 50.0;
/// Fallback aperture seeded for a brand-new op; mirrors `query_from_metadata`'s
/// own f/8 fallback for the same "EXIF absent" case.
const DEFAULT_APERTURE: f32 = 8.0;
/// Fallback crop factor seeded for a brand-new op when no auto-match candidate
/// is available yet (unmatched EXIF, DB unavailable, or the match hasn't
/// resolved this frame) — 1.0 (full-frame) is the same neutral default
/// `find_lenses`/`match_by_id` fall back to elsewhere in the lens pipeline.
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
                commit: (rt.drag_stopped() || rn.drag_stopped()) || !(rt.dragged() || rn.dragged()),
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

        if let Some(v) = state.viewer.as_mut() {
            if let Some(o) = hsl_widget::show(ui, &stack, &mut v.hsl_band) {
                out = Some(o);
            }
        }

        out
    }
}

pub struct CurveTab;
impl PanelTab for CurveTab {
    fn id(&self) -> TabId {
        TabId("curve")
    }
    fn label(&self) -> &str {
        "Curve"
    }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        let stack = state.viewer.as_ref()?.op_stack.clone();
        let mut out: Option<EditOutcome> = None;

        if let Some(o) = curve_widget::show(ui, &stack) {
            out = Some(o);
        }

        out
    }
}

pub struct DetailTab;
impl PanelTab for DetailTab {
    fn id(&self) -> TabId {
        TabId("detail")
    }
    fn label(&self) -> &str {
        "Detail"
    }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        let stack = state.viewer.as_ref()?.op_stack.clone();
        let mut out: Option<EditOutcome> = None;

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

        out
    }
}

pub struct OpticsTab;
impl PanelTab for OpticsTab {
    fn id(&self) -> TabId {
        TabId("optics")
    }
    fn label(&self) -> &str {
        "Optics"
    }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        let stack = state.viewer.as_ref()?.op_stack.clone();
        let mut out: Option<EditOutcome> = None;

        let Some(db) = state.lens_db.clone() else {
            ui.weak("Lens database unavailable — corrections disabled.");
            return out;
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
            // Reserve space for the buttons first (right-aligned) so a long
            // lens name can never push `Choose lens…`/`Clear` out of the
            // panel; the name then gets whatever width remains and is
            // truncated with an ellipsis (full name on hover) rather than
            // expanding the row.
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
                // A truncated egui `Label` already shows the full text on hover;
                // adding an explicit `on_hover_text` here rendered the tooltip
                // TWICE. Keep only the built-in one (name shown once on hover).
                ui.add(egui::Label::new(label).truncate());
            });
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
                    // Seed/clamp the focal length into the newly-picked
                    // lens's own calibrated range: a focal carried over from
                    // a previous (or default) lens can be way outside this
                    // one's range (e.g. a stale 800mm on a 14-54mm zoom),
                    // which Lensfun would silently clamp internally — making
                    // the correction look like it "does nothing". Clamping
                    // here instead keeps the visible slider value in sync
                    // with what the bake will actually use. An EXIF-seeded
                    // focal that's already in-range is left untouched. If the
                    // range can't be resolved, leave focal as-is (unchanged
                    // behavior).
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

        // ── Layout v2 (round 5) ── Title-above-row group per correction:
        //   Line 1 (title): the correction name, full-width, left-aligned to
        //     the checkbox below. Availability is now surfaced INLINE here
        //     (greyed + " — reason", via `lens_caps_ui::correction_title`)
        //     instead of hover-only — a hover-only reason was easy to miss.
        //   Line 2 (control): `[checkbox] + <EguiSlider, empty label> + value + reset`.
        // Long names ("Transverse CA") no longer crowd the slider (MV3's
        // single-row layout wrapped them awkwardly), and the slider spans the
        // full width under the title since its label column collapses when
        // empty (see `EguiSlider`/`widgets::slider`).
        //
        // Distortion + TCA need lens (Lensfun) data: the row (title + control)
        // is disabled/greyed when no lens is matched — a matched lens_id
        // (persisted) OR the resolved name means we have profile data.
        // Vignetting's row is ALWAYS enabled: it works lens-free via the
        // parametric manual gain (MV1), so it can be toggled + adjusted with
        // no lens.
        //
        // Spacing (author-specified): TIGHT between a group's title and its
        // control line (they must read as one unit), LARGER between groups
        // (separates the three corrections). Applied via explicit
        // `ui.add_space` calls inside `correction_row` below (not
        // `spacing_mut`) so the gap is local to each group and doesn't leak
        // into surrounding sections (a per-component Adjust expander, the
        // section Reset button, etc.).
        const TITLE_TO_CONTROL_GAP: f32 = 2.0;
        const BETWEEN_GROUP_GAP: f32 = 10.0;
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

        // Per-correction data availability for the matched lens at the
        // current focal/aperture (FB2, `ferrolite_lens::LensDb::lens_caps`).
        // `lens_caps` is a cheap in-memory interpolate-presence check (no I/O,
        // no bake), so it's fine to call inline on the UI thread every render
        // — same cost class as `find_lenses` in `lens_picker.rs` above.
        // `None` when no lens is matched yet OR the persisted `lens_id`
        // doesn't resolve (stale profile); either way the Distortion/TCA
        // gates below fall back to the same "Needs a matched lens" hint as
        // pre-FB2, since we can't claim specific missing data we didn't
        // actually get to check.
        let caps = new_lc
            .lens_id
            .as_deref()
            .and_then(|id| db.lens_caps(id, new_lc.focal_len, new_lc.aperture));

        // Each slider keeps its OWN reset arrow (CLAUDE.md: per-control reset
        // is load-bearing — `EguiSlider` always renders + wires
        // `draw_reset_arrow` in its reset column, so every visible slider is
        // independently resettable, checkbox-enabled or not). Distortion/TCA
        // are unchanged (0..2, reset 1). The Vignetting slider is MODE-AWARE
        // (MV2): profile-correction strength (0..2, reset 1, unipolar) when a
        // profile LUT is bound, else a bipolar lens-free manual gain (-1..1,
        // reset 0).
        let correction_row = |ui: &mut egui::Ui,
                              name: &str,
                              c: &mut Correction,
                              params: vignette_mode::VigSliderParams,
                              row_enabled: bool,
                              hover_text: Option<&str>,
                              dragged: &mut bool,
                              drag_stopped: &mut bool,
                              toggled: &mut bool| {
            // Zero the ambient vertical item_spacing (egui default 3.0) for
            // this group so the two explicit `add_space` calls below are the
            // ONLY vertical gaps in play — otherwise the default would stack
            // on top of them and the tight title↔control gap would read the
            // same as the loose between-group gap.
            let prev_spacing_y = ui.spacing().item_spacing.y;
            ui.spacing_mut().item_spacing.y = 0.0;

            // Line 1: title, greyed + reason inline when unavailable (still
            // ALSO exposed as `on_disabled_hover_text` on the checkbox below
            // for parity with the rest of the panel's disabled controls).
            let title = lens_caps_ui::correction_title(name, row_enabled, hover_text);
            let title_text = if row_enabled {
                egui::RichText::new(title)
            } else {
                egui::RichText::new(title).color(theme::TEXT_DIM)
            };
            ui.label(title_text);
            ui.add_space(TITLE_TO_CONTROL_GAP);

            // Line 2: checkbox + full-width slider (empty label — the title
            // above already carries the name) + value + reset.
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
                });
            });

            // Restore ambient spacing before the (larger, explicit) gap
            // between groups so sibling widgets outside `correction_row`
            // (e.g. the Vignette group after this one, or a per-component
            // "Adjust" expander) aren't left with the zeroed spacing.
            ui.spacing_mut().item_spacing.y = prev_spacing_y;
            ui.add_space(BETWEEN_GROUP_GAP);
        };

        // Round 8 (author visual test): Focal and Aperture used to live in a
        // standalone "Advanced" section at the bottom, which read as if they
        // were their own independent effects. They aren't — Focal is an input
        // to the Distortion (and, invisibly, TCA) bake, and Aperture only
        // matters for PROFILE vignetting. Each now lives collapsed under an
        // "Adjust" expander INSIDE the correction it actually drives, so the
        // relationship is obvious from placement alone. Both still route
        // through the same bake-triggering `changed`/commit path as before —
        // only their location in the tree changed, not their wiring.
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
        // Focal · Adjust — only when a lens is matched at all (same
        // predicate as the Distortion/TCA gates above): focal selects the
        // Lensfun calibration point, so it has no effect with no lens
        // matched. The Distortion row above is already greyed in that case;
        // omitting Focal entirely (rather than greying it) avoids a dead
        // control with nothing to explain beyond what the row already says.
        //
        // The range is clamped to the matched lens's own calibrated focal
        // range (FB3): letting the author drag it way outside that range
        // (e.g. 800mm on a 14-54mm zoom) used to silently get clamped by
        // Lensfun internally, which reads as "the correction does nothing".
        // Falls back to the pre-FB3 default 8..800 range when the range
        // can't be resolved.
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
            true, // Vignetting row is always enabled (manual works lens-free).
            None, // Never disabled, so there's nothing to hover-explain.
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
            // On ENABLE in manual mode, seed the neutral 0.0 gain instead of
            // the profile default 1.0 (which would be full brightening).
            // Doing it at the toggle (not every frame) means a user CAN dial
            // manual amount up to +1.0 without it snapping back (MV2).
            new_lc.vignetting.amount = vignette_mode::MANUAL_PARAMS.default;
        }
        // Aperture · Adjust — ONLY when the matched lens has a vignetting
        // PROFILE (`caps.vignetting == true`). Aperture is exclusively an
        // input to profile vignetting (distortion/TCA don't take it at all),
        // so for a manual-vignette lens (no profile) or no lens at all it has
        // no effect whatsoever — rather than showing it greyed with an
        // explanatory hint (the pre-round-8 approach), it's simply absent:
        // there's nothing here for the author to be confused about.
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

        // Emit: a toggle/lens-pick/focal/aperture change (`changed` or an
        // Adjust-field edit) always routes through `set_lens_correction` with
        // `kind: LensCorrection`, so `apply_edit`'s existing
        // `maybe_spawn_lens_bake` gate (keyed on `lens_rebuild_key`, U7)
        // decides bake-vs-not — this panel never calls the bake directly. An
        // Amount-only drag emits the SAME kind but never changes
        // `lens_rebuild_key`, so `maybe_spawn_lens_bake` naturally no-ops for
        // it (uniform-only update via `set_preview_and_full`). Mid-drag frames
        // (focal/aperture/Amount) commit=false, same convention as every
        // other slider in this panel (`drag_stopped() || !dragged()`).
        let adjust_changed = adjust_dragged || adjust_drag_stopped;
        if changed || adjust_changed || amount_dragged || amount_drag_stopped {
            let s = ops_edit::set_lens_correction(&stack, new_lc);
            let any_dragging = adjust_dragged || amount_dragged;
            let any_drag_stopped = adjust_drag_stopped || amount_drag_stopped;
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

        out
    }
}

pub fn base_tabs() -> Vec<Box<dyn PanelTab>> {
    vec![
        Box::new(LightTab),
        Box::new(ColorTab),
        Box::new(CurveTab),
        Box::new(DetailTab),
        Box::new(OpticsTab),
    ]
}
