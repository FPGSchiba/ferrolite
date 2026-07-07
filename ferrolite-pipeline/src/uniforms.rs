//! Pure CPU math turning UI op params into GPU shader uniforms, plus the
//! `#[repr(C)]` Pod uniform structs (layouts MIRROR the WGSL `struct P` in each
//! shader). Display-linear space; the sRGB OETF lives only in the display/blit
//! shader. No GPU here — fully unit-tested.

use crate::op::{
    Aspect, Contrast, CropRect, Exposure, Geometry, Hsl, LensCorrection, Sharpen, WhiteBalance,
};
use ferrolite_lens::{lens_halo, WarpGrid};

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
/// Points are clamped to [0,1], sorted by x, interpolated per `mode`, and held
/// flat outside the control range; the result is forced monotone
/// non-decreasing. Empty input is the identity ramp.
pub fn curve_lut(points: &[(f32, f32)], mode: crate::op::CurveMode) -> [f32; 256] {
    let mut pts: Vec<(f32, f32)> = points
        .iter()
        .map(|&(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
        .collect();
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if pts.is_empty() {
        pts = vec![(0.0, 0.0), (1.0, 1.0)];
    }
    let mut lut = [0.0f32; 256];
    match mode {
        crate::op::CurveMode::Linear => {
            for (i, slot) in lut.iter_mut().enumerate() {
                *slot = curve_interp_linear(&pts, i as f32 / 255.0);
            }
        }
        crate::op::CurveMode::Smooth => {
            let tangents = fritsch_carlson_tangents(&pts);
            for (i, slot) in lut.iter_mut().enumerate() {
                *slot = curve_interp_smooth(&pts, &tangents, i as f32 / 255.0);
            }
        }
    }
    for i in 1..256 {
        if lut[i] < lut[i - 1] {
            lut[i] = lut[i - 1];
        }
    }
    lut
}

/// Piecewise-linear sample of sorted control points; flat (clamped) outside.
fn curve_interp_linear(pts: &[(f32, f32)], x: f32) -> f32 {
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

/// Fritsch–Carlson monotone tangents for control points (x ascending).
fn fritsch_carlson_tangents(pts: &[(f32, f32)]) -> Vec<f32> {
    let n = pts.len();
    if n < 2 {
        return vec![0.0; n];
    }
    // Secant slopes.
    let mut d = vec![0.0f32; n - 1];
    for i in 0..n - 1 {
        let dx = pts[i + 1].0 - pts[i].0;
        d[i] = if dx.abs() < 1e-9 {
            0.0
        } else {
            (pts[i + 1].1 - pts[i].1) / dx
        };
    }
    // Initial tangents (average of adjacent secants; ends = one-sided).
    let mut m = vec![0.0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = (d[i - 1] + d[i]) / 2.0;
    }
    // Fritsch–Carlson limiter: enforce monotonicity / no overshoot.
    for i in 0..n - 1 {
        if d[i].abs() < 1e-9 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let alpha = m[i] / d[i];
            let beta = m[i + 1] / d[i];
            let s = alpha * alpha + beta * beta;
            if s > 9.0 {
                let tau = 3.0 / s.sqrt();
                m[i] = tau * alpha * d[i];
                m[i + 1] = tau * beta * d[i];
            }
        }
    }
    m
}

fn curve_interp_smooth(pts: &[(f32, f32)], m: &[f32], x: f32) -> f32 {
    if x <= pts[0].0 {
        return pts[0].1;
    }
    let last = pts[pts.len() - 1];
    if x >= last.0 {
        return last.1;
    }
    for i in 0..pts.len() - 1 {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[i + 1];
        if x >= x0 && x <= x1 {
            let h = x1 - x0;
            if h.abs() < 1e-9 {
                return y1;
            }
            let t = (x - x0) / h;
            let t2 = t * t;
            let t3 = t2 * t;
            let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
            let h10 = t3 - 2.0 * t2 + t;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = t3 - t2;
            return h00 * y0 + h10 * h * m[i] + h01 * y1 + h11 * h * m[i + 1];
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
    /// Output-pixel origin added to `gid` before the transform, so a tile pass can
    /// render a sub-region of the output image. Whole-image path uses `[0,0]`.
    pub out_origin: [f32; 2],
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
            out_origin: [0.0, 0.0],
        },
        out_w,
        out_h,
    )
}

/// A per-tile geometry-head uniform: identical `m`/`off`/`src_dims` to
/// `geometry_uniform` at the given source dims, but with the output origin set to
/// the haloed tile's top-left (may be negative) and `out_dims` set to the haloed
/// extent. Used by `TileEditPipeline`'s geometry head to resample the source for
/// one output tile (geometry applied at the head; spec §8.4).
pub fn geometry_tile_uniform(
    op: Option<Geometry>,
    src_w: u32,
    src_h: u32,
    out_origin: (f32, f32),
    ext: u32,
) -> GeometryUniform {
    let (base, _, _) = geometry_uniform(op, src_w, src_h);
    GeometryUniform {
        out_dims: [ext as f32, ext as f32],
        out_origin: [out_origin.0, out_origin.1],
        ..base
    }
}

/// WGSL `mat3x3<f32>` uniform for a 3×3 color transform. Column-major with each
/// column padded to 16 bytes (`[[f32; 4]; 3]`), matching WGSL layout rules.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ColorMatrixUniform {
    pub m: [[f32; 4]; 3],
}

/// Pack a **row-major** 3×3 (`m[row][col]`) into WGSL column-major padded columns
/// so that in-shader `M * v` equals the row-major `m · v`.
pub fn pack_mat3(m: [[f32; 3]; 3]) -> [[f32; 4]; 3] {
    [
        [m[0][0], m[1][0], m[2][0], 0.0],
        [m[0][1], m[1][1], m[2][1], 0.0],
        [m[0][2], m[1][2], m[2][2], 0.0],
    ]
}

/// Build the color-matrix uniform from a row-major camera→working (or any) 3×3.
pub fn color_matrix_uniform(m: [[f32; 3]; 3]) -> ColorMatrixUniform {
    ColorMatrixUniform { m: pack_mat3(m) }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LensUniform {
    /// Distortion/TCA/vignette lerp factors (0 when the correction is disabled).
    pub dist_amount: f32,
    pub tca_amount: f32,
    pub vig_amount: f32,
    /// 1 when a real warp grid is bound; 0 = identity (skip the grid sample).
    pub use_warp: u32,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VignetteUniform {
    /// Lerp factor between identity (gain 1.0) and the LUT gain. 0 = identity.
    pub vig_amount: f32,
    /// Parametric manual (lens-free) vignette strength. Composed as
    /// `1.0 + manual * r * r` (r = normalized center→corner radius), so 0 is
    /// identity. Negative darkens corners; positive brightens them. Independent
    /// of `vig_amount` — see `corr(r) * manual(r)` in vignette.wgsl.
    pub manual: f32,
    /// Full OUTPUT-image dimensions (this LOD's pixel space). When both are > 0.5
    /// the shader takes the GLOBAL path (radius from the full-image center); when
    /// `[0.0, 0.0]` (the default sentinel, and the whole-image preview path) it
    /// falls back to per-texture `textureDimensions(src)`. This is what lets the
    /// TILED path compute a single seamless vignette instead of one per tile.
    pub full_dims: [f32; 2],
    /// This tile's haloed output-space top-left origin (added to `gid` before
    /// normalizing by `full_dims`). Zero on the whole-image path.
    pub origin: [f32; 2],
    pub pad: [f32; 2],
}

impl Default for VignetteUniform {
    /// Identity: `vig_amount = 0`, `manual = 0` → the pass multiplies rgb by 1.0.
    /// `full_dims = [0.0, 0.0]` is the "whole-image" sentinel (per-texture radius),
    /// so the default is byte-identical to the pre-tiling behavior.
    fn default() -> Self {
        Self {
            vig_amount: 0.0,
            manual: 0.0,
            full_dims: [0.0, 0.0],
            origin: [0.0, 0.0],
            pad: [0.0; 2],
        }
    }
}

/// Build the `LensUniform` (per-channel amounts + `use_warp` flag) for the
/// geometry pass from the op's `LensCorrection` and whether a real warp grid is
/// bound. A disabled correction contributes amount 0 (identity for that channel
/// group); `use_warp = 1` only when a grid is present, so with no grid the shader
/// takes the byte-identical no-correction path regardless of the amounts.
pub fn lens_uniform(lc: Option<&LensCorrection>, has_grid: bool) -> LensUniform {
    match lc {
        Some(l) => LensUniform {
            dist_amount: if l.distortion.enabled {
                l.distortion.amount
            } else {
                0.0
            },
            tca_amount: if l.tca.enabled { l.tca.amount } else { 0.0 },
            vig_amount: if l.vignetting.enabled {
                l.vignetting.amount
            } else {
                0.0
            },
            use_warp: if has_grid { 1 } else { 0 },
        },
        None => LensUniform {
            dist_amount: 0.0,
            tca_amount: 0.0,
            vig_amount: 0.0,
            use_warp: 0,
        },
    }
}

/// The vignette pass lerp amount from the op's `LensCorrection`. Zero (identity)
/// unless vignetting is enabled. The vignette pass is separate from the geometry
/// warp, so this drives `VignetteNode`, not the `LensUniform`.
pub fn vignette_amount(lc: Option<&LensCorrection>) -> f32 {
    match lc {
        Some(l) if l.vignetting.enabled => l.vignetting.amount,
        _ => 0.0,
    }
}

/// The geometric halo (px) a tiled lens-corrected pass over-fetches. Zero unless
/// distortion or TCA is enabled AND a grid is present.
pub fn lens_halo_px(lc: Option<&LensCorrection>, grid: Option<&WarpGrid>) -> u32 {
    match (lc, grid) {
        (Some(l), Some(g)) if l.distortion.enabled || l.tca.enabled => lens_halo(g),
        _ => 0,
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

/// Max hue rotation (degrees) per unit `AdjustmentSet::hue`. Local hue spans a
/// full turn at ±1 (pragmatic; image science secondary, like `wb_multipliers`).
pub const MAX_LOCAL_HUE_DEG: f32 = 180.0;

/// GPU uniform for `local_adjust.wgsl`. `#[repr(C)]`, 16-byte aligned. Field order +
/// padding MIRROR the WGSL `struct P` exactly. `mask_origin` lets the tile tier read
/// a sub-region of a full-output mask (preview leaves it `[0,0]`).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LocalAdjustUniform {
    pub exposure_gain: f32, // 2^exposure
    pub contrast_gain: f32, // 1 + contrast
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32, // 1 + saturation (mix factor)
    pub hue_deg: f32,    // hue * MAX_LOCAL_HUE_DEG
    pub wb_mul: [f32; 3],
    pub color_amount: f32,
    pub color_rgb: [f32; 3],
    pub contrast_pivot: f32,
    pub mask_origin: [i32; 2],
    pub mask_lod: i32, // tile mip level; mask sampled at (origin+xy) << mask_lod. 0 = whole-image/preview.
    pub _pad: i32,
}

/// `light_color_apply` (below) is still test-only; `local_adjust_uniform` is now
/// consumed by `LocalAdjustmentsNode`.
pub fn local_adjust_uniform(a: &crate::local::AdjustmentSet) -> LocalAdjustUniform {
    LocalAdjustUniform {
        exposure_gain: exposure_gain(a.exposure),
        contrast_gain: 1.0 + a.contrast,
        highlights: a.highlights,
        shadows: a.shadows,
        whites: a.whites,
        blacks: a.blacks,
        saturation: 1.0 + a.saturation,
        hue_deg: a.hue * MAX_LOCAL_HUE_DEG,
        wb_mul: wb_multipliers(a.temp, a.tint),
        color_amount: a.color.amount,
        color_rgb: [a.color.r, a.color.g, a.color.b],
        contrast_pivot: CONTRAST_PIVOT,
        mask_origin: [0, 0],
        mask_lod: 0,
        _pad: 0,
    }
}

fn luma709(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn rgb_to_hsl(c: [f32; 3]) -> [f32; 3] {
    let (r, g, b) = (c[0], c[1], c[2]);
    let mx = r.max(g.max(b));
    let mn = r.min(g.min(b));
    let l = (mx + mn) * 0.5;
    let d = mx - mn;
    if d <= 1e-6 {
        return [0.0, 0.0, l];
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let mut h = if mx == r {
        ((g - b) / d) % 6.0
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    h *= 60.0;
    if h < 0.0 {
        h += 360.0;
    }
    [h, s, l]
}
fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let (h, s, l) = (hsl[0] / 360.0, hsl[1], hsl[2]);
    if s <= 1e-6 {
        return [l, l, l];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue = |t_in: f32| -> f32 {
        let mut t = t_in;
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 1.0 / 2.0 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0)]
}

/// CPU reference for the Light+Color point op. `local_adjust.wgsl` mirrors this
/// exactly (golden tolerance absorbs f16/driver drift). Order: exposure → tonal
/// region gains → contrast → wb → saturation → hue → color swatch. Output clamped ≥0.
#[allow(dead_code)]
pub fn light_color_apply(rgb: [f32; 3], a: &crate::local::AdjustmentSet) -> [f32; 3] {
    let u = local_adjust_uniform(a);
    let mut c = [
        rgb[0] * u.exposure_gain,
        rgb[1] * u.exposure_gain,
        rgb[2] * u.exposure_gain,
    ];
    let y = luma709(c);
    let hi = smoothstep(0.5, 1.0, y);
    let sh = 1.0 - smoothstep(0.0, 0.5, y);
    let wh = smoothstep(0.7, 1.0, y);
    let bl = 1.0 - smoothstep(0.0, 0.3, y);
    let region = (1.0 + u.highlights * hi)
        * (1.0 + u.shadows * sh)
        * (1.0 + u.whites * wh)
        * (1.0 + u.blacks * bl);
    for v in &mut c {
        *v *= region;
    }
    for v in &mut c {
        *v = (*v - u.contrast_pivot) * u.contrast_gain + u.contrast_pivot;
    }
    for (v, m) in c.iter_mut().zip(u.wb_mul) {
        *v *= m;
    }
    let y2 = luma709(c);
    for v in &mut c {
        *v = y2 + (*v - y2) * u.saturation;
    }
    if u.hue_deg != 0.0 {
        let mut hsl = rgb_to_hsl([c[0].max(0.0), c[1].max(0.0), c[2].max(0.0)]);
        hsl[0] = (hsl[0] + u.hue_deg).rem_euclid(360.0);
        c = hsl_to_rgb(hsl);
    }
    if u.color_amount != 0.0 {
        for (v, cr) in c.iter_mut().zip(u.color_rgb) {
            *v += (cr - *v) * u.color_amount;
        }
    }
    [c[0].max(0.0), c[1].max(0.0), c[2].max(0.0)]
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
        let lut = curve_lut(&[(0.0, 0.0), (1.0, 1.0)], crate::op::CurveMode::Linear);
        assert!((lut[0] - 0.0).abs() < 1e-6);
        assert!((lut[255] - 1.0).abs() < 1e-6);
        assert!((lut[128] - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn curve_lut_empty_points_is_identity() {
        let lut = curve_lut(&[], crate::op::CurveMode::Linear);
        assert!((lut[64] - 64.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn curve_lut_pulls_midtones_down() {
        // A point below the diagonal at x=0.5 darkens the midtones.
        let lut = curve_lut(
            &[(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)],
            crate::op::CurveMode::Linear,
        );
        assert!(lut[128] < 128.0 / 255.0, "midpoint pulled below diagonal");
        assert!((lut[0] - 0.0).abs() < 1e-6);
        assert!((lut[255] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn curve_lut_is_monotone_non_decreasing() {
        // A non-monotone control set must still produce a non-decreasing LUT.
        let lut = curve_lut(
            &[(0.0, 0.0), (0.5, 0.8), (1.0, 0.2)],
            crate::op::CurveMode::Linear,
        );
        for i in 1..256 {
            assert!(lut[i] >= lut[i - 1], "lut dipped at {i}");
        }
    }

    #[test]
    fn linear_mode_matches_legacy_lut() {
        // Linear must reproduce the pre-feature piecewise-linear LUT exactly.
        let pts = [(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)];
        let lut = curve_lut(&pts, crate::op::CurveMode::Linear);
        // midpoint pulled below diagonal, endpoints pinned (same asserts as the old test)
        assert!(lut[128] < 128.0 / 255.0);
        assert!((lut[0] - 0.0).abs() < 1e-6);
        assert!((lut[255] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn smooth_passes_through_control_points() {
        // At a control point's x, the smooth LUT equals its y (within LUT quantization).
        let pts = [(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)];
        let lut = curve_lut(&pts, crate::op::CurveMode::Smooth);
        let idx = (0.5f32 * 255.0).round() as usize; // x = 0.5
        assert!(
            (lut[idx] - 0.25).abs() < 0.02,
            "smooth LUT hits the control point"
        );
    }

    #[test]
    fn smooth_is_monotonic_and_no_overshoot() {
        let pts = [(0.0, 0.0), (0.3, 0.7), (0.7, 0.72), (1.0, 1.0)]; // steep then flat — classic overshoot trap
        let lut = curve_lut(&pts, crate::op::CurveMode::Smooth);
        for i in 1..256 {
            assert!(
                lut[i] >= lut[i - 1] - 1e-6,
                "monotonic non-decreasing at {i}"
            );
            assert!(
                (0.0..=1.0).contains(&lut[i]),
                "no overshoot outside [0,1] at {i}"
            );
        }
        // No overshoot above the local max (0.72) in the flat middle region: sample x≈0.5
        let mid = (0.5f32 * 255.0).round() as usize;
        assert!(
            lut[mid] <= 0.72 + 1e-3,
            "monotone cubic must not bulge above neighboring control y"
        );
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

    #[test]
    fn geometry_uniform_default_out_origin_is_zero() {
        let (u, _, _) = geometry_uniform(None, 64, 48);
        assert_eq!(u.out_origin, [0.0, 0.0]);
    }

    #[test]
    fn geometry_tile_uniform_sets_origin_and_extent() {
        // Identity geometry, source 600x500, tile origin (254, -2), extent 260.
        let u = geometry_tile_uniform(None, 600, 500, (254.0, -2.0), 260);
        assert_eq!(u.out_origin, [254.0, -2.0]);
        assert_eq!(u.out_dims, [260.0, 260.0]);
        // Identity transform + source dims preserved.
        assert_eq!(u.m, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(u.src_dims, [600.0, 500.0]);
    }

    #[test]
    fn pack_mat3_identity_columns() {
        // Row-major identity packs to WGSL column-major identity (last lane = 0 pad).
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        assert_eq!(
            pack_mat3(id),
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0]
            ]
        );
    }

    #[test]
    fn pack_mat3_transposes_into_columns() {
        // Row-major m[row][col]; WGSL column c = (m[0][c], m[1][c], m[2][c], 0).
        let m = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        assert_eq!(
            pack_mat3(m),
            [
                [1.0, 4.0, 7.0, 0.0],
                [2.0, 5.0, 8.0, 0.0],
                [3.0, 6.0, 9.0, 0.0]
            ]
        );
    }

    #[test]
    fn color_matrix_uniform_wraps_packed_mat() {
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        assert_eq!(color_matrix_uniform(id).m, pack_mat3(id));
    }

    #[test]
    fn lens_uniform_is_16_byte_aligned() {
        assert_eq!(std::mem::size_of::<LensUniform>() % 16, 0);
    }

    #[test]
    fn vignette_uniform_default_is_identity_and_aligned() {
        assert_eq!(VignetteUniform::default().vig_amount, 0.0);
        assert_eq!(VignetteUniform::default().manual, 0.0);
        // `full_dims = [0,0]` is the whole-image sentinel: the shader falls back to
        // per-texture dims, keeping the preview + all existing goldens byte-identical.
        assert_eq!(VignetteUniform::default().full_dims, [0.0, 0.0]);
        assert_eq!(VignetteUniform::default().origin, [0.0, 0.0]);
        // 32 bytes, still 16-aligned (matches the 8-scalar WGSL `struct V`).
        assert_eq!(std::mem::size_of::<VignetteUniform>(), 32);
        assert_eq!(std::mem::size_of::<VignetteUniform>() % 16, 0);
    }

    #[test]
    fn lens_halo_zero_when_disabled_or_absent() {
        assert_eq!(lens_halo_px(None, None), 0);
        let lc = crate::op::LensCorrection {
            lens_id: Some("x".into()),
            focal_len: 24.0,
            aperture: 8.0,
            crop_factor: 1.0,
            distortion: crate::op::Correction {
                enabled: false,
                amount: 1.0,
            },
            tca: crate::op::Correction::default(),
            vignetting: crate::op::Correction::default(),
        };
        // Distortion disabled → no geometric halo even if a grid exists.
        let g = ferrolite_lens::WarpGrid {
            n: 2,
            coords: vec![[0.0; 6]; 4],
            max_disp: 30.0,
        };
        assert_eq!(lens_halo_px(Some(&lc), Some(&g)), 0);
        let lc_on = crate::op::LensCorrection {
            distortion: crate::op::Correction {
                enabled: true,
                amount: 1.0,
            },
            ..lc
        };
        assert_eq!(lens_halo_px(Some(&lc_on), Some(&g)), 30);
    }

    #[test]
    fn light_color_identity_is_a_no_op() {
        use crate::local::AdjustmentSet;
        let c = light_color_apply([0.4, 0.5, 0.6], &AdjustmentSet::default());
        assert!(
            (c[0] - 0.4).abs() < 1e-6 && (c[1] - 0.5).abs() < 1e-6 && (c[2] - 0.6).abs() < 1e-6
        );
    }

    #[test]
    fn light_color_exposure_plus_one_doubles() {
        use crate::local::AdjustmentSet;
        let c = light_color_apply(
            [0.2, 0.2, 0.2],
            &AdjustmentSet {
                exposure: 1.0,
                ..Default::default()
            },
        );
        assert!((c[0] - 0.4).abs() < 1e-4, "got {}", c[0]);
    }

    #[test]
    fn light_color_contrast_pushes_away_from_pivot() {
        use crate::local::AdjustmentSet;
        // A value above the 0.18 pivot moves further up under positive contrast.
        let c = light_color_apply(
            [0.5, 0.5, 0.5],
            &AdjustmentSet {
                contrast: 0.5,
                ..Default::default()
            },
        );
        assert!(c[0] > 0.5, "above-pivot value brightened: {}", c[0]);
    }

    #[test]
    fn light_color_full_desaturation_goes_grey() {
        use crate::local::AdjustmentSet;
        let c = light_color_apply(
            [0.9, 0.1, 0.1],
            &AdjustmentSet {
                saturation: -1.0,
                ..Default::default()
            },
        );
        assert!(
            (c[0] - c[1]).abs() < 1e-4 && (c[1] - c[2]).abs() < 1e-4,
            "grey: {c:?}"
        );
    }

    #[test]
    fn light_color_warm_temp_raises_red_over_blue() {
        use crate::local::AdjustmentSet;
        let c = light_color_apply(
            [0.5, 0.5, 0.5],
            &AdjustmentSet {
                temp: 0.8,
                ..Default::default()
            },
        );
        assert!(c[0] > c[2], "warm temp: r={} b={}", c[0], c[2]);
    }

    #[test]
    fn local_adjust_uniform_is_identity_when_default() {
        use crate::local::AdjustmentSet;
        let u = local_adjust_uniform(&AdjustmentSet::default());
        assert_eq!(u.exposure_gain, 1.0);
        assert_eq!(u.contrast_gain, 1.0);
        assert_eq!(u.wb_mul, [1.0, 1.0, 1.0]);
        assert_eq!(std::mem::size_of::<LocalAdjustUniform>() % 16, 0);
    }

    #[test]
    fn reserved_fields_do_not_change_output() {
        use crate::local::AdjustmentSet;
        let a = AdjustmentSet {
            texture: 1.0,
            clarity: 1.0,
            dehaze: 1.0,
            sharpness: 1.0,
            noise: 1.0,
            ..Default::default()
        };
        assert_eq!(light_color_apply([0.3, 0.4, 0.5], &a), [0.3, 0.4, 0.5]);
    }
}
