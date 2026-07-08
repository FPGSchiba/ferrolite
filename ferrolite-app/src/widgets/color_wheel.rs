//! Reusable hue-sat color wheel for color grading. Hue = angle (0° at +x,
//! increasing counter-clockwise on screen), sat = radius (0 at center = neutral,
//! 1 at the rim). Drawn as a plain egui `Mesh` (grey center → full-sat rim), with
//! a draggable thumb and its own per-control reset (→ neutral sat 0). All memory
//! is salted with `id_source` so multiple wheels coexist (design §4.3).

use crate::theme;
use crate::widgets::draw_reset_arrow;
use egui::{pos2, vec2, Color32, Mesh, Pos2, Sense, Shape, Stroke};

const RADIUS: f32 = 46.0;
const SEGMENTS: usize = 48;
const RESET_R: f32 = 4.5;

/// A change emitted by `color_wheel`. `commit` false = live drag preview.
pub struct WheelEdit {
    pub hue: f32,
    pub sat: f32,
    pub commit: bool,
}

/// Screen position of the thumb for a given (hue, sat). Screen y is down, so the
/// angle's sine is negated to make hue increase counter-clockwise visually.
fn wheel_pos(center: Pos2, radius: f32, hue: f32, sat: f32) -> Pos2 {
    let a = hue.to_radians();
    center + radius * sat.clamp(0.0, 1.0) * vec2(a.cos(), -a.sin())
}

/// (hue, sat) for a pointer position: sat = distance/radius clamped [0,1], hue =
/// screen angle (y-down inverted) in [0,360).
fn wheel_from_pos(center: Pos2, radius: f32, p: Pos2) -> (f32, f32) {
    let d = p - center;
    let sat = (d.length() / radius).clamp(0.0, 1.0);
    let mut hue = (-d.y).atan2(d.x).to_degrees();
    if hue < 0.0 {
        hue += 360.0;
    }
    (hue, sat)
}

/// HSV → egui Color32 (h in degrees, s/v in [0,1]) for the disc mesh (UI only).
fn hsv_color(h_deg: f32, s: f32, v: f32) -> Color32 {
    let h = h_deg.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h.floor() as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color32::from_rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

pub fn color_wheel(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    hue: f32,
    sat: f32,
) -> Option<WheelEdit> {
    let size = RADIUS * 2.0 + 4.0;
    // Extra height below the disc for the reset affordance.
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size + 18.0), Sense::click_and_drag());
    let base_id = ui.id().with(id_source);
    let center = pos2(rect.center().x, rect.top() + RADIUS + 2.0);

    // Hue-sat disc as a triangle fan: grey center + full-sat rim.
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, Color32::from_gray(0x80));
    for i in 0..=SEGMENTS {
        let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let deg = a.to_degrees();
        mesh.colored_vertex(
            center + RADIUS * vec2(a.cos(), -a.sin()),
            hsv_color(deg, 1.0, 1.0),
        );
    }
    for i in 1..=SEGMENTS as u32 {
        mesh.add_triangle(0, i, i + 1);
    }
    let painter = ui.painter();
    painter.add(Shape::mesh(mesh));
    painter.circle_stroke(center, RADIUS, Stroke::new(1.0, theme::BORDER_STRONG));

    // Thumb at the current (hue, sat).
    let thumb = wheel_pos(center, RADIUS, hue, sat);
    painter.circle(thumb, 5.0, Color32::WHITE, Stroke::new(1.5, Color32::BLACK));

    // Interaction: drag/click sets hue+sat; release commits.
    let mut result: Option<WheelEdit> = None;
    if let Some(p) = resp.interact_pointer_pos() {
        if resp.dragged() {
            let (h, s) = wheel_from_pos(center, RADIUS, p);
            result = Some(WheelEdit {
                hue: h,
                sat: s,
                commit: false,
            });
        } else if resp.clicked() {
            let (h, s) = wheel_from_pos(center, RADIUS, p);
            result = Some(WheelEdit {
                hue: h,
                sat: s,
                commit: true,
            });
        }
    }
    if resp.drag_stopped() {
        // Commit the caller's current (already-applied) value on release.
        result = Some(WheelEdit {
            hue,
            sat,
            commit: true,
        });
    }

    // Per-control reset (→ neutral sat 0), dim when already neutral.
    let reset_rect =
        egui::Rect::from_center_size(pos2(center.x, rect.bottom() - 8.0), vec2(16.0, 16.0));
    let reset_resp = ui.interact(reset_rect, base_id.with("wheel_reset"), Sense::click());
    let modified = sat > 0.0;
    let reset_color = if modified {
        if reset_resp.hovered() {
            theme::ACCENT_BRIGHT
        } else {
            theme::TEXT_FAINT
        }
    } else {
        theme::BORDER_STRONG
    };
    draw_reset_arrow(ui.painter(), reset_rect.center(), RESET_R, reset_color);
    if reset_resp.clicked() && modified {
        result = Some(WheelEdit {
            hue,
            sat: 0.0,
            commit: true,
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn center_maps_to_zero_sat() {
        let c = pos2(50.0, 50.0);
        let (_hue, sat) = wheel_from_pos(c, 40.0, c);
        assert!(sat.abs() < 1e-6, "pointer at center = sat 0");
    }

    #[test]
    fn edge_maps_to_full_sat() {
        let c = pos2(50.0, 50.0);
        let edge = pos2(50.0 + 40.0, 50.0); // one radius to the right
        let (_hue, sat) = wheel_from_pos(c, 40.0, edge);
        assert!((sat - 1.0).abs() < 1e-6, "pointer at the rim = sat 1");
    }

    #[test]
    fn beyond_edge_clamps_sat_to_one() {
        let c = pos2(50.0, 50.0);
        let far = pos2(50.0 + 80.0, 50.0); // two radii out
        let (_h, sat) = wheel_from_pos(c, 40.0, far);
        assert!((sat - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pos_and_from_pos_roundtrip() {
        let c = pos2(50.0, 50.0);
        let r = 40.0;
        for &(hue, sat) in &[(0.0, 0.5), (90.0, 1.0), (210.0, 0.3), (330.0, 0.8)] {
            let p = wheel_pos(c, r, hue, sat);
            let (h2, s2) = wheel_from_pos(c, r, p);
            assert!((s2 - sat).abs() < 1e-4, "sat roundtrip {sat} -> {s2}");
            let dh = ((h2 - hue + 180.0).rem_euclid(360.0)) - 180.0;
            assert!(dh.abs() < 1e-3, "hue roundtrip {hue} -> {h2}");
        }
    }
}
