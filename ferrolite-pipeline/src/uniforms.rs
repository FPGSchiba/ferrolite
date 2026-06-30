//! Pure CPU math turning UI op params into GPU shader uniforms, plus the
//! `#[repr(C)]` Pod uniform structs (layouts MIRROR the WGSL `struct P` in each
//! shader). Display-linear space; the sRGB OETF lives only in the display/blit
//! shader. No GPU here — fully unit-tested.

use crate::op::{Contrast, Exposure, WhiteBalance};

/// Mid-grey pivot (display-linear) about which contrast scales. Placeholder
/// constant; Spec 3 may refine once the working space is fixed.
pub const CONTRAST_PIVOT: f32 = 0.18;

/// EV (stops) -> linear gain. `2^ev`. ev=0 -> 1.0 (identity).
pub fn exposure_gain(ev: f32) -> f32 {
    2.0f32.powf(ev)
}

/// Normalized temp/tint in [-1,1] -> per-channel linear multipliers `[r,g,b]`.
/// Pragmatic placeholder (image science is secondary): warm temp boosts R /
/// cuts B; magenta tint cuts G. Clamped non-negative.
pub fn wb_multipliers(temp: f32, tint: f32) -> [f32; 3] {
    let r = (1.0 + 0.5 * temp).max(0.0);
    let b = (1.0 - 0.5 * temp).max(0.0);
    let g = (1.0 - 0.5 * tint).max(0.0);
    [r, g, b]
}

/// Bipolar amount -> (gain, pivot). amount=0 -> gain 1.0 (identity).
pub fn contrast_gain_pivot(amount: f32) -> (f32, f32) {
    (1.0 + amount, CONTRAST_PIVOT)
}

/// Bake tone-curve control points into a 256-entry display-linear LUT.
/// Points are clamped to [0,1], sorted by x, linearly interpolated, and held
/// flat outside the control range; the result is forced monotone
/// non-decreasing. Empty input is the identity ramp.
pub fn curve_lut(points: &[(f32, f32)]) -> [f32; 256] {
    let mut pts: Vec<(f32, f32)> = points
        .iter()
        .map(|&(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
        .collect();
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if pts.is_empty() {
        pts = vec![(0.0, 0.0), (1.0, 1.0)];
    }

    let mut lut = [0.0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let x = i as f32 / 255.0;
        *slot = curve_interp(&pts, x);
    }
    for i in 1..256 {
        if lut[i] < lut[i - 1] {
            lut[i] = lut[i - 1];
        }
    }
    lut
}

/// Piecewise-linear sample of sorted control points; flat (clamped) outside.
fn curve_interp(pts: &[(f32, f32)], x: f32) -> f32 {
    if x <= pts[0].0 {
        return pts[0].1;
    }
    let last = pts[pts.len() - 1];
    if x >= last.0 {
        return last.1;
    }
    for w in pts.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        if x >= x0 && x <= x1 {
            let t = if (x1 - x0).abs() < 1e-9 {
                0.0
            } else {
                (x - x0) / (x1 - x0)
            };
            return y0 + t * (y1 - y0);
        }
    }
    last.1
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ExposureUniform {
    pub gain: f32,
    pub pad: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WbUniform {
    pub mul: [f32; 3],
    pub pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ContrastUniform {
    pub gain: f32,
    pub pivot: f32,
    pub pad: [f32; 2],
}

pub fn exposure_uniform(op: Option<Exposure>) -> ExposureUniform {
    let ev = op.map(|e| e.ev).unwrap_or(0.0);
    ExposureUniform {
        gain: exposure_gain(ev),
        pad: [0.0; 3],
    }
}

pub fn wb_uniform(op: Option<WhiteBalance>) -> WbUniform {
    let (t, ti) = op.map(|w| (w.temp, w.tint)).unwrap_or((0.0, 0.0));
    WbUniform {
        mul: wb_multipliers(t, ti),
        pad: 0.0,
    }
}

pub fn contrast_uniform(op: Option<Contrast>) -> ContrastUniform {
    let a = op.map(|c| c.amount).unwrap_or(0.0);
    let (gain, pivot) = contrast_gain_pivot(a);
    ContrastUniform {
        gain,
        pivot,
        pad: [0.0; 2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_gain_is_two_to_the_ev() {
        assert!((exposure_gain(0.0) - 1.0).abs() < 1e-6);
        assert!((exposure_gain(1.0) - 2.0).abs() < 1e-6);
        assert!((exposure_gain(-1.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn wb_identity_at_zero() {
        assert_eq!(wb_multipliers(0.0, 0.0), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn wb_warm_temp_boosts_red_cuts_blue() {
        assert_eq!(wb_multipliers(1.0, 0.0), [1.5, 1.0, 0.5]);
    }

    #[test]
    fn wb_magenta_tint_cuts_green() {
        assert_eq!(wb_multipliers(0.0, 1.0), [1.0, 0.5, 1.0]);
    }

    #[test]
    fn contrast_identity_and_gain() {
        assert_eq!(contrast_gain_pivot(0.0), (1.0, CONTRAST_PIVOT));
        assert_eq!(contrast_gain_pivot(1.0), (2.0, CONTRAST_PIVOT));
    }

    #[test]
    fn uniform_constructors_use_identity_when_absent() {
        assert_eq!(exposure_uniform(None).gain, 1.0);
        assert_eq!(wb_uniform(None).mul, [1.0, 1.0, 1.0]);
        assert_eq!(contrast_uniform(None).gain, 1.0);
    }

    #[test]
    fn curve_lut_identity_is_a_linear_ramp() {
        let lut = curve_lut(&[(0.0, 0.0), (1.0, 1.0)]);
        assert!((lut[0] - 0.0).abs() < 1e-6);
        assert!((lut[255] - 1.0).abs() < 1e-6);
        assert!((lut[128] - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn curve_lut_empty_points_is_identity() {
        let lut = curve_lut(&[]);
        assert!((lut[64] - 64.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn curve_lut_pulls_midtones_down() {
        // A point below the diagonal at x=0.5 darkens the midtones.
        let lut = curve_lut(&[(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)]);
        assert!(lut[128] < 128.0 / 255.0, "midpoint pulled below diagonal");
        assert!((lut[0] - 0.0).abs() < 1e-6);
        assert!((lut[255] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn curve_lut_is_monotone_non_decreasing() {
        // A non-monotone control set must still produce a non-decreasing LUT.
        let lut = curve_lut(&[(0.0, 0.0), (0.5, 0.8), (1.0, 0.2)]);
        for i in 1..256 {
            assert!(lut[i] >= lut[i - 1], "lut dipped at {i}");
        }
    }
}
