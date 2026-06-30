//! Interactive tone-curve widget. Pure point math in `curve_math`; this layer
//! only paints + routes pointer events. Visual-tested (no unit tests).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::curve_math;
use crate::theme;
use ferrolite_pipeline::{Op, OpKind, OpStack, ToneCurve};

const SIZE: f32 = 260.0; // square edit area
const HIT_R: f32 = 0.04; // normalized hit radius

pub fn show(ui: &mut egui::Ui, stack: &OpStack) -> Option<EditOutcome> {
    let mut points = stack
        .tone_curve()
        .map(|t| t.points)
        .filter(|p| !p.is_empty())
        .unwrap_or_else(curve_math::identity_points);

    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::click_and_drag());
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
    let to_screen = |p: (f32, f32)| egui::pos2(rect.left() + p.0 * SIZE, rect.bottom() - p.1 * SIZE);
    let to_norm = |s: egui::Pos2| ((s.x - rect.left()) / SIZE, (rect.bottom() - s.y) / SIZE);

    // Curve polyline.
    let poly: Vec<egui::Pos2> = points.iter().map(|&p| to_screen(p)).collect();
    painter.add(egui::Shape::line(poly, egui::Stroke::new(1.5, theme::ACCENT)));
    for &p in &points {
        painter.circle(
            to_screen(p),
            3.5,
            theme::ACCENT_BRIGHT,
            egui::Stroke::new(1.0, theme::BG_BASE),
        );
    }

    let mut changed = false;
    let mut commit = false;
    if let Some(pos) = resp.interact_pointer_pos() {
        let norm = to_norm(pos);
        if resp.drag_started() || resp.clicked() {
            // Grab the nearest existing point, else insert a new one.
            match curve_math::nearest_point(&points, norm, HIT_R) {
                Some(idx) => ui.memory_mut(|m| m.data.insert_temp(resp.id, idx)),
                None => {
                    points = curve_math::insert_point(&points, norm);
                    let idx =
                        curve_math::nearest_point(&points, norm, HIT_R).unwrap_or(0);
                    ui.memory_mut(|m| m.data.insert_temp(resp.id, idx));
                    changed = true;
                }
            }
        }
        if resp.dragged() {
            if let Some(idx) = ui.memory(|m| m.data.get_temp::<usize>(resp.id)) {
                points = curve_math::move_point(&points, idx, norm);
                changed = true;
            }
        }
    }
    if resp.drag_stopped() {
        commit = true;
    }
    // Right-click a point to delete it.
    if resp.secondary_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(idx) = curve_math::nearest_point(&points, to_norm(pos), HIT_R) {
                points = curve_math::delete_point(&points, idx);
                changed = true;
                commit = true;
            }
        }
    }

    if changed {
        let s = if curve_math::is_identity(&points) {
            stack.reset(OpKind::ToneCurve)
        } else {
            stack.set_op(Op::ToneCurve(ToneCurve { points }))
        };
        return Some(EditOutcome { stack: s, kind: OpKind::ToneCurve, commit });
    }
    None
}
