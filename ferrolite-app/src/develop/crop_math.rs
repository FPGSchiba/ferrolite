//! Pure crop-overlay geometry in image-normalized [0,1] space. egui-free; the
//! overlay widget converts screen↔image coords and routes pointer events here.

use ferrolite_pipeline::{Aspect, CropRect};

/// Which crop-rect handle (or the body) a drag is manipulating. `crop_overlay`
/// stores this directly (it's `Copy`) as part of its mid-drag state — no
/// `u8` index translation needed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

/// Handle-drag resize in normalized [0,1]² space. `aspect`, when locked, is a
/// NORMALIZED-space ratio (w/h of the normalized rect), NOT an image-space
/// pixel ratio — convert presets with [`normalized_aspect`] first. Passing the
/// image-space value straight in holds the wrong ratio on any non-square
/// source (the root cause of the "aspect not held while dragging" bug).
pub fn resize(c: CropRect, handle: Handle, pos: (f32, f32), aspect: Option<f32>) -> CropRect {
    let px = clamp01(pos.0);
    let py = clamp01(pos.1);

    if let Some(ar) = aspect.filter(|a| a.is_finite() && *a > 0.0) {
        return resize_aspect(c, handle, px, py, ar);
    }

    let (mut l, mut t, mut rt, mut b) = (c.x, c.y, c.x + c.w, c.y + c.h);
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
    out.x = clamp01(out.x);
    out.y = clamp01(out.y);
    // Upper bounds use `.max(MIN_SIZE)` so f32::clamp can never see min > max.
    out.w = out.w.clamp(MIN_SIZE, (1.0 - out.x).max(MIN_SIZE));
    out.h = out.h.clamp(MIN_SIZE, (1.0 - out.y).max(MIN_SIZE));
    out
}

/// How one axis of the aspect-true rect relates to the drag: either pinned to a
/// fixed edge (the side opposite the dragged handle — no freedom to move, only
/// to shrink if it would overflow `[0,1]`), or centered on the rect's own
/// midpoint (the axis orthogonal to an edge handle's drag — free to shift
/// within bounds, only forced to shrink if the ideal size itself exceeds the
/// full `[0,1]` span).
#[derive(Clone, Copy)]
enum Anchor {
    Low(f32),    // fixed at this coordinate; the rect extends toward 1.0
    High(f32),   // fixed at this coordinate; the rect extends toward 0.0
    Center(f32), // fixed at this midpoint; the rect may shift either way
}

impl Anchor {
    /// Largest scale `s` (relative to `ideal`, the unscaled aspect-true size on
    /// this axis) that keeps `s * ideal` inside `[0,1]` given how this axis is
    /// anchored.
    fn max_scale(self, ideal: f32) -> f32 {
        match self {
            Anchor::Low(at) => ((1.0 - at) / ideal).max(0.0),
            Anchor::High(at) => (at / ideal).max(0.0),
            // A centered axis can always shift to fit as long as the ideal size
            // itself isn't larger than the whole [0,1] span.
            Anchor::Center(_) => (1.0 / ideal).max(0.0),
        }
    }

    /// The coordinate for a rect of size `size` on this axis: the fixed
    /// position for a pinned edge, or a bounds-clamped shift for a centered one.
    fn place(self, size: f32) -> f32 {
        match self {
            Anchor::Low(at) => at,
            Anchor::High(at) => at - size,
            Anchor::Center(at) => (at - size * 0.5).clamp(0.0, (1.0 - size).max(0.0)),
        }
    }
}

/// Aspect-locked resize. Derives the aspect-true rect the handle's drag
/// implies, then scales BOTH axes together by the largest factor that fits the
/// image bounds and the min-size floor, anchored at the handle's fixed
/// corner/edge — so the ratio never drifts, unlike clamping each axis
/// independently after the fact. If no scale satisfies both the min-size floor
/// and the bounds (an extreme ratio anchored right at a corner/edge can make
/// this infeasible), the previous rect is returned unchanged rather than emit
/// an out-of-bounds or sub-floor crop.
fn resize_aspect(c: CropRect, handle: Handle, px: f32, py: f32, ar: f32) -> CropRect {
    let (l, t, rt, b) = (c.x, c.y, c.x + c.w, c.y + c.h);
    let cx = c.x + c.w * 0.5;
    let cy = c.y + c.h * 0.5;

    // Corners and the width-driving edge handles (Left/Right) derive height
    // from a pointer-driven width; the height-driving edge handles (Top/Bottom)
    // derive width from a pointer-driven height — matches the pre-fix behavior.
    let (ideal_w, ideal_h, x_anchor, y_anchor) = match handle {
        Handle::Left => {
            let w = (rt - px).max(MIN_SIZE);
            (w, w / ar, Anchor::High(rt), Anchor::Center(cy))
        }
        Handle::Right => {
            let w = (px - l).max(MIN_SIZE);
            (w, w / ar, Anchor::Low(l), Anchor::Center(cy))
        }
        Handle::Top => {
            let h = (b - py).max(MIN_SIZE);
            (h * ar, h, Anchor::Center(cx), Anchor::High(b))
        }
        Handle::Bottom => {
            let h = (py - t).max(MIN_SIZE);
            (h * ar, h, Anchor::Center(cx), Anchor::Low(t))
        }
        Handle::TopLeft => {
            let w = (rt - px).max(MIN_SIZE);
            (w, w / ar, Anchor::High(rt), Anchor::High(b))
        }
        Handle::TopRight => {
            let w = (px - l).max(MIN_SIZE);
            (w, w / ar, Anchor::Low(l), Anchor::High(b))
        }
        Handle::BottomLeft => {
            let w = (rt - px).max(MIN_SIZE);
            (w, w / ar, Anchor::High(rt), Anchor::Low(t))
        }
        Handle::BottomRight => {
            let w = (px - l).max(MIN_SIZE);
            (w, w / ar, Anchor::Low(l), Anchor::Low(t))
        }
        // Not driven through resize() in practice (Body drags route through
        // move_body instead), but kept exhaustive and non-panicking: hold the
        // current width, derive height, anchored at the rect's own top-left.
        Handle::Body => {
            let w = c.w.max(MIN_SIZE);
            (w, w / ar, Anchor::Low(l), Anchor::Low(t))
        }
    };

    let s_max = x_anchor.max_scale(ideal_w).min(y_anchor.max_scale(ideal_h));
    let s_min_floor = (MIN_SIZE / ideal_w).max(MIN_SIZE / ideal_h);
    if s_min_floor > s_max {
        // No aspect-true rect anchored here satisfies both the min-size floor
        // and the image bounds. Degrade gracefully: keep the previous rect.
        return c;
    }
    let s = 1.0_f32.clamp(s_min_floor, s_max);

    let w = ideal_w * s;
    let h = ideal_h * s;
    CropRect {
        x: clamp01(x_anchor.place(w)),
        y: clamp01(y_anchor.place(h)),
        w,
        h,
    }
}

/// Conform an existing crop rect to a NORMALIZED-space aspect ratio in one
/// discrete step — used when the user PICKS a new aspect (chip or combo), as
/// opposed to [`resize`], which constrains a drag. Keeps the rect's center and
/// preserves its area exactly when feasible; otherwise scales both axes
/// together (the same feasible-scale machinery as `resize_aspect`) down to the
/// largest aspect-true rect that fits `[0,1]²` and the min-size floor,
/// shifting off-center only as far as the bounds force. A non-finite or
/// non-positive `ar`, or an infeasible request (a ratio more extreme than the
/// floor allows anywhere in the frame), returns the input rect unchanged.
pub fn conform_to_aspect(c: CropRect, ar: f32) -> CropRect {
    if !ar.is_finite() || ar <= 0.0 {
        return c;
    }
    // Area-preserving ideal: w/h == ar and w*h == area(c).
    let area = (c.w * c.h).max(MIN_SIZE * MIN_SIZE);
    let ideal_w = (area * ar).sqrt();
    let ideal_h = (area / ar).sqrt();
    let x_anchor = Anchor::Center(c.x + c.w * 0.5);
    let y_anchor = Anchor::Center(c.y + c.h * 0.5);
    let s_max = x_anchor.max_scale(ideal_w).min(y_anchor.max_scale(ideal_h));
    let s_min_floor = (MIN_SIZE / ideal_w).max(MIN_SIZE / ideal_h);
    if s_min_floor > s_max {
        return c;
    }
    let s = 1.0_f32.clamp(s_min_floor, s_max);
    let w = ideal_w * s;
    let h = ideal_h * s;
    CropRect {
        x: clamp01(x_anchor.place(w)),
        y: clamp01(y_anchor.place(h)),
        w,
        h,
    }
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
        Aspect::FiveFour => Some(5.0 / 4.0),
        // Portrait counterparts (ratio < 1) of the four non-square landscape
        // presets above — see `Aspect`'s doc comments and
        // `develop::tools::crop::flipped` for the pairing.
        Aspect::ThreeFour => Some(3.0 / 4.0),
        Aspect::TwoThree => Some(2.0 / 3.0),
        Aspect::NineSixteen => Some(9.0 / 16.0),
        Aspect::FourFive => Some(4.0 / 5.0),
        Aspect::Original => {
            if img_h == 0 {
                None
            } else {
                Some(img_w as f32 / img_h as f32)
            }
        }
    }
}

/// [`aspect_ratio`]'s IMAGE-space ratio (pixel w/h, e.g. 1.5 for 3:2)
/// converted into the equivalent ratio in the normalized [0,1]² space that
/// [`resize`] and [`conform_to_aspect`] operate in. The two spaces differ by
/// the source's own shape: a normalized rect `(w, h)` spans `(w·img_w,
/// h·img_h)` pixels, so its PIXEL ratio is `(w/h)·(img_w/img_h)`. Locking a
/// pixel ratio `ar` therefore requires the normalized ratio `ar·img_h/img_w`
/// — e.g. 3:2 on a 6000×4000 source is exactly 1.0 in normalized space (the
/// full frame), NOT 1.5. `Aspect::Original` always maps to 1.0.
pub fn normalized_aspect(aspect: Aspect, img_w: u32, img_h: u32) -> Option<f32> {
    if img_w == 0 || img_h == 0 {
        return None;
    }
    let ar = aspect_ratio(aspect, img_w, img_h)?;
    Some(ar * img_h as f32 / img_w as f32)
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
        assert_eq!(aspect_ratio(Aspect::FourThree, 6000, 4000), Some(4.0 / 3.0));
        assert_eq!(
            aspect_ratio(Aspect::SixteenNine, 6000, 4000),
            Some(16.0 / 9.0)
        );
        assert_eq!(aspect_ratio(Aspect::FiveFour, 6000, 4000), Some(1.25));
        assert_eq!(aspect_ratio(Aspect::Free, 6000, 4000), None);
        assert_eq!(aspect_ratio(Aspect::Original, 6000, 4000), Some(1.5));
    }

    /// The four portrait presets (Task "crop-portrait"): each is the exact
    /// reciprocal of its landscape counterpart's ratio, independent of source
    /// dims (dims only matter for `Aspect::Original`).
    #[test]
    fn aspect_ratio_maps_portrait_presets_as_reciprocals_of_their_landscape_counterpart() {
        assert_eq!(aspect_ratio(Aspect::ThreeFour, 6000, 4000), Some(0.75));
        assert_eq!(aspect_ratio(Aspect::TwoThree, 6000, 4000), Some(2.0 / 3.0));
        assert_eq!(aspect_ratio(Aspect::NineSixteen, 6000, 4000), Some(0.5625));
        assert_eq!(aspect_ratio(Aspect::FourFive, 6000, 4000), Some(0.8));

        for (portrait, landscape) in [
            (Aspect::ThreeFour, Aspect::FourThree),
            (Aspect::TwoThree, Aspect::ThreeTwo),
            (Aspect::NineSixteen, Aspect::SixteenNine),
            (Aspect::FourFive, Aspect::FiveFour),
        ] {
            let p = aspect_ratio(portrait, 6000, 4000).unwrap();
            let l = aspect_ratio(landscape, 6000, 4000).unwrap();
            assert!(
                (p * l - 1.0).abs() < 1e-6,
                "{portrait:?} ({p}) must be the reciprocal of {landscape:?} ({l})"
            );
        }
    }

    #[test]
    fn normalized_aspect_converts_by_source_shape() {
        // 3:2 on a 3:2 (6000×4000) source: the normalized ratio is 1.0 (the
        // full frame IS 3:2), not the image-space 1.5.
        assert_eq!(aspect_ratio(Aspect::ThreeTwo, 6000, 4000), Some(1.5));
        assert_eq!(normalized_aspect(Aspect::ThreeTwo, 6000, 4000), Some(1.0));
        // 1:1 on the same source needs a normalized w/h of 4000/6000.
        let sq = normalized_aspect(Aspect::Square, 6000, 4000).unwrap();
        assert!((sq - 4000.0 / 6000.0).abs() < 1e-6);
        // Original is always the full frame: normalized 1.0, any source shape.
        assert_eq!(normalized_aspect(Aspect::Original, 6000, 4000), Some(1.0));
        assert_eq!(normalized_aspect(Aspect::Original, 4000, 6000), Some(1.0));
        // Free stays unconstrained; degenerate dims yield None, not a NaN/inf.
        assert_eq!(normalized_aspect(Aspect::Free, 6000, 4000), None);
        assert_eq!(normalized_aspect(Aspect::ThreeTwo, 0, 4000), None);
        assert_eq!(normalized_aspect(Aspect::ThreeTwo, 6000, 0), None);
    }

    /// The bug-A regression, end to end in pure math: on a 6000×4000 source
    /// with the 3:2 preset (image-space ratio 1.5), the ratio handed to
    /// `resize` must be the NORMALIZED one, and the resulting rect must be
    /// 3:2 in PIXELS — the space the user (and the author's screenshot) sees.
    #[test]
    fn resize_with_normalized_aspect_yields_a_three_two_pixel_rect() {
        let (img_w, img_h) = (6000_u32, 4000_u32);
        let ar_norm = normalized_aspect(Aspect::ThreeTwo, img_w, img_h).unwrap();
        let c = CropRect {
            x: 0.1,
            y: 0.1,
            w: 0.4,
            h: 0.4,
        };
        for (handle, pos) in [
            (Handle::BottomRight, (0.9_f32, 0.6_f32)),
            (Handle::Top, (0.5, 0.02)),
            (Handle::Left, (0.02, 0.5)),
        ] {
            let r = resize(c, handle, pos, Some(ar_norm));
            let pixel_ratio = (r.w * img_w as f32) / (r.h * img_h as f32);
            assert!(
                (pixel_ratio - 1.5).abs() < 1e-3,
                "{handle:?}: pixel rect not 3:2, got {pixel_ratio} ({r:?})"
            );
        }
    }

    #[test]
    fn conform_to_aspect_keeps_center_and_area_when_feasible() {
        let c = CropRect {
            x: 0.3,
            y: 0.3,
            w: 0.4,
            h: 0.4,
        };
        let ar = 2.0_f32;
        let r = conform_to_aspect(c, ar);
        assert!(
            (r.w / r.h - ar).abs() < 1e-4,
            "ratio exact, got {}",
            r.w / r.h
        );
        assert!(
            (r.x + r.w * 0.5 - 0.5).abs() < 1e-4 && (r.y + r.h * 0.5 - 0.5).abs() < 1e-4,
            "center kept, got {r:?}"
        );
        assert!(
            (r.w * r.h - c.w * c.h).abs() < 1e-4,
            "area preserved, got {}",
            r.w * r.h
        );
    }

    #[test]
    fn conform_to_aspect_stays_in_bounds_near_edges() {
        // A rect hugging the bottom-right corner conformed to a wide ratio:
        // the result must hold the ratio exactly and stay inside [0,1]²,
        // shifting/scaling as needed.
        let c = CropRect {
            x: 0.55,
            y: 0.55,
            w: 0.44,
            h: 0.44,
        };
        for ar in [3.0_f32, 1.0 / 3.0] {
            let r = conform_to_aspect(c, ar);
            assert!(
                (r.w / r.h - ar).abs() < 1e-3,
                "ratio held, got {}",
                r.w / r.h
            );
            assert!(
                r.x >= -1e-6 && r.y >= -1e-6 && r.x + r.w <= 1.0 + 1e-6 && r.y + r.h <= 1.0 + 1e-6,
                "in bounds, got {r:?}"
            );
            assert!(r.w >= MIN_SIZE - 1e-6 && r.h >= MIN_SIZE - 1e-6);
        }
    }

    #[test]
    fn conform_to_aspect_full_rect_to_original_stays_full() {
        // "Original" is normalized ratio 1.0; an uncropped frame stays uncropped.
        let r = conform_to_aspect(full(), 1.0);
        assert!(r.x.abs() < 1e-6 && r.y.abs() < 1e-6);
        assert!((r.w - 1.0).abs() < 1e-6 && (r.h - 1.0).abs() < 1e-6);
    }

    #[test]
    fn conform_to_aspect_rejects_degenerate_ratios() {
        let c = CropRect {
            x: 0.2,
            y: 0.2,
            w: 0.5,
            h: 0.5,
        };
        for ar in [0.0_f32, -1.0, f32::NAN, f32::INFINITY] {
            let r = conform_to_aspect(c, ar);
            assert_eq!(
                (r.x, r.y, r.w, r.h),
                (c.x, c.y, c.w, c.h),
                "ar {ar} is a no-op"
            );
        }
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

    // Same sweep as `resize_adversarial_does_not_panic`, now also asserting the
    // aspect ratio holds whenever `resize` doesn't fall back to the documented
    // "no feasible rect" case (see `resize_aspect_no_feasible_rect_keeps_previous_rect`).
    // A result is treated as that fallback when it is exactly the input rect.
    #[test]
    fn resize_adversarial_keeps_ratio() {
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
            for (pos, ar) in [((0.99_f32, 0.99_f32), 0.1_f32), ((-0.5, -0.5), 50.0_f32)] {
                let r = resize(c, h, pos, Some(ar));
                let is_fallback = (r.x - c.x).abs() < 1e-6
                    && (r.y - c.y).abs() < 1e-6
                    && (r.w - c.w).abs() < 1e-6
                    && (r.h - c.h).abs() < 1e-6;
                if is_fallback {
                    continue;
                }
                assert!(
                    (r.w / r.h - ar).abs() < 1e-3,
                    "{h:?} @ ar {ar}: ratio not held, got {} ({r:?})",
                    r.w / r.h
                );
                assert!(
                    r.x >= -1e-6 && r.y >= -1e-6,
                    "{h:?} @ ar {ar}: origin in bounds, got {r:?}"
                );
                assert!(
                    r.x + r.w <= 1.0 + 1e-6 && r.y + r.h <= 1.0 + 1e-6,
                    "{h:?} @ ar {ar}: extent in bounds, got {r:?}"
                );
                assert!(
                    r.w >= MIN_SIZE - 1e-6 && r.h >= MIN_SIZE - 1e-6,
                    "{h:?} @ ar {ar}: min size enforced, got {r:?}"
                );
            }
        }
    }

    #[test]
    fn resize_boundary_holds_ratio_for_all_handles() {
        // A moderate centered crop, dragged far past each edge/corner: with a
        // non-extreme ratio + tiny MIN_SIZE floor, a feasible aspect-true rect
        // always exists, so the ratio must hold exactly (not just approximately
        // by the independent per-axis clamps that used to run after).
        let c = CropRect {
            x: 0.3,
            y: 0.3,
            w: 0.4,
            h: 0.4,
        };
        // 1.0, 1.5, and a 16:9 preset adjusted by a non-square (6000x4000) sensor
        // factor — i.e. the same kind of value `aspect_ratio` callers pass in.
        let ratios = [1.0_f32, 1.5_f32, (16.0_f32 / 9.0) * (4000.0 / 6000.0)];
        let drags: [(Handle, (f32, f32)); 8] = [
            (Handle::TopLeft, (-10.0, -10.0)),
            (Handle::Top, (0.5, -10.0)),
            (Handle::TopRight, (10.0, -10.0)),
            (Handle::Right, (10.0, 0.5)),
            (Handle::BottomRight, (10.0, 10.0)),
            (Handle::Bottom, (0.5, 10.0)),
            (Handle::BottomLeft, (-10.0, 10.0)),
            (Handle::Left, (-10.0, 0.5)),
        ];
        for ratio in ratios {
            for (handle, pos) in drags {
                let r = resize(c, handle, pos, Some(ratio));
                assert!(
                    (r.w / r.h - ratio).abs() < 1e-4,
                    "{handle:?} @ ratio {ratio}: ratio not held, got {} ({r:?})",
                    r.w / r.h
                );
                assert!(
                    r.x >= -1e-6
                        && r.y >= -1e-6
                        && r.x + r.w <= 1.0 + 1e-6
                        && r.y + r.h <= 1.0 + 1e-6,
                    "{handle:?} @ ratio {ratio}: out of [0,1]^2: {r:?}"
                );
                assert!(
                    r.w >= MIN_SIZE - 1e-6 && r.h >= MIN_SIZE - 1e-6,
                    "{handle:?} @ ratio {ratio}: below min size: {r:?}"
                );
            }
        }
    }

    #[test]
    fn resize_aspect_no_feasible_rect_keeps_previous_rect() {
        // A tiny rect pinned near the top-left corner, asked to resize to an
        // extreme 1000:1 aspect anchored at the fixed left edge: no rect of at
        // least MIN_SIZE in both dimensions fits inside [0,1] at that anchor
        // (height would have to be 0.02/1000). Per the documented fallback,
        // resize() leaves the crop unchanged rather than emit an out-of-bounds
        // or sub-floor rect.
        let c = CropRect {
            x: 0.0,
            y: 0.0,
            w: 0.1,
            h: 0.1,
        };
        let r = resize(c, Handle::Right, (0.02, 0.02), Some(1000.0));
        assert_eq!((r.x, r.y, r.w, r.h), (c.x, c.y, c.w, c.h));
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
