//! Interactive tone-curve widget. Pure point math in `curve_math`; this layer
//! only paints + routes pointer events. Visual-tested (no unit tests).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::curve_math;
use crate::theme;
use crate::widgets::draw_reset_arrow;
use ferrolite_pipeline::{Op, OpKind, OpStack, ToneCurve};

const SIZE: f32 = 260.0; // square edit area
const HIT_R: f32 = 0.04; // normalized hit radius
const RESET_SIZE: f32 = 16.0; // reset hit-target square size
const RESET_ICON_R: f32 = 4.5; // reset icon radius, matches EguiSlider's
                               // Mirrors `EguiSlider`'s HANDLE_IDLE token (not in the shared `theme` module).
const HANDLE_IDLE: egui::Color32 = egui::Color32::from_rgb(0x9a, 0x9a, 0x9a);

pub fn show(ui: &mut egui::Ui, stack: &OpStack) -> Option<EditOutcome> {
    let mut points = stack
        .tone_curve()
        .map(|t| t.points)
        .filter(|p| !p.is_empty())
        .unwrap_or_else(curve_math::identity_points);

    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::click_and_drag());

    // Per-component reset affordance (top-right corner), consistent with the
    // slider's reset icon. See CLAUDE.md "Per-component reset" rule.
    let reset_rect = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 18.0, rect.top() + 2.0),
        egui::pos2(rect.right() - 2.0, rect.top() + 2.0 + RESET_SIZE),
    );
    let modified = !curve_math::is_identity(&points);
    let reset_resp = ui.interact(
        reset_rect,
        ui.id().with("tone_curve_reset"),
        egui::Sense::click(),
    );
    if reset_resp.clicked() && modified {
        return Some(EditOutcome {
            stack: stack.reset(OpKind::ToneCurve),
            kind: OpKind::ToneCurve,
            commit: true,
        });
    }

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

    // Curve polyline.
    let poly: Vec<egui::Pos2> = points.iter().map(|&p| to_screen(p)).collect();
    painter.add(egui::Shape::line(
        poly,
        egui::Stroke::new(1.5, theme::ACCENT),
    ));
    for &p in &points {
        painter.circle(
            to_screen(p),
            3.5,
            theme::ACCENT_BRIGHT,
            egui::Stroke::new(1.0, theme::BG_BASE),
        );
    }

    // Reset icon: dim when already at default; matches the slider's scheme.
    let reset_color = if modified {
        if reset_resp.hovered() {
            theme::ACCENT_BRIGHT
        } else {
            HANDLE_IDLE
        }
    } else {
        theme::BORDER_STRONG
    };
    draw_reset_arrow(painter, reset_rect.center(), RESET_ICON_R, reset_color);

    let mut changed = false;
    let mut commit = false;
    if let Some(pos) = resp
        .interact_pointer_pos()
        .filter(|p| !reset_rect.contains(*p))
    {
        let norm = to_norm(pos);
        if resp.drag_started() || resp.clicked() {
            // Grab the nearest existing point, else insert a new one.
            match curve_math::nearest_point(&points, norm, HIT_R) {
                Some(idx) => ui.memory_mut(|m| m.data.insert_temp(resp.id, idx)),
                None => {
                    // Insert at the clamped coordinate, then grab THAT point by its
                    // exact (bit-identical) value — nearest_point can resolve to a
                    // neighbor on a crowded curve.
                    let inserted = (norm.0.clamp(0.0, 1.0), norm.1.clamp(0.0, 1.0));
                    points = curve_math::insert_point(&points, norm);
                    let idx = points.iter().position(|&q| q == inserted).unwrap_or(0);
                    ui.memory_mut(|m| m.data.insert_temp(resp.id, idx));
                    changed = true;
                    commit = true;
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
        return Some(EditOutcome {
            stack: s,
            kind: OpKind::ToneCurve,
            commit,
        });
    }
    None
}
