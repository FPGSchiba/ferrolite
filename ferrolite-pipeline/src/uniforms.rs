//! Pure CPU math turning UI op params into GPU shader uniforms, plus the
//! `#[repr(C)]` Pod uniform structs (layouts MIRROR the WGSL `struct P` in each
//! shader). Display-linear space; the sRGB OETF lives only in the display/blit
//! shader. No GPU here — fully unit-tested.

use crate::op::{Aspect, Contrast, CropRect, Exposure, Geometry, Hsl, Sharpen, WhiteBalance};

/// Mid-grey pivot (display-linear) about which contrast scales. Placeholder
/// constant; Spec 3 may refine once the working space is fixed.
pub const CONTRAST_PIVOT: f32 = 0.18;

/// Safety cap on sharpen radius (pixels). Far above any realistic preview-res
/// sharpen; bounds the box-blur loop and prevents a u32->i32 wrap to negative.
pub const MAX_SHARPEN_RADIUS: u32 = 256;

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

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct HslUniform {
    /// 8 bands × (hue, sat, lum, pad). Mirrors WGSL `array<vec4<f32>, 8>`.
    pub bands: [[f32; 4]; 8],
}

pub fn hsl_uniform(op: Option<Hsl>) -> HslUniform {
    let mut bands = [[0.0f32; 4]; 8];
    if let Some(h) = op {
        for (i, b) in h.bands.iter().enumerate() {
            bands[i] = [b.hue, b.sat, b.lum, 0.0];
        }
    }
    HslUniform { bands }
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

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SharpenUniform {
    pub amount: f32,
    pub radius: i32,
    pub pad: [f32; 2],
}

pub fn sharpen_uniform(op: Option<Sharpen>) -> SharpenUniform {
    let (amount, radius) = op.map(|s| (s.amount, s.radius)).unwrap_or((0.0, 0));
    SharpenUniform {
        amount,
        radius: radius.min(MAX_SHARPEN_RADIUS) as i32,
        pad: [0.0; 2],
    }
}

/// Halo (pixels) a tiled full-res sharpen pass must over-fetch. Zero when the
/// op is absent or a no-op (amount 0). Consumed by Plan 3's tile producer.
pub fn sharpen_halo(op: Option<Sharpen>) -> u32 {
    match op {
        Some(s) if s.amount != 0.0 => s.radius.min(MAX_SHARPEN_RADIUS),
        _ => 0,
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GeometryUniform {
    /// Row-major 2×2 mapping output-pixel → source-pixel: [m00, m01, m10, m11].
    pub m: [f32; 4],
    /// Source-pixel translation: src = m·out + off.
    pub off: [f32; 2],
    pub src_dims: [f32; 2],
    pub out_dims: [f32; 2],
    pub pad: [f32; 2],
}

/// Crop + rotate as a sampling transform. Returns the uniform plus the output
/// (width, height) in pixels. Maps each output pixel center to a source pixel:
/// `src = R(angle)·(out − out_center) + crop_center`, sampled bilinearly.
pub fn geometry_uniform(
    op: Option<Geometry>,
    src_w: u32,
    src_h: u32,
) -> (GeometryUniform, u32, u32) {
    let sw = src_w as f32;
    let sh = src_h as f32;
    let geo = op.unwrap_or(Geometry {
        crop: CropRect::full(),
        angle_deg: 0.0,
        aspect: Aspect::Original,
    });

    let cx = geo.crop.x.clamp(0.0, 1.0);
    let cy = geo.crop.y.clamp(0.0, 1.0);
    let cw = geo.crop.w.clamp(1e-4, (1.0 - cx).max(1e-4));
    let ch = geo.crop.h.clamp(1e-4, (1.0 - cy).max(1e-4));

    let crop_w_px = cw * sw;
    let crop_h_px = ch * sh;
    let out_w = (crop_w_px.round() as u32).max(1);
    let out_h = (crop_h_px.round() as u32).max(1);

    let theta = geo.angle_deg.to_radians();
    let (s, c) = theta.sin_cos();
    let m = [c, -s, s, c];

    let out_center = [out_w as f32 * 0.5, out_h as f32 * 0.5];
    let crop_center = [cx * sw + crop_w_px * 0.5, cy * sh + crop_h_px * 0.5];
    let off = [
        crop_center[0] - (m[0] * out_center[0] + m[1] * out_center[1]),
        crop_center[1] - (m[2] * out_center[0] + m[3] * out_center[1]),
    ];

    (
        GeometryUniform {
            m,
            off,
            src_dims: [sw, sh],
            out_dims: [out_w as f32, out_h as f32],
            pad: [0.0; 2],
        },
        out_w,
        out_h,
    )
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

    #[test]
    fn hsl_uniform_identity_is_all_zero() {
        let u = hsl_uniform(None);
        assert_eq!(u.bands, [[0.0; 4]; 8]);
    }

    #[test]
    fn hsl_uniform_packs_bands_in_order() {
        use crate::op::{Hsl, HslBand};
        let mut bands = [HslBand {
            hue: 0.0,
            sat: 0.0,
            lum: 0.0,
        }; 8];
        bands[3] = HslBand {
            hue: 0.2,
            sat: -0.3,
            lum: 0.1,
        };
        let u = hsl_uniform(Some(Hsl { bands }));
        assert_eq!(u.bands[3], [0.2, -0.3, 0.1, 0.0]);
        assert_eq!(u.bands[0], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn sharpen_uniform_identity_when_absent() {
        let u = sharpen_uniform(None);
        assert_eq!(u.amount, 0.0);
        assert_eq!(u.radius, 0);
    }

    #[test]
    fn sharpen_uniform_carries_amount_and_radius() {
        use crate::op::Sharpen;
        let u = sharpen_uniform(Some(Sharpen {
            amount: 0.75,
            radius: 3,
        }));
        assert_eq!(u.amount, 0.75);
        assert_eq!(u.radius, 3);
    }

    #[test]
    fn sharpen_halo_is_radius_or_zero() {
        use crate::op::Sharpen;
        assert_eq!(sharpen_halo(None), 0);
        // amount 0 contributes no halo even with radius set.
        assert_eq!(
            sharpen_halo(Some(Sharpen {
                amount: 0.0,
                radius: 4
            })),
            0
        );
        assert_eq!(
            sharpen_halo(Some(Sharpen {
                amount: 0.5,
                radius: 4
            })),
            4
        );
    }

    #[test]
    fn sharpen_radius_is_clamped_to_max() {
        use crate::op::Sharpen;
        let huge = Sharpen {
            amount: 0.5,
            radius: u32::MAX,
        };
        assert_eq!(
            sharpen_uniform(Some(huge)).radius,
            MAX_SHARPEN_RADIUS as i32
        );
        assert_eq!(sharpen_halo(Some(huge)), MAX_SHARPEN_RADIUS);
        // No wrap to negative.
        assert!(sharpen_uniform(Some(huge)).radius > 0);
    }

    #[test]
    fn geometry_uniform_identity_when_absent() {
        let (u, w, h) = geometry_uniform(None, 64, 48);
        assert_eq!((w, h), (64, 48));
        assert_eq!(u.m, [1.0, 0.0, 0.0, 1.0]);
        assert!(u.off[0].abs() < 1e-4 && u.off[1].abs() < 1e-4);
        assert_eq!(u.src_dims, [64.0, 48.0]);
        assert_eq!(u.out_dims, [64.0, 48.0]);
    }

    #[test]
    fn geometry_uniform_crop_halves_output_dims() {
        use crate::op::{Aspect, CropRect, Geometry};
        let (_, w, h) = geometry_uniform(
            Some(Geometry {
                crop: CropRect {
                    x: 0.25,
                    y: 0.25,
                    w: 0.5,
                    h: 0.5,
                },
                angle_deg: 0.0,
                aspect: Aspect::Free,
            }),
            64,
            48,
        );
        assert_eq!((w, h), (32, 24));
    }

    #[test]
    fn geometry_uniform_rotation_sets_rotation_matrix() {
        use crate::op::{Aspect, CropRect, Geometry};
        let (u, _, _) = geometry_uniform(
            Some(Geometry {
                crop: CropRect::full(),
                angle_deg: 90.0,
                aspect: Aspect::Original,
            }),
            64,
            48,
        );
        // 90°: cos=0, sin=1 -> m = [0,-1,1,0] (row-major).
        assert!(u.m[0].abs() < 1e-5);
        assert!((u.m[1] - -1.0).abs() < 1e-5);
        assert!((u.m[2] - 1.0).abs() < 1e-5);
        assert!(u.m[3].abs() < 1e-5);
    }
}
