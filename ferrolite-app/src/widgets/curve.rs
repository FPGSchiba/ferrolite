//! Reusable interactive curve editor widget. Pure point math lives in
//! `crate::develop::curve_math`; this layer paints + routes pointer events and
//! is generic over the caller's `id_source` so multiple curve editors can
//! coexist on one screen (e.g. tone curve + future per-channel color curves).
//!
//! This module is the reusable widget only (Spec 4.1 CD2 Task 4/5 of the
//! curve-spline-modes plan). Nothing calls `curve_editor` yet — the tone-curve
//! adapter rewrite (Task 6) wires it in. Hence the module-wide `dead_code`
//! allow until that call site lands.
#![allow(dead_code)]

use crate::develop::curve_math::{self, GrabOrInsert};
use crate::theme;
use ferrolite_pipeline::{curve_lut, CurveMode};

const SIZE: f32 = 260.0; // square edit area
const HIT_R: f32 = 0.06; // normalized hit radius
const DOT_R: f32 = 5.0; // idle point-dot radius
const DOT_R_HOVER: f32 = 6.5; // enlarged radius for the hovered point

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

/// Paint + interact with a curve editor bound to `points`/`mode`. All memory
/// keys are salted with `id_source` so two instances on one screen don't
/// collide. Returns `Some(CurveEdit)` on any change (drag, insert, delete, or
/// reset), `None` otherwise.
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
            egui::Stroke::new(1.0, theme::BORDER_STRONG),
        );
        painter.line_segment(
            [
                egui::pos2(rect.left(), rect.top() + f * SIZE),
                egui::pos2(rect.right(), rect.top() + f * SIZE),
            ],
            egui::Stroke::new(1.0, theme::BORDER_STRONG),
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
        egui::Stroke::new(1.5, style.curve_color),
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
            egui::Stroke::new(1.0, theme::BG_BASE),
        );
        if is_selected {
            // Accent ring around the selected point so selection reads clearly
            // and independently of hover state.
            painter.circle_stroke(
                to_screen(p),
                radius + 3.0,
                egui::Stroke::new(1.5, style.curve_color),
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

    // Per-component reset affordance, styled like the Basic section's "Reset"
    // (see CLAUDE.md "Per-component reset" rule). Dim/disabled at default.
    let modified = !curve_math::is_identity(&points);
    if ui
        .add_enabled(modified, egui::Button::new("Reset").small())
        .clicked()
    {
        // Resetting clears any stale selection tied to the old point list.
        ui.memory_mut(|m| m.data.insert_temp::<Option<usize>>(selected_id, None));
        return Some(CurveEdit {
            points: curve_math::identity_points(),
            mode,
            reset: true,
            commit: true,
        });
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
