//! Icon-font-based icon helpers for the Library UI.
//!
//! Ratings/flags/carets render glyphs from the bundled Phosphor icon font (see
//! `crate::icons`) via `Painter::text`, rather than hand-built `egui::Shape`
//! polygons — this is the single icon system for the app (root `CLAUDE.md`).
//! Helpers that draw non-Phosphor glyphs (cross/check/export tray/queued
//! badge/split-compare) are unaffected and remain pure geometry.

use crate::icons;
use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};

// ── public API ───────────────────────────────────────────────────────────────

/// Draw a star glyph centred at `center` sized so its visual extent matches
/// the former circumradius-`r` polygon.
///
/// `filled = true`  → `icons::STAR_FILL` (solid star).
/// `filled = false` → `icons::STAR` (outline star).
pub fn star(painter: &Painter, center: Pos2, r: f32, filled: bool, color: Color32) {
    // The old vector star's circumradius `r` corresponds to a glyph font size
    // of about `r * 2.2` (matches the reset-arrow glyph's scale factor for the
    // same "geometric radius -> font size" conversion).
    let size = r * 2.2;
    if filled {
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            icons::STAR_FILL,
            icons::font_fill(size),
            color,
        );
    } else {
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            icons::STAR,
            icons::font(size),
            color,
        );
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
#[allow(clippy::too_many_arguments)]
pub fn rating_stars(
    painter: &Painter,
    origin: Pos2,
    r: f32,
    gap: f32,
    filled: u8,
    total: u8,
    color: Color32,
    bg: bool,
) -> f32 {
    if bg && total > 0 {
        let pad = 3.0;
        let w = advance_width(r, gap, total);
        let bg_rect = Rect::from_min_max(
            Pos2::new(origin.x - pad, origin.y - r - pad),
            Pos2::new(origin.x + w + pad, origin.y + r + pad),
        );
        painter.rect_filled(bg_rect, 3.0, Color32::from_black_alpha(120));
    }
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

/// Draw a flag glyph anchored so its footprint matches the former hand-drawn
/// pennant's bounding box (pole bottom at `base`, extending up/right by `h`).
///
/// `filled = true` → `icons::FLAG_FILL` (solid pick flag) unless `reject`, in
/// which case reject always renders `icons::FLAG_REJECT` (a prohibit glyph
/// has no separate filled variant in Phosphor, so `filled` only toggles
/// weight for the pick glyph). `filled = false` → `icons::FLAG` outline.
///
/// `reject = true` selects the reject glyph (`icons::FLAG_REJECT`) instead of
/// the pick flag glyph — this parameter was added during the icon-font
/// migration since the old geometry drew the same pennant shape for both
/// pick/reject and relied purely on `color` to distinguish them.
pub fn flag(
    painter: &Painter,
    base: Pos2,
    h: f32,
    filled: bool,
    color: Color32,
    bg: bool,
    reject: bool,
) {
    // Old geometry's bounding box was roughly h tall x (h*0.55) wide, so the
    // glyph anchor sits at the vertical/horizontal center of that box.
    let center = Pos2::new(base.x + h * 0.275, base.y - h * 0.5);
    if bg {
        let pad = 2.5;
        let bg_rect = Rect::from_min_max(
            Pos2::new(base.x - pad, base.y - h - pad),
            Pos2::new(base.x + h * 0.55 + pad, base.y + pad),
        );
        painter.rect_filled(bg_rect, 3.0, Color32::from_black_alpha(120));
    }
    let size = h * 2.2;
    if reject {
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            icons::FLAG_REJECT,
            icons::font(size),
            color,
        );
    } else if filled {
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            icons::FLAG_FILL,
            icons::font_fill(size),
            color,
        );
    } else {
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            icons::FLAG,
            icons::font(size),
            color,
        );
    }
}

/// Draw a caret glyph centred at `center`.
///
/// `down = true`  → `icons::CARET_DOWN` (▾).
/// `down = false` → `icons::CARET_UP` (▲).
/// `half_w` is half the base width the old triangle used; the glyph is sized
/// to match that visual footprint.
pub fn caret(painter: &Painter, center: Pos2, half_w: f32, color: Color32, down: bool) {
    // Old triangle's base width was `half_w * 2`; apply the same
    // "geometric size -> font size" factor used by star/flag/reset.
    let size = (half_w * 2.0) * 2.2;
    let glyph = if down {
        icons::CARET_DOWN
    } else {
        icons::CARET_UP
    };
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        glyph,
        icons::font(size),
        color,
    );
}

/// Draw a small "✕" (remove/close) icon as two crossing diagonal strokes
/// centred at `center` with half-extent `r`.
pub fn cross(painter: &Painter, center: Pos2, r: f32, color: Color32) {
    let stroke = Stroke::new(1.4, color);
    painter.line_segment(
        [
            Pos2::new(center.x - r, center.y - r),
            Pos2::new(center.x + r, center.y + r),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(center.x - r, center.y + r),
            Pos2::new(center.x + r, center.y - r),
        ],
        stroke,
    );
}

/// Draw a checkmark ("✓", done) centred at `center` with half-extent `r`, as a
/// short down-stroke into a longer up-stroke. No font glyph.
pub fn check(painter: &Painter, center: Pos2, r: f32, color: Color32) {
    let stroke = Stroke::new(1.6, color);
    // elbow low-left, tip up-right — a conventional tick.
    let left = Pos2::new(center.x - r, center.y);
    let elbow = Pos2::new(center.x - r * 0.25, center.y + r * 0.7);
    let tip = Pos2::new(center.x + r, center.y - r * 0.7);
    painter.line_segment([left, elbow], stroke);
    painter.line_segment([elbow, tip], stroke);
}

/// Draw an "export to queue" glyph: a downward arrow landing into an open
/// tray, centred at `center`. `size` is the overall icon height/width.
///
/// Reads universally as "send/add to queue" without relying on font glyphs.
pub fn export_tray(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let stroke = Stroke::new(1.3, color);
    let half = size * 0.5;

    // Downward arrow shaft, from just below the top to mid-height.
    let shaft_top = Pos2::new(center.x, center.y - half);
    let shaft_bottom = Pos2::new(center.x, center.y + half * 0.15);
    painter.line_segment([shaft_top, shaft_bottom], stroke);

    // Arrowhead at the shaft's bottom tip.
    let head_w = size * 0.28;
    let head_h = size * 0.28;
    let head_left = Pos2::new(shaft_bottom.x - head_w, shaft_bottom.y - head_h);
    let head_right = Pos2::new(shaft_bottom.x + head_w, shaft_bottom.y - head_h);
    painter.line_segment([shaft_bottom, head_left], stroke);
    painter.line_segment([shaft_bottom, head_right], stroke);

    // Tray: bottom line + two short upturned sides, sitting under the arrow.
    let tray_y = center.y + half * 0.55;
    let tray_half_w = half * 0.85;
    let side_h = size * 0.22;
    let tray_left_bottom = Pos2::new(center.x - tray_half_w, tray_y);
    let tray_right_bottom = Pos2::new(center.x + tray_half_w, tray_y);
    let tray_left_top = Pos2::new(center.x - tray_half_w, tray_y - side_h);
    let tray_right_top = Pos2::new(center.x + tray_half_w, tray_y - side_h);
    painter.line_segment([tray_left_top, tray_left_bottom], stroke);
    painter.line_segment([tray_left_bottom, tray_right_bottom], stroke);
    painter.line_segment([tray_right_bottom, tray_right_top], stroke);
}

/// Draw a small "in export queue" badge: an accent-filled rounded square with
/// a "Q" glyph, anchored by its top-right corner at `top_right`.
///
/// Used by the Library grid and the Develop filmstrip to mark thumbnails that
/// are currently queued for export. `size` is the badge's edge length.
pub fn queued_badge(painter: &Painter, top_right: Pos2, size: f32, fg: Color32, bg: Color32) {
    let rect = Rect::from_min_size(
        Pos2::new(top_right.x - size, top_right.y),
        Vec2::splat(size),
    );
    painter.rect_filled(rect, size * 0.25, bg);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "Q",
        egui::FontId::proportional(size * 0.65),
        fg,
    );
}

/// Draw a before/after "split-compare" glyph: a rounded-rect outline with a
/// vertical center divider, the left half subtly filled to read as "before" —
/// pure geometry, no font glyph (IBM Plex Sans has no `⇔` glyph).
///
/// `size` is the overall icon edge length (square, centred at `center`).
pub fn split_compare(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let rect = Rect::from_center_size(center, Vec2::splat(size));
    let rounding = size * 0.18;

    // Left half: a dim fill so it reads as the "before" side of the split. Only
    // the outer (left) edge is rounded — the divider-side edge stays square so
    // the fill reads as a clean vertical cut, not a separately rounded chip.
    let left = Rect::from_min_max(rect.min, Pos2::new(center.x, rect.max.y));
    painter.rect_filled(
        left,
        egui::Rounding {
            nw: rounding,
            sw: rounding,
            ne: 0.0,
            se: 0.0,
        },
        Color32::from_black_alpha(70),
    );

    // Outline.
    painter.rect_stroke(rect, rounding, Stroke::new(1.3, color));

    // Center divider.
    painter.line_segment(
        [
            Pos2::new(center.x, rect.min.y),
            Pos2::new(center.x, rect.max.y),
        ],
        Stroke::new(1.3, color),
    );
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
}
