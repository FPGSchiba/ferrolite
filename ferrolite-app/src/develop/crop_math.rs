//! Pure crop-overlay geometry in image-normalized [0,1] space. egui-free; the
//! overlay widget converts screen↔image coords and routes pointer events here.

use ferrolite_pipeline::{Aspect, CropRect};

/// `#[repr(u8)]` makes `handle as u8` well-defined and lets crop_overlay index
/// into HANDLES with a simple cast (`HANDLES[h as usize]`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    Body,
}

const MIN_SIZE: f32 = 0.02;

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

fn near(a: (f32, f32), b: (f32, f32), r: f32) -> bool {
    (a.0 - b.0).abs() <= r && (a.1 - b.1).abs() <= r
}

pub fn hit_test(c: CropRect, pos: (f32, f32), r: f32) -> Option<Handle> {
    let (l, t, rt, b) = (c.x, c.y, c.x + c.w, c.y + c.h);
    let (mx, my) = (c.x + c.w * 0.5, c.y + c.h * 0.5);
    let candidates = [
        (Handle::TopLeft, (l, t)),
        (Handle::TopRight, (rt, t)),
        (Handle::BottomRight, (rt, b)),
        (Handle::BottomLeft, (l, b)),
        (Handle::Top, (mx, t)),
        (Handle::Bottom, (mx, b)),
        (Handle::Left, (l, my)),
        (Handle::Right, (rt, my)),
    ];
    for (h, p) in candidates {
        if near(pos, p, r) {
            return Some(h);
        }
    }
    if pos.0 >= l && pos.0 <= rt && pos.1 >= t && pos.1 <= b {
        return Some(Handle::Body);
    }
    None
}

pub fn resize(c: CropRect, handle: Handle, pos: (f32, f32), aspect: Option<f32>) -> CropRect {
    let (mut l, mut t, mut rt, mut b) = (c.x, c.y, c.x + c.w, c.y + c.h);
    let px = clamp01(pos.0);
    let py = clamp01(pos.1);
    match handle {
        Handle::Left | Handle::TopLeft | Handle::BottomLeft => l = px.min(rt - MIN_SIZE),
        Handle::Right | Handle::TopRight | Handle::BottomRight => rt = px.max(l + MIN_SIZE),
        _ => {}
    }
    match handle {
        Handle::Top | Handle::TopLeft | Handle::TopRight => t = py.min(b - MIN_SIZE),
        Handle::Bottom | Handle::BottomLeft | Handle::BottomRight => b = py.max(t + MIN_SIZE),
        _ => {}
    }
    let mut out = CropRect {
        x: l,
        y: t,
        w: rt - l,
        h: b - t,
    };
    if let Some(ar) = aspect {
        match handle {
            // Vertical handles drive HEIGHT; derive width from height, keep the
            // horizontal center fixed.
            Handle::Top | Handle::Bottom => {
                let new_w = (out.h * ar).clamp(MIN_SIZE, 1.0);
                let cx = c.x + c.w * 0.5;
                out.x = clamp01(cx - new_w * 0.5);
                out.w = new_w;
            }
            // Horizontal handles drive WIDTH; derive height, keep vertical center fixed.
            Handle::Left | Handle::Right => {
                let new_h = (out.w / ar).clamp(MIN_SIZE, 1.0);
                let cy = c.y + c.h * 0.5;
                out.y = clamp01(cy - new_h * 0.5);
                out.h = new_h;
            }
            // Corners drive WIDTH; derive height, anchored at the opposite corner.
            _ => {
                let new_h = (out.w / ar).clamp(MIN_SIZE, 1.0);
                if matches!(handle, Handle::TopLeft | Handle::TopRight) {
                    out.y = (b - new_h).max(0.0);
                }
                out.h = new_h;
            }
        }
        if out.y + out.h > 1.0 {
            out.h = (1.0 - out.y).max(MIN_SIZE);
            out.w = out.h * ar;
        }
        if out.x + out.w > 1.0 {
            out.w = (1.0 - out.x).max(MIN_SIZE);
        }
    }
    out.x = clamp01(out.x);
    out.y = clamp01(out.y);
    // Upper bounds use `.max(MIN_SIZE)` so f32::clamp can never see min > max.
    out.w = out.w.clamp(MIN_SIZE, (1.0 - out.x).max(MIN_SIZE));
    out.h = out.h.clamp(MIN_SIZE, (1.0 - out.y).max(MIN_SIZE));
    out
}

pub fn move_body(c: CropRect, delta: (f32, f32)) -> CropRect {
    let x = (c.x + delta.0).clamp(0.0, (1.0 - c.w).max(0.0));
    let y = (c.y + delta.1).clamp(0.0, (1.0 - c.h).max(0.0));
    CropRect {
        x,
        y,
        w: c.w,
        h: c.h,
    }
}

// rotate_angle is reserved for the rotate-handle; the Angle slider wires it in a later task.
#[allow(dead_code)]
pub fn rotate_angle(center: (f32, f32), pos: (f32, f32)) -> f32 {
    let dy = pos.1 - center.1;
    let dx = pos.0 - center.0;
    dy.atan2(dx).to_degrees()
}

pub fn aspect_ratio(aspect: Aspect, img_w: u32, img_h: u32) -> Option<f32> {
    match aspect {
        Aspect::Free => None,
        Aspect::Square => Some(1.0),
        Aspect::ThreeTwo => Some(3.0 / 2.0),
        Aspect::FourThree => Some(4.0 / 3.0),
        Aspect::SixteenNine => Some(16.0 / 9.0),
        Aspect::Original => {
            if img_h == 0 {
                None
            } else {
                Some(img_w as f32 / img_h as f32)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{Aspect, CropRect};

    fn full() -> CropRect {
        CropRect::full()
    }

    #[test]
    fn hit_test_corners_and_body() {
        let c = CropRect {
            x: 0.2,
            y: 0.2,
            w: 0.6,
            h: 0.6,
        };
        assert_eq!(hit_test(c, (0.2, 0.2), 0.05), Some(Handle::TopLeft));
        assert_eq!(hit_test(c, (0.8, 0.8), 0.05), Some(Handle::BottomRight));
        assert_eq!(hit_test(c, (0.5, 0.5), 0.05), Some(Handle::Body));
        assert_eq!(
            hit_test(c, (0.95, 0.05), 0.02),
            None,
            "outside any handle/body"
        );
    }

    #[test]
    fn resize_clamps_into_unit_square() {
        let r = resize(full(), Handle::TopLeft, (-0.3, -0.3), None);
        assert!(r.x >= 0.0 && r.y >= 0.0, "origin in bounds");
        assert!(
            r.x + r.w <= 1.0 + 1e-6 && r.y + r.h <= 1.0 + 1e-6,
            "extent in bounds"
        );
        assert!(
            r.w >= MIN_SIZE - 1e-6 && r.h >= MIN_SIZE - 1e-6,
            "min size enforced"
        );
    }

    #[test]
    fn resize_with_aspect_holds_ratio() {
        let c = CropRect {
            x: 0.1,
            y: 0.1,
            w: 0.4,
            h: 0.4,
        };
        let r = resize(c, Handle::BottomRight, (0.9, 0.6), Some(2.0)); // 2:1
        assert!(
            (r.w / r.h - 2.0).abs() < 1e-3,
            "aspect held at 2:1, got {}",
            r.w / r.h
        );
    }

    #[test]
    fn move_body_clamps_inside() {
        let c = CropRect {
            x: 0.6,
            y: 0.6,
            w: 0.5,
            h: 0.5,
        };
        let m = move_body(c, (0.5, 0.5));
        assert!(
            m.x + m.w <= 1.0 + 1e-6 && m.y + m.h <= 1.0 + 1e-6,
            "stays inside"
        );
    }

    #[test]
    fn rotate_angle_is_zero_to_the_right() {
        let a = rotate_angle((0.5, 0.5), (1.0, 0.5));
        assert!(a.abs() < 1e-3, "pointer due-right of center = 0°, got {a}");
    }

    #[test]
    fn aspect_ratio_maps_presets() {
        assert_eq!(aspect_ratio(Aspect::Square, 6000, 4000), Some(1.0));
        assert_eq!(aspect_ratio(Aspect::ThreeTwo, 6000, 4000), Some(1.5));
        assert_eq!(aspect_ratio(Aspect::Free, 6000, 4000), None);
        assert_eq!(aspect_ratio(Aspect::Original, 6000, 4000), Some(1.5));
    }

    #[test]
    fn resize_aspect_top_handle_changes_crop_and_holds_ratio() {
        // Drag the Top handle up; with a 2:1 lock the crop must actually change
        // (the old code left it inert) and hold the ratio.
        let c = CropRect {
            x: 0.1,
            y: 0.1,
            w: 0.4,
            h: 0.4,
        };
        let r = resize(c, Handle::Top, (0.5, 0.0), Some(2.0));
        assert!(
            r.h > c.h,
            "top-handle drag changed the crop (not inert); h={}",
            r.h
        );
        assert!(
            (r.w / r.h - 2.0).abs() < 1e-2,
            "2:1 held; got {}",
            r.w / r.h
        );
    }

    #[test]
    fn resize_aspect_left_handle_holds_ratio() {
        let c = CropRect {
            x: 0.3,
            y: 0.3,
            w: 0.4,
            h: 0.4,
        };
        let r = resize(c, Handle::Left, (0.0, 0.5), Some(2.0));
        assert!(
            (r.w / r.h - 2.0).abs() < 1e-2,
            "2:1 held; got {}",
            r.w / r.h
        );
    }

    #[test]
    fn resize_adversarial_does_not_panic() {
        // Near-full rect + tall aspect + every handle: must not panic on the clamps.
        for h in [
            Handle::Top,
            Handle::Bottom,
            Handle::Left,
            Handle::Right,
            Handle::TopLeft,
            Handle::TopRight,
            Handle::BottomLeft,
            Handle::BottomRight,
        ] {
            let c = CropRect {
                x: 0.0,
                y: 0.0,
                w: 0.98,
                h: 0.98,
            };
            let _ = resize(c, h, (0.99, 0.99), Some(0.1));
            let _ = resize(c, h, (-0.5, -0.5), Some(50.0));
        }
    }

    #[test]
    fn move_body_oversized_crop_does_not_panic() {
        let c = CropRect {
            x: 0.0,
            y: 0.0,
            w: 1.5,
            h: 1.5,
        }; // invalid but must not panic
        let m = move_body(c, (0.2, 0.2));
        assert_eq!((m.x, m.y), (0.0, 0.0), "pinned to 0 when oversize");
    }
}
