//! Reusable interactive curve editor widget. Pure point math lives in
//! `crate::develop::curve_math`; this layer paints + routes pointer events and
//! is generic over the caller's `id_source` so multiple curve editors can
//! coexist on one screen (e.g. tone curve + future per-channel color curves).
//!
//! This module is the reusable widget (Spec 4.1 CD2 Task 4/5 of the
//! curve-spline-modes plan), wired in by the tone-curve adapter
//! (`develop::curve_widget`, Task 6).

use crate::develop::curve_math::{self, GrabOrInsert};
use crate::theme;
use crate::widgets::chips::SegmentedControl;
use crate::widgets::draw_reset_arrow;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{curve_lut, CurveMode};

const SIZE: f32 = 260.0; // square edit area
const HIT_R: f32 = 0.06; // normalized hit radius
const DOT_R: f32 = 5.0; // idle point-dot radius
const DOT_R_HOVER: f32 = 6.5; // enlarged radius for the hovered point
const MODE_RESET_R: f32 = 4.5; // mode-selector reset arrow radius

/// The app default curve mode for newly-created curves (Spec 4.1 CD2 Task 6);
/// also the target the mode selector's own reset returns to.
const DEFAULT_MODE: CurveMode = CurveMode::Smooth;

/// Visual styling for a `curve_editor` instance, so different curve uses
/// (tone curve vs. future per-channel color curves) can have distinct colors.
pub struct CurveStyle {
    pub curve_color: egui::Color32,
    pub point_color: egui::Color32,
}

/// A change emitted by `curve_editor`. `None` is returned when nothing
/// changed this frame.
pub struct CurveEdit {
    pub points: Vec<(f32, f32)>,
    pub mode: CurveMode,
    pub reset: bool,
    pub commit: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneCurveTab {
    Point,
    Parametric,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct ParametricCurveValues {
    pub highlights: f32,
    pub lights: f32,
    pub darks: f32,
    pub shadows: f32,
    pub shadow_split: f32,
    pub midtone_split: f32,
    pub highlight_split: f32,
}

impl Default for ParametricCurveValues {
    fn default() -> Self {
        Self {
            highlights: 0.0_f32,
            lights: 0.0_f32,
            darks: 0.0_f32,
            shadows: 0.0_f32,
            shadow_split: 0.25_f32,
            midtone_split: 0.50_f32,
            highlight_split: 0.75_f32,
        }
    }
}

impl ParametricCurveValues {
    #[allow(dead_code)]
    pub fn to_pipeline(&self) -> ferrolite_pipeline::ParametricCurve {
        ferrolite_pipeline::ParametricCurve {
            highlights: self.highlights / 100.0_f32,
            lights: self.lights / 100.0_f32,
            darks: self.darks / 100.0_f32,
            shadows: self.shadows / 100.0_f32,
            shadow_split: self.shadow_split,
            midtone_split: self.midtone_split,
            highlight_split: self.highlight_split,
        }
    }

    #[allow(dead_code)]
    pub fn from_pipeline(p: &ferrolite_pipeline::ParametricCurve) -> Self {
        Self {
            highlights: p.highlights * 100.0_f32,
            lights: p.lights * 100.0_f32,
            darks: p.darks * 100.0_f32,
            shadows: p.shadows * 100.0_f32,
            shadow_split: p.shadow_split,
            midtone_split: p.midtone_split,
            highlight_split: p.highlight_split,
        }
    }
}

#[allow(dead_code)]
pub struct ToneCurveEdit {
    pub points: Option<Vec<(f32, f32)>>,
    pub parametric: Option<ParametricCurveValues>,
    pub mode: CurveMode,
    pub reset: bool,
    pub commit: bool,
}

/// Paint + interact with a curve editor bound to `points`/`mode`. All memory
/// keys are salted with `id_source` so two instances on one screen don't
/// collide. Returns `Some(CurveEdit)` on any change (drag, insert, delete,
/// reset, or mode change), `None` otherwise.
pub fn curve_editor(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    points: &[(f32, f32)],
    mode: CurveMode,
    style: &CurveStyle,
) -> Option<CurveEdit> {
    let base_id = ui.id().with(id_source);
    let mut points = points.to_vec();

    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::click_and_drag());

    let selected_id = base_id.with("selected_point");
    let grab_id = base_id.with("grab_point");
    let mut selected: Option<usize> = ui
        .memory(|m| m.data.get_temp::<Option<usize>>(selected_id))
        .unwrap_or(None);

    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::BG_BASE);
    // Grid (quarters).
    for i in 1..4 {
        let f = i as f32 / 4.0;
        painter.line_segment(
            [
                egui::pos2(rect.left() + f * SIZE, rect.top()),
                egui::pos2(rect.left() + f * SIZE, rect.bottom()),
            ],
            egui::Stroke::new(1.0_f32, theme::BORDER_STRONG),
        );
        painter.line_segment(
            [
                egui::pos2(rect.left(), rect.top() + f * SIZE),
                egui::pos2(rect.right(), rect.top() + f * SIZE),
            ],
            egui::Stroke::new(1.0_f32, theme::BORDER_STRONG),
        );
    }

    // Coord transforms: image y is inverted on screen (0 at bottom).
    let to_screen =
        |p: (f32, f32)| egui::pos2(rect.left() + p.0 * SIZE, rect.bottom() - p.1 * SIZE);
    let to_norm = |s: egui::Pos2| ((s.x - rect.left()) / SIZE, (rect.bottom() - s.y) / SIZE);

    // Curve polyline: sample the pipeline's own interpolation for `mode` so the
    // drawn shape matches the applied result (straight segments for Linear,
    // a smooth monotone curve for Smooth) rather than connecting control
    // points directly.
    let lut = curve_lut(&points, mode);
    let poly: Vec<egui::Pos2> = lut
        .iter()
        .enumerate()
        .map(|(i, &y)| to_screen((i as f32 / 255.0, y)))
        .collect();
    painter.add(egui::Shape::line(
        poly,
        egui::Stroke::new(1.5_f32, style.curve_color),
    ));

    // Hover highlight: the point the cursor is currently within HIT_R of.
    let hovered_idx = resp
        .hover_pos()
        .and_then(|p| curve_math::nearest_point(&points, to_norm(p), HIT_R));

    for (i, &p) in points.iter().enumerate() {
        let is_hovered = hovered_idx == Some(i);
        let is_selected = selected == Some(i);
        let radius = if is_hovered { DOT_R_HOVER } else { DOT_R };
        painter.circle(
            to_screen(p),
            radius,
            style.point_color,
            egui::Stroke::new(1.0_f32, theme::BG_BASE),
        );
        if is_selected {
            // Accent ring around the selected point so selection reads clearly
            // and independently of hover state.
            painter.circle_stroke(
                to_screen(p),
                radius + 3.0,
                egui::Stroke::new(1.5_f32, style.curve_color),
            );
        }
    }

    let mut changed = false;
    let mut commit = false;
    let mut deleted = false;

    if let Some(pos) = resp.interact_pointer_pos() {
        let norm = to_norm(pos);
        if resp.drag_started() || resp.clicked() {
            match curve_math::grab_or_insert(&points, norm, HIT_R) {
                GrabOrInsert::Grab(idx) => {
                    ui.memory_mut(|m| m.data.insert_temp(grab_id, idx));
                    if resp.clicked() && !resp.dragged() {
                        // A plain click (not a drag) on an existing point selects it.
                        selected = Some(idx);
                        ui.memory_mut(|m| m.data.insert_temp(selected_id, selected));
                    }
                }
                GrabOrInsert::Insert => {
                    // Insert at the clamped coordinate, then grab THAT point by its
                    // exact (bit-identical) value — nearest_point can resolve to a
                    // neighbor on a crowded curve.
                    let inserted = (norm.0.clamp(0.0, 1.0), norm.1.clamp(0.0, 1.0));
                    points = curve_math::insert_point(&points, norm);
                    let idx = points.iter().position(|&q| q == inserted).unwrap_or(0);
                    ui.memory_mut(|m| m.data.insert_temp(grab_id, idx));
                    changed = true;
                    commit = true;
                }
            }
        }
        if resp.dragged() {
            if let Some(idx) = ui.memory(|m| m.data.get_temp::<usize>(grab_id)) {
                points = curve_math::move_point(&points, idx, norm);
                changed = true;
            }
        }
    }
    if resp.drag_stopped() {
        commit = true;
    }

    // Double-click a point to delete it.
    if resp.double_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(idx) = curve_math::nearest_point(&points, to_norm(pos), HIT_R) {
                points = curve_math::delete_point(&points, idx);
                changed = true;
                commit = true;
                deleted = true;
            }
        }
    }
    // Right-click a point to delete it.
    if resp.secondary_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(idx) = curve_math::nearest_point(&points, to_norm(pos), HIT_R) {
                points = curve_math::delete_point(&points, idx);
                changed = true;
                commit = true;
                deleted = true;
            }
        }
    }
    // Delete/Backspace removes the selected point, if any.
    if let Some(idx) = selected {
        let delete_key_pressed =
            ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace));
        if delete_key_pressed {
            points = curve_math::delete_point(&points, idx);
            changed = true;
            commit = true;
            deleted = true;
        }
    }
    if deleted {
        // The selected index may now be out of range (or the deletion was a
        // no-op on a protected endpoint); clear it either way so a stale
        // index can't linger and drive a later Delete press.
        selected = None;
        ui.memory_mut(|m| m.data.insert_temp(selected_id, selected));
        // Also clear the active grab/drag index. If a point is deleted while
        // a drag-grab index is still stashed there, the list can shrink such
        // that the stale index is still in range, and a subsequent
        // resp.dragged() would silently move the wrong point via move_point.
        ui.memory_mut(|m| m.data.remove::<usize>(grab_id));
    }

    ui.small(
        egui::RichText::new("Drag to adjust · double/right-click or Delete to remove a point")
            .color(theme::TEXT_FAINT),
    );

    let mut result: Option<CurveEdit> = None;

    ui.horizontal(|ui| {
        // Per-component reset affordance, styled like the Basic section's "Reset"
        // (see CLAUDE.md "Per-component reset" rule). Dim/disabled at default.
        let modified = !curve_math::is_identity(&points);
        if ui
            .add_enabled(modified, egui::Button::new("Reset").small())
            .clicked()
        {
            // Resetting clears any stale selection tied to the old point list.
            ui.memory_mut(|m| m.data.insert_temp::<Option<usize>>(selected_id, None));
            result = Some(CurveEdit {
                points: curve_math::identity_points(),
                mode,
                reset: true,
                commit: true,
            });
        }

        ui.add_space(8.0);

        // Mode selector: Linear / Smooth segmented control, with its own
        // per-control reset (CLAUDE.md rule) back to the app default (Smooth).
        let mut new_mode = mode;
        if ui
            .selectable_label(mode == CurveMode::Linear, "Linear")
            .clicked()
        {
            new_mode = CurveMode::Linear;
        }
        if ui
            .selectable_label(mode == CurveMode::Smooth, "Smooth")
            .clicked()
        {
            new_mode = CurveMode::Smooth;
        }

        let (mode_reset_rect, _) =
            ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
        let mode_modified = mode != DEFAULT_MODE;
        let mode_reset_resp = ui.interact(
            mode_reset_rect,
            base_id.with("mode_reset"),
            egui::Sense::click(),
        );
        let mode_reset_color = if mode_modified {
            if mode_reset_resp.hovered() {
                style.curve_color
            } else {
                theme::TEXT_FAINT
            }
        } else {
            theme::BORDER_STRONG
        };
        draw_reset_arrow(
            ui.painter(),
            mode_reset_rect.center(),
            MODE_RESET_R,
            mode_reset_color,
        );
        if mode_reset_resp.clicked() && mode_modified {
            new_mode = DEFAULT_MODE;
        }

        if new_mode != mode && result.is_none() {
            result = Some(CurveEdit {
                points: points.clone(),
                mode: new_mode,
                reset: false,
                commit: true,
            });
        }
    });

    if result.is_some() {
        return result;
    }

    if changed {
        return Some(CurveEdit {
            points,
            mode,
            reset: false,
            commit,
        });
    }
    None
}

/// Tone curve widget providing both Point curve editing and Parametric region/split sliders.
#[allow(dead_code)]
pub fn tone_curve_widget(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    active_tab: &mut ToneCurveTab,
    points: &[(f32, f32)],
    mode: CurveMode,
    style: &CurveStyle,
    parametric: &ParametricCurveValues,
) -> Option<ToneCurveEdit> {
    let base_id = ui.id().with(id_source);

    let options = [
        (ToneCurveTab::Point, "Point"),
        (ToneCurveTab::Parametric, "Parametric"),
    ];
    SegmentedControl::new(active_tab, &options).ui(ui, base_id.with("tab_switcher"));

    ui.add_space(4.0_f32);

    match active_tab {
        ToneCurveTab::Point => {
            if let Some(edit) = curve_editor(ui, base_id.with("point_editor"), points, mode, style)
            {
                Some(ToneCurveEdit {
                    points: Some(edit.points),
                    parametric: None,
                    mode: edit.mode,
                    reset: edit.reset,
                    commit: edit.commit,
                })
            } else {
                None
            }
        }
        ToneCurveTab::Parametric => {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(SIZE, 140.0_f32), egui::Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, 2.0_f32, theme::BG_BASE);

            painter.line_segment(
                [
                    egui::pos2(rect.left(), rect.bottom()),
                    egui::pos2(rect.right(), rect.top()),
                ],
                egui::Stroke::new(1.0_f32, theme::BORDER_STRONG),
            );

            let lut = ferrolite_pipeline::parametric_curve_lut(&parametric.to_pipeline());
            let poly: Vec<egui::Pos2> = lut
                .iter()
                .enumerate()
                .map(|(i, &y)| {
                    egui::pos2(
                        rect.left() + (i as f32 / 255.0_f32) * rect.width(),
                        rect.bottom() - y * rect.height(),
                    )
                })
                .collect();
            painter.add(egui::Shape::line(
                poly,
                egui::Stroke::new(1.5_f32, style.curve_color),
            ));

            ui.add_space(8.0_f32);

            let mut p = parametric.clone();
            let before = p.clone();

            let mut dragged = false;
            let mut drag_stopped = false;
            let mut add_slider = |ui: &mut egui::Ui, s: EguiSlider| {
                let r = ui.add(s);
                if r.changed() {
                    if r.drag_stopped() {
                        drag_stopped = true;
                    } else if r.dragged() {
                        dragged = true;
                    } else {
                        drag_stopped = true;
                    }
                }
            };

            add_slider(
                ui,
                EguiSlider {
                    label: "Highlights",
                    value: &mut p.highlights,
                    min: -100.0_f32,
                    max: 100.0_f32,
                    default: 0.0_f32,
                    step: 1.0_f32,
                    decimals: 0,
                    unit: "",
                    bipolar: true,
                    signed: true,
                    custom_label_w: None,
                },
            );
            add_slider(
                ui,
                EguiSlider {
                    label: "Lights",
                    value: &mut p.lights,
                    min: -100.0_f32,
                    max: 100.0_f32,
                    default: 0.0_f32,
                    step: 1.0_f32,
                    decimals: 0,
                    unit: "",
                    bipolar: true,
                    signed: true,
                    custom_label_w: None,
                },
            );
            add_slider(
                ui,
                EguiSlider {
                    label: "Darks",
                    value: &mut p.darks,
                    min: -100.0_f32,
                    max: 100.0_f32,
                    default: 0.0_f32,
                    step: 1.0_f32,
                    decimals: 0,
                    unit: "",
                    bipolar: true,
                    signed: true,
                    custom_label_w: None,
                },
            );
            add_slider(
                ui,
                EguiSlider {
                    label: "Shadows",
                    value: &mut p.shadows,
                    min: -100.0_f32,
                    max: 100.0_f32,
                    default: 0.0_f32,
                    step: 1.0_f32,
                    decimals: 0,
                    unit: "",
                    bipolar: true,
                    signed: true,
                    custom_label_w: None,
                },
            );

            ui.add_space(4.0_f32);

            add_slider(
                ui,
                EguiSlider {
                    label: "Shadow Split",
                    value: &mut p.shadow_split,
                    min: 0.10_f32,
                    max: 0.40_f32,
                    default: 0.25_f32,
                    step: 0.01_f32,
                    decimals: 2,
                    unit: "",
                    bipolar: false,
                    signed: false,
                    custom_label_w: None,
                },
            );
            add_slider(
                ui,
                EguiSlider {
                    label: "Midtone Split",
                    value: &mut p.midtone_split,
                    min: 0.40_f32,
                    max: 0.70_f32,
                    default: 0.50_f32,
                    step: 0.01_f32,
                    decimals: 2,
                    unit: "",
                    bipolar: false,
                    signed: false,
                    custom_label_w: None,
                },
            );
            add_slider(
                ui,
                EguiSlider {
                    label: "Highlight Split",
                    value: &mut p.highlight_split,
                    min: 0.70_f32,
                    max: 0.90_f32,
                    default: 0.75_f32,
                    step: 0.01_f32,
                    decimals: 2,
                    unit: "",
                    bipolar: false,
                    signed: false,
                    custom_label_w: None,
                },
            );

            ui.add_space(4.0_f32);

            let mut reset_clicked = false;
            let modified = p != ParametricCurveValues::default();
            if ui
                .add_enabled(modified, egui::Button::new("Reset curve").small())
                .clicked()
            {
                p = ParametricCurveValues::default();
                reset_clicked = true;
            }

            if reset_clicked {
                return Some(ToneCurveEdit {
                    points: None,
                    parametric: Some(ParametricCurveValues::default()),
                    mode,
                    reset: true,
                    commit: true,
                });
            }

            if p != before {
                return Some(ToneCurveEdit {
                    points: None,
                    parametric: Some(p),
                    mode,
                    reset: false,
                    commit: drag_stopped || !dragged,
                });
            }

            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Context, RawInput};

    #[test]
    fn parametric_curve_values_default() {
        let p = ParametricCurveValues::default();
        assert_eq!(p.highlights, 0.0);
        assert_eq!(p.lights, 0.0);
        assert_eq!(p.darks, 0.0);
        assert_eq!(p.shadows, 0.0);
        assert_eq!(p.shadow_split, 0.25);
        assert_eq!(p.midtone_split, 0.50);
        assert_eq!(p.highlight_split, 0.75);
    }

    #[test]
    fn parametric_curve_values_pipeline_conversion() {
        let p = ParametricCurveValues {
            highlights: 50.0,
            lights: -20.0,
            darks: 10.0,
            shadows: -50.0,
            shadow_split: 0.30,
            midtone_split: 0.60,
            highlight_split: 0.80,
        };
        let pipe = p.to_pipeline();
        assert_eq!(pipe.highlights, 0.5);
        assert_eq!(pipe.lights, -0.2);
        assert_eq!(pipe.darks, 0.1);
        assert_eq!(pipe.shadows, -0.5);
        assert_eq!(pipe.shadow_split, 0.30);
        assert_eq!(pipe.midtone_split, 0.60);
        assert_eq!(pipe.highlight_split, 0.80);

        let roundtrip = ParametricCurveValues::from_pipeline(&pipe);
        assert_eq!(roundtrip, p);
    }

    #[test]
    fn tone_curve_widget_point_mode_render() {
        let ctx = Context::default();
        let mut tab = ToneCurveTab::Point;
        let points = [(0.0, 0.0), (1.0, 1.0)];
        let mode = CurveMode::Smooth;
        let style = CurveStyle {
            curve_color: theme::ACCENT,
            point_color: theme::ACCENT_BRIGHT,
        };
        let parametric = ParametricCurveValues::default();

        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let edit = tone_curve_widget(
                    ui,
                    "test_tone_curve",
                    &mut tab,
                    &points,
                    mode,
                    &style,
                    &parametric,
                );
                assert!(edit.is_none());
            });
        });
        assert_eq!(tab, ToneCurveTab::Point);
    }

    #[test]
    fn tone_curve_widget_parametric_mode_render() {
        let ctx = Context::default();
        let mut tab = ToneCurveTab::Parametric;
        let points = [(0.0, 0.0), (1.0, 1.0)];
        let mode = CurveMode::Smooth;
        let style = CurveStyle {
            curve_color: theme::ACCENT,
            point_color: theme::ACCENT_BRIGHT,
        };
        let parametric = ParametricCurveValues::default();

        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let edit = tone_curve_widget(
                    ui,
                    "test_tone_curve",
                    &mut tab,
                    &points,
                    mode,
                    &style,
                    &parametric,
                );
                assert!(edit.is_none());
            });
        });
        assert_eq!(tab, ToneCurveTab::Parametric);
    }
}
