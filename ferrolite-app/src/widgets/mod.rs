pub mod slider;
pub use slider::EguiSlider;

use egui::{vec2, Color32, Stroke};

/// Draw a small circular-arrow "reset" glyph centered at `center` with radius
/// `r`: a ~280° stroked arc plus a tiny two-segment arrowhead at its open end.
/// Pure `egui::Painter` geometry (no font glyph), matching the `icons` module's
/// convention of sampling points along curves into a `Shape::line`.
///
/// Shared by `EguiSlider` and any other editable control that needs a
/// per-control reset affordance (see the design rule in the root `CLAUDE.md`).
pub(crate) fn draw_reset_arrow(
    painter: &egui::Painter,
    center: egui::Pos2,
    r: f32,
    color: Color32,
) {
    use std::f32::consts::PI;

    // Arc spans 280°, leaving a gap at the top-right for the arrowhead.
    let start_angle = -PI / 2.0 - 0.35; // just past top, going clockwise
    let sweep = 2.0 * PI * (280.0 / 360.0);
    let steps = 16;
    let arc_points: Vec<egui::Pos2> = (0..=steps)
        .map(|i| {
            let t = start_angle + sweep * (i as f32 / steps as f32);
            center + vec2(r * t.cos(), r * t.sin())
        })
        .collect();
    painter.add(egui::Shape::line(
        arc_points.clone(),
        Stroke::new(1.2, color),
    ));

    // Arrowhead at the arc's end, pointing in the direction of travel.
    if let Some(&tip) = arc_points.last() {
        let end_angle = start_angle + sweep;
        let tangent = vec2(-end_angle.sin(), end_angle.cos()); // direction of travel
        let normal = vec2(tangent.y, -tangent.x);
        let head_len = r * 0.9;
        let back = tip - tangent * head_len;
        let p1 = back + normal * head_len * 0.6;
        let p2 = back - normal * head_len * 0.6;
        painter.add(egui::Shape::line_segment(
            [tip, p1],
            Stroke::new(1.2, color),
        ));
        painter.add(egui::Shape::line_segment(
            [tip, p2],
            Stroke::new(1.2, color),
        ));
    }
}
