//! Ferrolite app icon (Concept A "faceted F"). One geometry, two renderings:
//! `paint_mark` for the egui title-bar mark, `icon_rgba` for the OS IconData.
//! All geometry is in a 64x64 design space (see assets/icon/ferrolite.svg).

use crate::theme;
use egui::{Color32, Painter, Rect, Rounding};

// F is a union of axis-aligned rectangles in 64-space: (x0, y0, x1, y1).
const STEM: [f32; 4] = [20.0, 16.0, 29.0, 49.0];
const TOP_ARM: [f32; 4] = [29.0, 16.0, 46.0, 25.0];
const MID_ARM: [f32; 4] = [29.0, 31.0, 42.0, 40.0];
// Bright facet over the top band of the F (x 20..46, y 16..25), drawn semi-transparent.
const FACET: [f32; 4] = [20.0, 16.0, 46.0, 25.0];
const FACET_ALPHA: u8 = 153; // 0.6 * 255

// OS-icon tile (icon only; the title-bar mark is transparent).
const TILE_BG: Color32 = Color32::from_rgb(0x16, 0x1a, 0x1f);
const TILE_RECT: [f32; 4] = [2.0, 2.0, 62.0, 62.0];
const TILE_RADIUS: f32 = 13.0;

fn in_rect(x: f32, y: f32, r: &[f32; 4]) -> bool {
    x >= r[0] && x < r[2] && y >= r[1] && y < r[3]
}

/// Coverage of the rounded tile at design-space point (x,y): 1.0 inside, 0.0 outside.
fn tile_covered(x: f32, y: f32) -> bool {
    let [x0, y0, x1, y1] = TILE_RECT;
    if x < x0 || x >= x1 || y < y0 || y >= y1 {
        return false;
    }
    // Round the four corners.
    let r = TILE_RADIUS;
    let cx = if x < x0 + r {
        x0 + r
    } else if x > x1 - r {
        x1 - r
    } else {
        x
    };
    let cy = if y < y0 + r {
        y0 + r
    } else if y > y1 - r {
        y1 - r
    } else {
        y
    };
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

/// Returns the RGBA at design-space point (x,y), or None if fully transparent.
fn sample(x: f32, y: f32) -> Option<[u8; 4]> {
    if !tile_covered(x, y) {
        return None;
    }
    let mut col = TILE_BG.to_array(); // [r,g,b,a]
    if in_rect(x, y, &STEM) || in_rect(x, y, &TOP_ARM) || in_rect(x, y, &MID_ARM) {
        col = theme::ACCENT.to_array();
    }
    if in_rect(x, y, &FACET) {
        // Blend ACCENT_BRIGHT (alpha 0.6) over current color.
        let b = theme::ACCENT_BRIGHT.to_array();
        let a = FACET_ALPHA as u32;
        for i in 0..3 {
            col[i] = ((b[i] as u32 * a + col[i] as u32 * (255 - a)) / 255) as u8;
        }
    }
    col[3] = 255;
    Some(col)
}

/// `px*px*4` RGBA8 of the F-on-tile, 2x2 supersampled for smooth tile edges.
pub fn icon_rgba(px: u32) -> Vec<u8> {
    let scale = px as f32 / 64.0;
    let mut buf = vec![0u8; (px * px * 4) as usize];
    for y in 0..px {
        for x in 0..px {
            let mut acc = [0u32; 4];
            for sy in 0..2 {
                for sx in 0..2 {
                    let fx = (x as f32 + 0.25 + 0.5 * sx as f32) / scale;
                    let fy = (y as f32 + 0.25 + 0.5 * sy as f32) / scale;
                    if let Some(c) = sample(fx, fy) {
                        for i in 0..4 {
                            acc[i] += c[i] as u32;
                        }
                    }
                }
            }
            let i = ((y * px + x) * 4) as usize;
            for k in 0..4 {
                buf[i + k] = (acc[k] / 4) as u8;
            }
        }
    }
    buf
}

fn scaled(r: &[f32; 4], origin: egui::Pos2, s: f32) -> Rect {
    Rect::from_min_max(
        egui::pos2(origin.x + r[0] * s, origin.y + r[1] * s),
        egui::pos2(origin.x + r[2] * s, origin.y + r[3] * s),
    )
}

/// Paint the faceted-F mark (no tile, transparent bg) fitted into `rect`.
pub fn paint_mark(painter: &Painter, rect: Rect) {
    // The F occupies design-space x 20..46, y 16..49 (26 x 33). Fit it into rect.
    let s = (rect.width() / 26.0).min(rect.height() / 33.0);
    // origin so that design point (20,16) maps near rect.min, vertically centered.
    let origin = egui::pos2(rect.left() - 20.0 * s, rect.center().y - 32.5 * s); // 32.5 = 16 (F top) + 33/2 (half the F's design-space height) — centers the F's midpoint on rect.center().y
    for r in [&STEM, &TOP_ARM, &MID_ARM] {
        painter.rect_filled(scaled(r, origin, s), Rounding::ZERO, theme::ACCENT);
    }
    let facet = Color32::from_rgba_unmultiplied(
        theme::ACCENT_BRIGHT.r(),
        theme::ACCENT_BRIGHT.g(),
        theme::ACCENT_BRIGHT.b(),
        FACET_ALPHA,
    );
    painter.rect_filled(scaled(&FACET, origin, s), Rounding::ZERO, facet);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px_at(buf: &[u8], px: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * px + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn rgba_buffer_has_correct_length() {
        assert_eq!(icon_rgba(64).len(), 64 * 64 * 4);
        assert_eq!(icon_rgba(32).len(), 32 * 32 * 4);
    }

    #[test]
    fn f_stem_pixel_is_accent() {
        // 64-space (24,35) is inside the stem; at px=64 that's pixel (24,35).
        let buf = icon_rgba(64);
        let [r, g, b, a] = px_at(&buf, 64, 24, 35);
        assert_eq!([r, g, b], [0x6d, 0x97, 0xb5]);
        assert_eq!(a, 255);
    }

    #[test]
    fn facet_band_pixel_is_brighter_than_accent() {
        // 64-space (24,20): stem within the bright facet band -> blended brighter.
        let buf = icon_rgba(64);
        let [_, _, b, a] = px_at(&buf, 64, 24, 20);
        assert!(b > 0xb5, "facet blue {b} should exceed accent blue 0xb5");
        assert_eq!(a, 255);
    }

    #[test]
    fn tile_interior_outside_f_is_tile_color() {
        // 64-space (40,45): inside tile, outside the F.
        let buf = icon_rgba(64);
        let [r, g, b, a] = px_at(&buf, 64, 40, 45);
        assert_eq!([r, g, b], [0x16, 0x1a, 0x1f]);
        assert_eq!(a, 255);
    }

    #[test]
    fn rounded_corner_is_transparent() {
        // 64-space (4,4): outside the rounded tile corner -> fully transparent.
        let buf = icon_rgba(64);
        let [_, _, _, a] = px_at(&buf, 64, 4, 4);
        assert_eq!(a, 0);
    }
}
