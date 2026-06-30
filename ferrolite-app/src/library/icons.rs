//! Painter-based icon helpers for the Library UI.
//!
//! All drawing uses `egui::Painter`/`egui::Shape` directly — no font glyphs —
//! so they render correctly with IBM Plex Sans (which lacks symbol glyphs such
//! as ★, ⚑, ▾).  Every function is pure geometry: no state, no allocation
//! beyond the shape list appended to the painter.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};

// ── colour / gradient helpers ────────────────────────────────────────────────

/// Fade `c` toward black: `t = 0.0` keeps `c`, `t = 1.0` → black.
pub fn scale_to_black(c: Color32, t: f32) -> Color32 {
    let f = (1.0 - t).clamp(0.0, 1.0);
    Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

/// Draw a soft selection border on `rect`'s edge: `color` at the band
/// centerline, fading to black at the inner and outer edges.  `width` is the
/// total band thickness.  Approximated with a few concentric rounded strokes
/// (egui has no gradient stroke).
pub fn gradient_border(
    painter: &egui::Painter,
    rect: Rect,
    rounding: f32,
    width: f32,
    color: Color32,
) {
    const STEPS: usize = 6;
    let step_w = width / STEPS as f32;
    for i in 0..STEPS {
        let t = (i as f32 + 0.5) / STEPS as f32; // 0..1 across the band
        let off = -width / 2.0 + t * width; // <0 outside the edge, >0 inside
        let d = ((t - 0.5).abs()) * 2.0; // 0 at centerline, 1 at edges
        let c = scale_to_black(color, d);
        let r = if off >= 0.0 {
            rect.shrink(off)
        } else {
            rect.expand(-off)
        };
        painter.rect_stroke(r, rounding, egui::Stroke::new(step_w + 0.6, c));
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Build the 10 vertices of a regular 5-pointed star centred at `center`.
/// `r_outer` is the circumradius; `r_inner` ≈ r_outer * 0.382 gives sharp tips.
fn star_points(center: Pos2, r_outer: f32, r_inner: f32) -> Vec<Pos2> {
    // Vertex 0 points straight up (−π/2).
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let angle = std::f32::consts::FRAC_PI_2 + (i as f32) * std::f32::consts::PI / 5.0;
        let r = if i % 2 == 0 { r_outer } else { r_inner };
        // subtract because egui y increases downward
        pts.push(center + Vec2::new(r * angle.cos(), -r * angle.sin()));
    }
    pts
}

// ── public API ───────────────────────────────────────────────────────────────

/// Draw a 5-pointed star centred at `center` with circumradius `r`.
///
/// `filled = true`  → filled polygon (solid star).
/// `filled = false` → stroked outline only.
pub fn star(painter: &Painter, center: Pos2, r: f32, filled: bool, color: Color32) {
    let r_inner = r * 0.382;
    let points = star_points(center, r, r_inner);
    if filled {
        painter.add(Shape::Path(egui::epaint::PathShape {
            points,
            closed: true,
            fill: color,
            stroke: egui::epaint::PathStroke::NONE,
        }));
    } else {
        painter.add(Shape::Path(egui::epaint::PathShape {
            points,
            closed: true,
            fill: Color32::TRANSPARENT,
            stroke: egui::epaint::PathStroke::new(1.0, color),
        }));
    }
}

/// Draw `total` stars in a row starting at the left-centre `origin`, spaced
/// `r * 2.0 + gap` apart.  The first `filled` stars are solid; the rest are
/// outlined.
///
/// Returns the total advance width (number of stars × cell width, less one gap
/// at the end) so callers can size their allocated rect.
///
/// Not yet called in the toolbar (individual star calls handle per-star click
/// targets), but called by the grid cell pass.
pub fn rating_stars(
    painter: &Painter,
    origin: Pos2,
    r: f32,
    gap: f32,
    filled: u8,
    total: u8,
    color: Color32,
) -> f32 {
    let cell = r * 2.0 + gap;
    for i in 0..total {
        let cx = origin.x + r + (i as f32) * cell;
        let cy = origin.y;
        star(painter, Pos2::new(cx, cy), r, i < filled, color);
    }
    advance_width(r, gap, total)
}

/// Pure-geometry helper: total advance width for `n` stars of radius `r`
/// with spacing `gap` between them (no trailing gap).
pub fn advance_width(r: f32, gap: f32, n: u8) -> f32 {
    if n == 0 {
        return 0.0;
    }
    (n as f32) * 2.0 * r + ((n as f32) - 1.0) * gap
}

/// Draw a small pennant flag whose pole bottom sits near `base`.
///
/// `h` is the total height of the icon (pole + pennant).
/// `filled = true` fills the pennant head; `false` draws an outline only.
pub fn flag(painter: &Painter, base: Pos2, h: f32, filled: bool, color: Color32) {
    let pole_x = base.x;
    // Pole: vertical line from base upward.
    let pole_top = Pos2::new(pole_x, base.y - h);
    painter.line_segment([base, pole_top], Stroke::new(1.2, color));

    // Pennant: a triangle to the right of the top half of the pole.
    let pennant_h = h * 0.55;
    let pennant_w = h * 0.55;
    let p0 = Pos2::new(pole_x, base.y - h);
    let p1 = Pos2::new(pole_x + pennant_w, base.y - h + pennant_h * 0.5);
    let p2 = Pos2::new(pole_x, base.y - h + pennant_h);
    if filled {
        painter.add(Shape::convex_polygon(vec![p0, p1, p2], color, Stroke::NONE));
    } else {
        painter.add(Shape::Path(egui::epaint::PathShape {
            points: vec![p0, p1, p2],
            closed: true,
            fill: Color32::TRANSPARENT,
            stroke: egui::epaint::PathStroke::new(1.0, color),
        }));
    }
}

/// Draw a small triangle caret centred at `center`.
///
/// `down = true`  → triangle points downward (▾).
/// `down = false` → triangle points upward  (▲).
/// `half_w` is half the base width of the triangle.
pub fn caret(painter: &Painter, center: Pos2, half_w: f32, color: Color32, down: bool) {
    let half_h = half_w * 0.65;
    let (tip, left, right) = if down {
        (
            Pos2::new(center.x, center.y + half_h),
            Pos2::new(center.x - half_w, center.y - half_h),
            Pos2::new(center.x + half_w, center.y - half_h),
        )
    } else {
        (
            Pos2::new(center.x, center.y - half_h),
            Pos2::new(center.x - half_w, center.y + half_h),
            Pos2::new(center.x + half_w, center.y + half_h),
        )
    };
    painter.add(Shape::convex_polygon(
        vec![tip, left, right],
        color,
        Stroke::NONE,
    ));
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── scale_to_black ────────────────────────────────────────────────────────

    #[test]
    fn scale_to_black_t0_preserves_color() {
        let c = Color32::from_rgb(100, 150, 200);
        let out = scale_to_black(c, 0.0);
        assert_eq!(out.r(), 100);
        assert_eq!(out.g(), 150);
        assert_eq!(out.b(), 200);
    }

    #[test]
    fn scale_to_black_t1_produces_black() {
        let c = Color32::from_rgb(100, 150, 200);
        let out = scale_to_black(c, 1.0);
        assert_eq!(out.r(), 0);
        assert_eq!(out.g(), 0);
        assert_eq!(out.b(), 0);
    }

    #[test]
    fn scale_to_black_t_half_roughly_halves_channels() {
        let c = Color32::from_rgb(200, 100, 50);
        let out = scale_to_black(c, 0.5);
        // f = 0.5; integer truncation means we expect floor(channel * 0.5)
        assert_eq!(out.r(), 100); // 200 * 0.5 = 100
        assert_eq!(out.g(), 50); //  100 * 0.5 = 50
        assert_eq!(out.b(), 25); //   50 * 0.5 = 25
    }

    // ── advance_width ─────────────────────────────────────────────────────────

    #[test]
    fn advance_width_zero_stars_is_zero() {
        assert_eq!(advance_width(6.0, 2.0, 0), 0.0);
    }

    #[test]
    fn advance_width_one_star_equals_diameter() {
        // 1 star: 2*r + 0 gaps
        let r = 6.0_f32;
        let w = advance_width(r, 2.0, 1);
        assert!((w - 12.0).abs() < 1e-4, "expected 12.0, got {w}");
    }

    #[test]
    fn advance_width_five_stars_correct() {
        // 5 stars × 12px diameter + 4 gaps × 2px
        let r = 6.0_f32;
        let gap = 2.0_f32;
        let expected = 5.0 * 12.0 + 4.0 * gap; // 68.0
        let w = advance_width(r, gap, 5);
        assert!((w - expected).abs() < 1e-4, "expected {expected}, got {w}");
    }

    #[test]
    fn star_points_has_ten_vertices() {
        let pts = star_points(Pos2::new(0.0, 0.0), 6.0, 2.3);
        assert_eq!(pts.len(), 10);
    }

    #[test]
    fn star_points_outer_radius_approx() {
        let r = 6.0_f32;
        let center = Pos2::new(0.0, 0.0);
        let pts = star_points(center, r, r * 0.382);
        // Even-indexed points should be at distance r from center.
        for i in (0..10).step_by(2) {
            let d = (pts[i] - center).length();
            assert!(
                (d - r).abs() < 1e-3,
                "outer vertex {i}: dist={d}, expected {r}"
            );
        }
    }
}
