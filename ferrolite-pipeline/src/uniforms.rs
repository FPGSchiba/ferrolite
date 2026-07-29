//! Pure CPU math turning UI op params into GPU shader uniforms, plus the
//! `#[repr(C)]` Pod uniform structs (layouts MIRROR the WGSL `struct P` in each
//! shader). Display-linear space; the sRGB OETF lives only in the display/blit
//! shader. No GPU here — fully unit-tested.

use crate::op::{Aspect, CropRect, Geometry, Hsl, LensCorrection, Sharpen};
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

/// Maximum tonal shift (in display-linear `[0,1]`) applied by a single region at
/// its extreme (region value ±1). Pragmatic constant (image science secondary,
/// same spirit as `wb_multipliers`); a region of +1 lifts its band by ~0.25.
pub const MAX_PARAMETRIC_SHIFT: f32 = 0.25;

/// Partition-of-unity weights for the four parametric regions
/// [shadows, darks, lights, highlights] at sample `x`, given the four region
/// centers (ascending, but adjacent centers may be equal after sanitizing
/// degenerate splits). Weights sum to 1 everywhere; adjacent centers with
/// positive width transition smoothly (smoothstep), a zero-width (collapsed)
/// pair transitions as a hard split instead, and it is flat past the end
/// centers.
fn region_weights(x: f32, centers: [f32; 4]) -> [f32; 4] {
    let mut w = [0.0f32; 4];
    if x <= centers[0] {
        w[0] = 1.0;
        return w;
    }
    if x >= centers[3] {
        w[3] = 1.0;
        return w;
    }
    for k in 0..3 {
        if x >= centers[k] && x <= centers[k + 1] {
            // A zero-width band (centers[k] == centers[k+1], e.g. from a
            // sanitized/collapsed split) would make smoothstep divide by
            // zero and produce NaN, which `clamp` does NOT turn into 0/1.
            // Guard it explicitly as a hard split instead of relying on
            // smoothstep: everything at or past a collapsed center belongs
            // to the later region. Bands with positive width still get the
            // smooth transition.
            if (centers[k + 1] - centers[k]).abs() < f32::EPSILON {
                w[k + 1] = 1.0;
                return w;
            }
            let t = smoothstep(centers[k], centers[k + 1], x);
            w[k] = 1.0 - t;
            w[k + 1] = t;
            return w;
        }
    }
    // Unreachable given ascending centers and the two early returns above:
    // every x either hits an early return or falls into exactly one [k,k+1]
    // window in the loop. Kept as a defensive fallback, not a live path.
    w[3] = 1.0;
    w
}

/// Bake a parametric region curve into a 256-entry display-linear LUT. Each
/// sample is offset by the weighted sum of the four region shifts, then the
/// result is clamped to `[0,1]` and forced monotone non-decreasing (mirroring
/// `curve_lut`). All-zero regions → the identity ramp. Pure — no GPU.
pub fn parametric_curve_lut(p: &crate::op::ParametricCurve) -> [f32; 256] {
    // Sanitize splits into ascending order in [0,1] so a user-dragged
    // out-of-order set can't produce non-ascending centers.
    let s1 = p.shadow_split.clamp(0.0, 1.0);
    let s2 = p.midtone_split.clamp(0.0, 1.0).max(s1);
    let s3 = p.highlight_split.clamp(0.0, 1.0).max(s2);
    let centers = [s1 * 0.5, (s1 + s2) * 0.5, (s2 + s3) * 0.5, (s3 + 1.0) * 0.5];
    let region = [p.shadows, p.darks, p.lights, p.highlights];

    let mut lut = [0.0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let x = i as f32 / 255.0;
        let w = region_weights(x, centers);
        let shift = MAX_PARAMETRIC_SHIFT
            * (region[0] * w[0] + region[1] * w[1] + region[2] * w[2] + region[3] * w[3]);
        *slot = (x + shift).clamp(0.0, 1.0);
    }
    for i in 1..256 {
        if lut[i] < lut[i - 1] {
            lut[i] = lut[i - 1];
        }
    }
    lut
}

/// Sample a 256-entry LUT at a continuous input `v`, mirroring
/// `tone_curve.wgsl`'s `apply_lut`: linear interpolation inside `[0,1]`, and
/// unit-slope extrapolation from the endpoints outside it (so an identity LUT is
/// exact pass-through). Kept in lock-step with the shader.
fn sample_lut(lut: &[f32; 256], v: f32) -> f32 {
    if v < 0.0 {
        return lut[0] + v;
    }
    if v > 1.0 {
        return lut[255] + (v - 1.0);
    }
    let x = v * 255.0;
    let i0 = x.floor() as usize;
    let i1 = (i0 + 1).min(255);
    let f = x - x.floor();
    lut[i0] * (1.0 - f) + lut[i1] * f
}

/// Compose two LUTs: `result[i] = sample_lut(outer, inner[i])` — i.e. apply
/// `inner` first, then `outer` (function composition `outer ∘ inner`).
fn compose_lut(inner: &[f32; 256], outer: &[f32; 256]) -> [f32; 256] {
    let mut out = [0.0f32; 256];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = sample_lut(outer, inner[i]);
    }
    out
}

/// Bake the three per-channel final tone-curve LUTs:
/// `finalₖ(x) = channelₖ( master( parametric(x) ) )` for k ∈ {R,G,B}.
/// Returns `[R, G, B]` rows. `None` (or a fully-identity curve) yields three
/// identity ramps. Pure — no GPU; the reusable transform per design §2.5.
pub fn tone_curve_luts(tc: Option<&crate::op::ToneCurve>) -> [[f32; 256]; 3] {
    let default = crate::op::ToneCurve::default();
    let tc = tc.unwrap_or(&default);
    let param = parametric_curve_lut(&tc.parametric);
    let master = curve_lut(&tc.points, tc.mode);
    let base = compose_lut(&param, &master); // master ∘ parametric
    let r = compose_lut(&base, &curve_lut(&tc.red.points, tc.red.mode));
    let g = compose_lut(&base, &curve_lut(&tc.green.points, tc.green.mode));
    let b = compose_lut(&base, &curve_lut(&tc.blue.points, tc.blue.mode));
    [r, g, b]
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

/// Max hue rotation (degrees) per unit `AdjustmentSet::hue`. Local hue spans a
/// full turn at ±1 (pragmatic; image science secondary, like `wb_multipliers`).
pub const MAX_LOCAL_HUE_DEG: f32 = 180.0;

/// GPU uniform for `local_adjust.wgsl`. `#[repr(C)]`, 16-byte aligned. Field order +
/// padding MIRROR the WGSL `struct P` exactly. The mask is composited at the SAME
/// resolution as this pass's input (whole image for preview, one tile for the tiled
/// tier), so the apply pass samples it 1:1 — no per-tile origin/LOD offset.
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
    // ── Phase 2b: per-layer curve/HSL/grade (identity when the layer leaves them default) ──
    /// 8 bands × (hue, sat, lum, pad) — same packing as the global `HslUniform`.
    pub hsl_bands: [[f32; 4]; 8],
    /// Same packing as the global `ColorGradeUniform` (shadows/midtones/highlights/global/params).
    pub grade_shadows: [f32; 4],
    pub grade_midtones: [f32; 4],
    pub grade_highlights: [f32; 4],
    pub grade_global: [f32; 4],
    pub grade_params: [f32; 4],
    /// x = curve active (LUT differs from the linear ramp), y = hsl active,
    /// z = grade active, w = pad. Skip flags so identity layers pay no extra math.
    pub active_flags: [f32; 4],
    // ── Phase 3 (fused layer engine): stage order + coverage flags + vibrance.
    /// x: 1.0 = global order (WB applied before contrast), 0.0 = mask order
    /// (contrast before WB — the historical/existing per-mask order, kept as
    /// the default so an untouched call site stays byte-identical). `x` also
    /// gates the mask-path floor clamp at the end of `adjust()`/
    /// `light_color_apply` (clamped when `x == 0`, i.e. mask order; skipped
    /// when `x != 0`, i.e. global order — see the comment at that clamp for
    /// why). y: 1.0 =
    /// force full coverage (the shader skips the mask sample entirely and uses
    /// m = 1.0 — NOT merely a post-hoc override, so an out-of-bounds/degenerate
    /// mask texture bound for a global pseudo-layer is never read). z: vibrance
    /// amount (rides in this vec4 rather than growing the struct again). w: pad.
    pub order_and_coverage: [f32; 4],
}

/// `light_color_apply` (below) is still test-only; `local_adjust_uniform` is now
/// consumed by `LocalAdjustmentsNode`. `global_order` selects the WB↔contrast
/// application order (true = global/light-engine order: WB before contrast;
/// false = mask order: contrast before WB — the pre-Phase-3 default, so mask
/// layers stay byte-identical). `full_coverage` forces the shader's mask sample
/// to 1.0 (used by the global pseudo-layers, which composite no mask at all).
pub fn local_adjust_uniform(
    a: &crate::local::AdjustmentSet,
    global_order: bool,
    full_coverage: bool,
) -> LocalAdjustUniform {
    let hsl_bands = hsl_uniform(Some(a.hsl)).bands;
    let grade = color_grade_uniform(Some(a.color_grade));
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
        hsl_bands,
        grade_shadows: grade.shadows,
        grade_midtones: grade.midtones,
        grade_highlights: grade.highlights,
        grade_global: grade.global,
        grade_params: grade.params,
        active_flags: [
            if !a.tone_curve.is_identity() {
                1.0
            } else {
                0.0
            },
            if !a.hsl.is_identity() { 1.0 } else { 0.0 },
            if !a.color_grade.is_identity() {
                1.0
            } else {
                0.0
            },
            0.0,
        ],
        order_and_coverage: [
            if global_order { 1.0 } else { 0.0 },
            if full_coverage { 1.0 } else { 0.0 },
            a.vibrance,
            0.0,
        ],
    }
}

/// Thin wrapper over `tone_curve_luts` giving `LocalAdjustmentsNode` one named
/// entry point for a mask layer's baked per-channel curve LUTs.
pub fn local_layer_lut(a: &crate::local::AdjustmentSet) -> [[f32; 256]; 3] {
    tone_curve_luts(Some(&a.tone_curve))
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

/// Band centers (degrees) for the 8-band HSL split — red, orange, yellow,
/// green, aqua, blue, purple, magenta. Mirrors `hsl.wgsl`'s `band_center`.
const HSL_BAND_CENTERS: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0];
/// Max hue rotation (degrees) at a band's full weight. Mirrors `hsl.wgsl`'s `MAX_HUE_SHIFT`.
const HSL_MAX_HUE_SHIFT: f32 = 30.0;

/// Triangular falloff weight for `hue` relative to a band `center`, wrapping across
/// the 0/360 seam. Mirrors `hsl.wgsl`'s `band_weight` exactly (same constant, 60°).
fn hsl_band_weight(hue: f32, center: f32) -> f32 {
    let mut d = (hue - center).abs();
    if d > 180.0 {
        d = 360.0 - d;
    }
    (1.0 - d / 60.0).max(0.0)
}

/// CPU reference for the 8-band HSL pass (`hsl.wgsl`). `bands` uses the same
/// `[hue, sat, lum, pad]` packing as `HslUniform`. Out-of-`[0,1]` channels bypass
/// the HSL round-trip additively (P2 §5.3) exactly like the shader: only the
/// in-gamut part is adjusted and the excess is re-added.
fn hsl_bands_apply(c: [f32; 3], bands: &[[f32; 4]; 8]) -> [f32; 3] {
    let in_gamut = [
        c[0].clamp(0.0, 1.0),
        c[1].clamp(0.0, 1.0),
        c[2].clamp(0.0, 1.0),
    ];
    let excess = [c[0] - in_gamut[0], c[1] - in_gamut[1], c[2] - in_gamut[2]];
    let hsl = rgb_to_hsl(in_gamut);

    let mut hue_acc = 0.0f32;
    let mut sat_acc = 0.0f32;
    let mut lum_acc = 0.0f32;
    for (i, band) in bands.iter().enumerate() {
        let w = hsl_band_weight(hsl[0], HSL_BAND_CENTERS[i]);
        hue_acc += w * band[0];
        sat_acc += w * band[1];
        lum_acc += w * band[2];
    }

    let mut out_hue = hsl[0] + hue_acc * HSL_MAX_HUE_SHIFT;
    if out_hue < 0.0 {
        out_hue += 360.0;
    }
    if out_hue >= 360.0 {
        out_hue -= 360.0;
    }
    let out_hsl = [
        out_hue,
        (hsl[1] * (1.0 + sat_acc)).clamp(0.0, 1.0),
        (hsl[2] * (1.0 + lum_acc)).clamp(0.0, 1.0),
    ];

    let rgb = hsl_to_rgb(out_hsl);
    [rgb[0] + excess[0], rgb[1] + excess[1], rgb[2] + excess[2]]
}

/// CPU reference for the Light+Color point op. `local_adjust.wgsl` mirrors this
/// exactly (golden tolerance absorbs f16/driver drift). Order: exposure → tonal
/// region gains → {contrast, wb} in the order `global_order` selects → saturation
/// → hue → vibrance → curve/HSL/grade → color swatch. Output clamped ≥0.
///
/// `global_order`: true = the light-engine/global-pseudo-layer order (WB before
/// contrast); false = the per-mask order (contrast before WB — the historical
/// default, unchanged by Phase 3). The two do not commute, so each call site
/// must pass the flag matching its stage (see `local_node.rs`).
#[allow(dead_code)]
pub fn light_color_apply(
    rgb: [f32; 3],
    a: &crate::local::AdjustmentSet,
    global_order: bool,
) -> [f32; 3] {
    let u = local_adjust_uniform(a, global_order, false);
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
    if global_order {
        // Global order: WB before contrast.
        for (v, m) in c.iter_mut().zip(u.wb_mul) {
            *v *= m;
        }
        for v in &mut c {
            *v = (*v - u.contrast_pivot) * u.contrast_gain + u.contrast_pivot;
        }
    } else {
        // Mask order (historical, unchanged): contrast before WB.
        for v in &mut c {
            *v = (*v - u.contrast_pivot) * u.contrast_gain + u.contrast_pivot;
        }
        for (v, m) in c.iter_mut().zip(u.wb_mul) {
            *v *= m;
        }
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
    // Vibrance (Phase 3, new — both scopes): a saturation boost that fades as a
    // pixel approaches full saturation; `s' = s * (1 + vibrance * (1 - w))`
    // where `w = clamp(s, 0, 1)`. Scene-linear pixels can have HSL saturation
    // s >> 1 (e.g. a strongly-graded highlight); weighting the fade term by
    // the CLAMPED saturation (not the raw one) keeps `(1 - w)` in [0,1] so the
    // factor never goes hugely negative and snaps such a pixel to grey (or, for
    // negative vibrance, boosts it). For s in [0,1] (the common case) w == s
    // and the formula is unchanged. Only the lower bound is clamped (0.0) —
    // s > 1 is a legitimate scene-linear state and must not be clipped down to
    // 1. Slots after hue, before the tone curve. Gated on non-zero (like the
    // hue step above) so a zero-vibrance AdjustmentSet stays bit-exact through
    // the rgb2hsl/hsl2rgb round trip. Keep in lockstep with the WGSL vibrance
    // branch in `shaders/local_adjust.wgsl`.
    if a.vibrance != 0.0 {
        let mut hsl = rgb_to_hsl([c[0].max(0.0), c[1].max(0.0), c[2].max(0.0)]);
        let w = hsl[1].clamp(0.0, 1.0);
        hsl[1] = (hsl[1] * (1.0 + a.vibrance * (1.0 - w))).max(0.0);
        c = hsl_to_rgb(hsl);
    }
    // ── Phase 2b: per-layer curve → HSL bands → grade, mirroring the WGSL Task 2
    // will add. Each step is gated by its identity flag (not just internal
    // early-outs) so a fully-default AdjustmentSet takes none of these branches —
    // required for `light_color_identity_is_a_no_op` to stay bit-exact, since
    // linear-interp LUT sampling of the identity ramp is not guaranteed to be
    // bit-identical to its input under float rounding.
    if !a.tone_curve.is_identity() {
        let luts = tone_curve_luts(Some(&a.tone_curve));
        for (v, lut) in c.iter_mut().zip(luts.iter()) {
            *v = sample_lut(lut, *v);
        }
    }
    if !a.hsl.is_identity() {
        c = hsl_bands_apply(c, &u.hsl_bands);
    }
    if !a.color_grade.is_identity() {
        c = color_grade_px(c, &a.color_grade);
    }
    if u.color_amount != 0.0 {
        for (v, cr) in c.iter_mut().zip(u.color_rgb) {
            *v += (cr - *v) * u.color_amount;
        }
    }
    // Phase 3 (Task 3 parity fix): mirrors `local_adjust.wgsl`'s `adjust()` —
    // the floor clamp is per-mask (mask-order) behavior only; the global-order
    // (pseudo-layer) path must NOT clamp, or the committed `light_trio`/
    // `curve_hsl_grade`/`wb_contrast_both` parity goldens (which legitimately
    // carry pixels down to ~-0.09 in scene-linear space) regress. See the WGSL
    // comment at the same spot for the full rationale.
    if global_order {
        c
    } else {
        [c[0].max(0.0), c[1].max(0.0), c[2].max(0.0)]
    }
}

/// Chroma strength added per unit (sat × weight). Pragmatic constant (image
/// science secondary, like `wb_multipliers`); sat 1 in a region adds ~0.5 chroma.
pub const GRADE_TINT_STRENGTH: f32 = 0.5;
/// Luminance offset strength added per unit (lum × weight).
pub const GRADE_LUM_STRENGTH: f32 = 0.5;

/// HSV → linear RGB (h in degrees, s/v in [0,1]). Standard sextant conversion.
fn hsv_to_rgb(h_deg: f32, s: f32, v: f32) -> [f32; 3] {
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
    [r + m, g + m, b + m]
}

/// The zero-luminance chroma vector for a wheel (hue, sat): the wheel color's
/// hue-sat direction with its luminance removed, so adding it tints without a
/// net brightness shift. Zero at `sat == 0` (identity).
fn grade_tint(hue: f32, sat: f32) -> [f32; 3] {
    let s = sat.clamp(0.0, 1.0);
    if s <= 0.0 {
        return [0.0, 0.0, 0.0];
    }
    let c = hsv_to_rgb(hue, s, 1.0);
    let y = luma709(c);
    [c[0] - y, c[1] - y, c[2] - y]
}

/// Region weights (shadow, midtone, highlight) for pixel luminance `y`, shaped by
/// `blending` (overlap width, [0,1]) and `balance` (shifts the shadow↔highlight
/// midpoint, [-1,1]). Highlight rises with `y`; shadow is its complement; midtone
/// is a bump peaking at the pivot. Not a strict partition (regions overlap, as in
/// LR grading); the WGSL kernel mirrors this exactly.
fn grade_region_weights(y: f32, blending: f32, balance: f32) -> (f32, f32, f32) {
    let pivot = 0.5 + 0.5 * balance.clamp(-1.0, 1.0);
    let width = 0.15 + 0.35 * blending.clamp(0.0, 1.0);
    let w_hi = smoothstep(pivot - width, pivot + width, y);
    let w_sh = 1.0 - w_hi;
    let w_mid = 4.0 * w_sh * w_hi;
    (w_sh, w_mid, w_hi)
}

/// Pure per-pixel color grade — the reusable transform (design §2.5) and the
/// `color_grade.wgsl` kernel's reference. Adds each region's tint (weighted by
/// its luminance region) plus the region's luminance offset; the Global wheel
/// applies uniformly. Identity when all wheels are neutral. Not clamped (out-of-
/// range values pass through, honoring P2 §5.3; display clamps later).
pub fn color_grade_px(rgb: [f32; 3], cg: &crate::op::ColorGrade) -> [f32; 3] {
    let y = luma709(rgb);
    let (w_sh, w_mid, w_hi) = grade_region_weights(y, cg.blending, cg.balance);
    let t_sh = grade_tint(cg.shadows.hue, cg.shadows.sat);
    let t_mid = grade_tint(cg.midtones.hue, cg.midtones.sat);
    let t_hi = grade_tint(cg.highlights.hue, cg.highlights.sat);
    let t_gl = grade_tint(cg.global.hue, cg.global.sat);
    let lum = GRADE_LUM_STRENGTH
        * (w_sh * cg.shadows.lum
            + w_mid * cg.midtones.lum
            + w_hi * cg.highlights.lum
            + cg.global.lum);
    let mut out = [0.0f32; 3];
    for (c, slot) in out.iter_mut().enumerate() {
        let tint = w_sh * t_sh[c] + w_mid * t_mid[c] + w_hi * t_hi[c] + t_gl[c];
        *slot = rgb[c] + GRADE_TINT_STRENGTH * tint + lum;
    }
    out
}

/// GPU uniform for `color_grade.wgsl`. `#[repr(C)]`, 16-byte aligned; field order
/// MIRRORS the WGSL `struct P`. Each wheel row is `[tint_r, tint_g, tint_b, lum]`
/// with tint pre-scaled by `GRADE_TINT_STRENGTH` and lum by `GRADE_LUM_STRENGTH`,
/// so the shader adds them directly (no magic constants in WGSL). `params` =
/// `[blending, balance, 0, 0]`.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ColorGradeUniform {
    pub shadows: [f32; 4],
    pub midtones: [f32; 4],
    pub highlights: [f32; 4],
    pub global: [f32; 4],
    pub params: [f32; 4],
}

pub fn color_grade_uniform(op: Option<crate::op::ColorGrade>) -> ColorGradeUniform {
    let cg = op.unwrap_or_default();
    let pack = |w: &crate::op::GradeWheel| {
        let t = grade_tint(w.hue, w.sat);
        [
            t[0] * GRADE_TINT_STRENGTH,
            t[1] * GRADE_TINT_STRENGTH,
            t[2] * GRADE_TINT_STRENGTH,
            w.lum * GRADE_LUM_STRENGTH,
        ]
    };
    ColorGradeUniform {
        shadows: pack(&cg.shadows),
        midtones: pack(&cg.midtones),
        highlights: pack(&cg.highlights),
        global: pack(&cg.global),
        params: [cg.blending, cg.balance, 0.0, 0.0],
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
        let c = light_color_apply([0.4, 0.5, 0.6], &AdjustmentSet::default(), false);
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
            false,
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
            false,
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
            false,
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
            false,
        );
        assert!(c[0] > c[2], "warm temp: r={} b={}", c[0], c[2]);
    }

    #[test]
    fn local_adjust_uniform_is_identity_when_default() {
        use crate::local::AdjustmentSet;
        let u = local_adjust_uniform(&AdjustmentSet::default(), false, false);
        assert_eq!(u.exposure_gain, 1.0);
        assert_eq!(u.contrast_gain, 1.0);
        assert_eq!(u.wb_mul, [1.0, 1.0, 1.0]);
        assert_eq!(u.order_and_coverage, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(std::mem::size_of::<LocalAdjustUniform>() % 16, 0);
    }

    #[test]
    fn extended_local_uniform_is_identity_safe() {
        use crate::local::AdjustmentSet;
        let a = AdjustmentSet::default();
        let u = local_adjust_uniform(&a, false, false);
        assert_eq!(u.active_flags, [0.0; 4]);
        assert_eq!(u.hsl_bands, [[0.0; 4]; 8]);
        // Identity LUT is the linear ramp.
        let luts = local_layer_lut(&a);
        assert!((luts[0][0] - 0.0).abs() < 1e-6);
        assert!((luts[0][255] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn order_and_coverage_flags_pack_global_order_and_full_coverage() {
        use crate::local::AdjustmentSet;
        let a = AdjustmentSet {
            vibrance: 0.3,
            ..Default::default()
        };
        let u_mask = local_adjust_uniform(&a, false, false);
        assert_eq!(u_mask.order_and_coverage, [0.0, 0.0, 0.3, 0.0]);
        let u_global = local_adjust_uniform(&a, true, true);
        assert_eq!(u_global.order_and_coverage, [1.0, 1.0, 0.3, 0.0]);
    }

    /// `AdjustmentSet::light_segment()` and `color_segment()` must partition the
    /// point-op fields exactly once each: every Light-stage field appears ONLY
    /// in `light_segment`, every Color-stage field ONLY in `color_segment`, and
    /// fields belonging to NEITHER stage (sharpen/dehaze/NR/texture/clarity —
    /// Phase 4's territory) are identity in BOTH.
    #[test]
    fn light_and_color_segments_partition_the_set() {
        use crate::local::{AdjustmentSet, ColorSwatch, NoiseReduction};

        let a = AdjustmentSet {
            exposure: 0.1,
            contrast: 0.2,
            highlights: 0.3,
            shadows: 0.4,
            whites: 0.5,
            blacks: 0.6,
            temp: 0.7,
            tint: 0.8,
            saturation: 0.9,
            hue: 0.11,
            color: ColorSwatch {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                amount: 0.5,
            },
            vibrance: 0.12,
            tone_curve: crate::op::ToneCurve {
                points: vec![(0.0, 0.1), (1.0, 1.0)],
                ..Default::default()
            },
            hsl: {
                let mut h = crate::op::Hsl::default();
                h.bands[0].sat = 0.3;
                h
            },
            color_grade: {
                let mut g = crate::op::ColorGrade::default();
                g.shadows.sat = 0.4;
                g
            },
            sharpen: crate::op::Sharpen {
                amount: 0.5,
                radius: 2,
            },
            dehaze: crate::op::Dehaze {
                amount: 0.2,
                ..Default::default()
            },
            noise_reduction: NoiseReduction {
                luminance: 0.5,
                ..Default::default()
            },
            texture: 0.3,
            clarity: 0.4,
        };

        let light = a.light_segment();
        let color = a.color_segment();

        // Light-owned fields: present in `light`, identity in `color`.
        assert_eq!(light.exposure, 0.1);
        assert_eq!(color.exposure, 0.0);
        assert_eq!(light.contrast, 0.2);
        assert_eq!(color.contrast, 0.0);
        assert_eq!(light.highlights, 0.3);
        assert_eq!(color.highlights, 0.0);
        assert_eq!(light.shadows, 0.4);
        assert_eq!(color.shadows, 0.0);
        assert_eq!(light.whites, 0.5);
        assert_eq!(color.whites, 0.0);
        assert_eq!(light.blacks, 0.6);
        assert_eq!(color.blacks, 0.0);
        assert_eq!(light.temp, 0.7);
        assert_eq!(color.temp, 0.0);
        assert_eq!(light.tint, 0.8);
        assert_eq!(color.tint, 0.0);

        // Color-owned fields: present in `color`, identity in `light`.
        assert_eq!(color.saturation, 0.9);
        assert_eq!(light.saturation, 0.0);
        assert_eq!(color.hue, 0.11);
        assert_eq!(light.hue, 0.0);
        assert_eq!(color.vibrance, 0.12);
        assert_eq!(light.vibrance, 0.0);
        assert_eq!(color.color.amount, 0.5);
        assert_eq!(light.color.amount, 0.0);
        assert!(!color.tone_curve.is_identity());
        assert!(light.tone_curve.is_identity());
        assert!(!color.hsl.is_identity());
        assert!(light.hsl.is_identity());
        assert!(!color.color_grade.is_identity());
        assert!(light.color_grade.is_identity());

        // Fields owned by NEITHER stage: identity in both.
        assert_eq!(light.sharpen.amount, 0.0);
        assert_eq!(color.sharpen.amount, 0.0);
        assert!(light.dehaze.is_identity());
        assert!(color.dehaze.is_identity());
        assert_eq!(light.noise_reduction, NoiseReduction::default());
        assert_eq!(color.noise_reduction, NoiseReduction::default());
        assert_eq!(light.texture, 0.0);
        assert_eq!(color.texture, 0.0);
        assert_eq!(light.clarity, 0.0);
        assert_eq!(color.clarity, 0.0);
    }

    /// Order flag: with temp + contrast both set, the global order (WB before
    /// contrast) and the mask order (contrast before WB) must diverge, and the
    /// global-order result must equal a hand-composed "wb then pivot-contrast".
    #[test]
    fn cpu_reference_order_flag_swaps_wb_contrast() {
        use crate::local::AdjustmentSet;
        let a = AdjustmentSet {
            temp: 0.5,
            contrast: 0.5,
            ..Default::default()
        };
        let c = [0.5, 0.5, 0.5];
        let global = light_color_apply(c, &a, true);
        let mask = light_color_apply(c, &a, false);
        assert_ne!(
            global, mask,
            "order flag must change output when temp+contrast are both set"
        );

        // Hand-compose the global order: wb, then pivot-contrast (everything
        // else in `a` is identity, so no other step contributes).
        let mul = wb_multipliers(0.5, 0.0);
        let mut manual = [c[0] * mul[0], c[1] * mul[1], c[2] * mul[2]];
        for v in &mut manual {
            *v = (*v - CONTRAST_PIVOT) * 1.5 + CONTRAST_PIVOT;
        }
        for (g, m) in global.iter().zip(manual.iter()) {
            assert!(
                (g - m).abs() < 1e-5,
                "global order must equal wb-then-pivot-contrast: {g} vs {m}"
            );
        }
    }

    /// Vibrance: a low-saturation pixel must gain relatively more saturation
    /// than a high-saturation pixel at the same vibrance amount (the formula
    /// fades toward full saturation); vibrance 0 must stay bit-exact identity.
    #[test]
    fn vibrance_boosts_low_sat_more_than_high_sat() {
        use crate::local::AdjustmentSet;

        let low_sat = [0.55, 0.5, 0.45];
        let high_sat = [0.9, 0.1, 0.1];

        let a = AdjustmentSet {
            vibrance: 0.5,
            ..Default::default()
        };
        let low_before = rgb_to_hsl(low_sat)[1];
        let high_before = rgb_to_hsl(high_sat)[1];

        let low_after = rgb_to_hsl(light_color_apply(low_sat, &a, false))[1];
        let high_after = rgb_to_hsl(light_color_apply(high_sat, &a, false))[1];

        let low_gain = (low_after - low_before) / low_before.max(1e-6);
        let high_gain = (high_after - high_before) / high_before.max(1e-6);
        assert!(
            low_gain > high_gain,
            "low-sat pixel must gain more relative saturation: low={low_gain} high={high_gain}"
        );

        // Vibrance 0 is a no-op: the step is flag-gated (not merely a no-op
        // formula), so it never enters the rgb2hsl/hsl2rgb round trip. Compared
        // with the same tolerance as `light_color_identity_is_a_no_op` (the
        // pivot-contrast subtract/add at identity params is not itself
        // bit-exact under float rounding, independent of vibrance).
        let a0 = AdjustmentSet::default();
        let c = [0.3, 0.6, 0.2];
        let out = light_color_apply(c, &a0, false);
        for (o, want) in out.iter().zip(c.iter()) {
            assert!(
                (o - want).abs() < 1e-6,
                "vibrance 0 must be a no-op: {out:?}"
            );
        }
    }

    /// Scene-linear pixels can have HSL saturation `s >> 1` (e.g. this fixture:
    /// (1.74, 0.17, 0.17) has s ≈ 18.7). The pre-fix formula weighted the
    /// `(1 - s)` fade term by the RAW (unclamped) saturation, so at s ≈ 18.7 the
    /// factor `1 + v*(1-s)` went hugely negative for any non-zero vibrance,
    /// snapping the pixel to flat grey — and doing so WORSE (more grey) for
    /// negative vibrance, i.e. negative vibrance BOOSTED such a pixel's spread
    /// instead of reducing it. The fix weights the fade term by the clamped
    /// saturation `w = clamp(s, 0, 1)` instead, so `(1 - w)` stays in [0,1] and
    /// the pixel's channel spread degrades gracefully rather than collapsing.
    #[test]
    fn vibrance_does_not_grey_snap_scene_linear_high_saturation_pixel() {
        use crate::local::AdjustmentSet;

        let hot = [1.74, 0.17, 0.17];
        let spread = |c: [f32; 3]| {
            c.iter().cloned().fold(f32::MIN, f32::max) - c.iter().cloned().fold(f32::MAX, f32::min)
        };
        let input_spread = spread(hot);
        assert!(
            input_spread > 1.0,
            "fixture must actually be high-saturation scene-linear: spread={input_spread}"
        );

        // Positive vibrance must not collapse the pixel toward grey: its output
        // spread must retain a sane fraction of the input spread.
        let a_pos = AdjustmentSet {
            vibrance: 0.3,
            ..Default::default()
        };
        let out_pos = light_color_apply(hot, &a_pos, false);
        let pos_spread = spread(out_pos);
        assert!(
            pos_spread >= 0.5 * input_spread,
            "vibrance +0.3 must not grey-snap a high-saturation scene-linear pixel: \
             input spread={input_spread} output spread={pos_spread} out={out_pos:?}"
        );

        // Negative vibrance must not INCREASE the spread (that would mean the
        // old bug's sign flip — grey-snapping "harder" than intended — is still
        // present).
        let a_neg = AdjustmentSet {
            vibrance: -0.5,
            ..Default::default()
        };
        let out_neg = light_color_apply(hot, &a_neg, false);
        let neg_spread = spread(out_neg);
        assert!(
            neg_spread <= input_spread + 1e-4,
            "vibrance -0.5 must not increase a high-saturation pixel's spread: \
             input spread={input_spread} output spread={neg_spread} out={out_neg:?}"
        );
    }

    #[test]
    fn cpu_reference_applies_curve_hsl_grade() {
        use crate::local::AdjustmentSet;

        // Curve: a strong lift must brighten the reference output.
        let mut a = AdjustmentSet::default();
        a.tone_curve.points = vec![(0.0, 0.3), (1.0, 1.0)];
        let lifted = light_color_apply([0.2, 0.2, 0.2], &a, false);
        let base = light_color_apply([0.2, 0.2, 0.2], &AdjustmentSet::default(), false);
        assert!(lifted[0] > base[0], "curve lift raises output");

        // Grade: a saturated shadows tint must move channel balance.
        let mut a = AdjustmentSet::default();
        a.color_grade.shadows = crate::op::GradeWheel {
            hue: 210.0,
            sat: 0.5,
            lum: 0.0,
        };
        let graded = light_color_apply([0.1, 0.1, 0.1], &a, false);
        assert_ne!(graded, [0.1, 0.1, 0.1]);

        // Identity set stays a pure pass-through (bit-stable vs the old reference).
        let id = light_color_apply([0.4, 0.5, 0.6], &AdjustmentSet::default(), false);
        let old = {
            // exposure-only path unchanged: 0 EV ⇒ input unchanged through the whole chain
            [0.4, 0.5, 0.6]
        };
        assert_eq!(id, old, "identity extension is a no-op");
    }

    #[test]
    fn reserved_fields_do_not_change_output() {
        use crate::local::AdjustmentSet;
        let a = AdjustmentSet {
            texture: 1.0,
            clarity: 1.0,
            ..Default::default()
        };
        assert_eq!(
            light_color_apply([0.3, 0.4, 0.5], &a, false),
            [0.3, 0.4, 0.5]
        );
    }

    #[test]
    fn parametric_identity_is_a_linear_ramp() {
        use crate::op::ParametricCurve;
        let lut = parametric_curve_lut(&ParametricCurve::default());
        for (i, &v) in lut.iter().enumerate() {
            assert!(
                (v - i as f32 / 255.0).abs() < 1e-4,
                "identity parametric must be the identity ramp at {i}"
            );
        }
    }

    #[test]
    fn parametric_is_monotone_non_decreasing() {
        use crate::op::ParametricCurve;
        // Opposing extreme regions — still must not dip.
        let p = ParametricCurve {
            shadows: 1.0,
            darks: -1.0,
            lights: 1.0,
            highlights: -1.0,
            ..Default::default()
        };
        let lut = parametric_curve_lut(&p);
        for i in 1..256 {
            assert!(lut[i] >= lut[i - 1] - 1e-6, "dipped at {i}");
            assert!(
                (0.0..=1.0).contains(&lut[i]),
                "out of range at {i}: {}",
                lut[i]
            );
        }
    }

    #[test]
    fn raising_shadows_lifts_low_end_only() {
        use crate::op::ParametricCurve;
        let p = ParametricCurve {
            shadows: 1.0,
            ..Default::default()
        };
        let lut = parametric_curve_lut(&p);
        // Low quarter is lifted above the identity ramp.
        let x_lo = 32usize;
        assert!(
            lut[x_lo] > x_lo as f32 / 255.0 + 0.01,
            "shadows lifted low end"
        );
        // The far highlight end is essentially untouched.
        let x_hi = 240usize;
        assert!(
            (lut[x_hi] - x_hi as f32 / 255.0).abs() < 0.02,
            "highlights end unchanged by a shadows lift"
        );
    }

    #[test]
    fn raising_highlights_lifts_high_end_only() {
        use crate::op::ParametricCurve;
        let p = ParametricCurve {
            highlights: 1.0,
            ..Default::default()
        };
        let lut = parametric_curve_lut(&p);
        let x_hi = 224usize;
        assert!(
            lut[x_hi] > x_hi as f32 / 255.0 + 0.01,
            "highlights lifted high end"
        );
        let x_lo = 16usize;
        assert!(
            (lut[x_lo] - x_lo as f32 / 255.0).abs() < 0.02,
            "shadows end unchanged by a highlights lift"
        );
    }

    #[test]
    fn out_of_order_splits_do_not_panic_and_stay_monotone() {
        use crate::op::ParametricCurve;
        // User dragged splits into a degenerate/reversed order.
        let p = ParametricCurve {
            shadows: 0.5,
            highlights: 0.5,
            shadow_split: 0.9,
            midtone_split: 0.1,
            highlight_split: 0.5,
            ..Default::default()
        };
        let lut = parametric_curve_lut(&p);
        for i in 1..256 {
            assert!(lut[i] >= lut[i - 1] - 1e-6, "dipped at {i}");
        }
    }

    #[test]
    fn parametric_degenerate_splits_no_nan_and_stays_monotone() {
        use crate::op::ParametricCurve;
        // Collapsed split configs: (shadow_split, midtone_split, highlight_split).
        let configs = [(0.0, 0.0, 0.5), (0.5, 1.0, 1.0), (0.5, 0.5, 0.5)];
        for (shadow_split, midtone_split, highlight_split) in configs {
            let p = ParametricCurve {
                shadows: 0.5,
                highlights: 0.5,
                shadow_split,
                midtone_split,
                highlight_split,
                ..Default::default()
            };
            let lut = parametric_curve_lut(&p);
            for i in 0..256 {
                assert!(
                    lut[i].is_finite(),
                    "NaN/inf at {i} for splits ({shadow_split}, {midtone_split}, {highlight_split})"
                );
                assert!(
                    (0.0..=1.0).contains(&lut[i]),
                    "out of range at {i}: {} for splits ({shadow_split}, {midtone_split}, {highlight_split})",
                    lut[i]
                );
                if i > 0 {
                    assert!(
                        lut[i] >= lut[i - 1] - 1e-6,
                        "dipped at {i} for splits ({shadow_split}, {midtone_split}, {highlight_split})"
                    );
                }
            }
        }
    }

    #[test]
    fn tone_curve_luts_none_is_three_identity_ramps() {
        let luts = tone_curve_luts(None);
        for (ch, row) in luts.iter().enumerate() {
            for (i, &v) in row.iter().enumerate() {
                assert!(
                    (v - i as f32 / 255.0).abs() < 1e-4,
                    "channel {ch} entry {i} must be identity"
                );
            }
        }
    }

    #[test]
    fn master_only_curve_equals_legacy_lut_on_all_channels() {
        use crate::op::{CurveMode, ToneCurve};
        // A master-only edit must bake the SAME curve onto R, G and B (regression
        // guard: existing single-LUT goldens must stay valid).
        let pts = vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)];
        let tc = ToneCurve {
            points: pts.clone(),
            mode: CurveMode::Linear,
            ..Default::default()
        };
        let master = curve_lut(&pts, CurveMode::Linear);
        let luts = tone_curve_luts(Some(&tc));
        for (ch, row) in luts.iter().enumerate() {
            for (i, (&v, &m)) in row.iter().zip(master.iter()).enumerate() {
                assert!(
                    (v - m).abs() < 1e-4,
                    "channel {ch} entry {i}: {v} vs master {m}"
                );
            }
        }
    }

    #[test]
    fn red_only_curve_changes_red_row_leaves_green_blue_identity() {
        use crate::op::{CurveMode, PointCurve, ToneCurve};
        let tc = ToneCurve {
            red: PointCurve {
                points: vec![(0.0, 0.0), (0.5, 0.2), (1.0, 1.0)],
                mode: CurveMode::Linear,
            },
            ..Default::default()
        };
        let luts = tone_curve_luts(Some(&tc));
        // Red midtone pulled below the diagonal.
        assert!(luts[0][128] < 128.0 / 255.0 - 0.02, "red midtones darkened");
        // Green and Blue remain the identity ramp.
        for ch in [1usize, 2usize] {
            for (i, &v) in luts[ch].iter().enumerate() {
                assert!(
                    (v - i as f32 / 255.0).abs() < 1e-4,
                    "channel {ch} entry {i} must stay identity"
                );
            }
        }
    }

    #[test]
    fn compose_order_is_channel_of_master_of_parametric() {
        use crate::op::{CurveMode, ParametricCurve, PointCurve, ToneCurve};
        // Parametric lifts shadows; master is identity; red darkens midtones.
        // The red row must equal red_curve( parametric(x) ) since master is identity.
        let param = ParametricCurve {
            shadows: 0.5,
            ..Default::default()
        };
        let red = PointCurve {
            points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
            mode: CurveMode::Linear,
        };
        let tc = ToneCurve {
            parametric: param,
            red: red.clone(),
            ..Default::default()
        };
        let luts = tone_curve_luts(Some(&tc));
        // Hand-compose the expected red row.
        let p_lut = parametric_curve_lut(&param);
        let r_lut = curve_lut(&red.points, red.mode);
        for i in 0..256 {
            let expected = sample_lut(&r_lut, p_lut[i]); // master identity is a no-op
            assert!(
                (luts[0][i] - expected).abs() < 2e-3,
                "red row entry {i}: {} vs expected {}",
                luts[0][i],
                expected
            );
        }
    }

    #[test]
    fn color_grade_identity_when_neutral() {
        use crate::op::ColorGrade;
        let c = color_grade_px([0.3, 0.5, 0.7], &ColorGrade::default());
        assert!(
            (c[0] - 0.3).abs() < 1e-6 && (c[1] - 0.5).abs() < 1e-6 && (c[2] - 0.7).abs() < 1e-6
        );
    }

    #[test]
    fn shadows_tint_colors_darks_not_highlights() {
        use crate::op::{ColorGrade, GradeWheel};
        // A blue (hue 240) shadow tint.
        let cg = ColorGrade {
            shadows: GradeWheel {
                hue: 240.0,
                sat: 1.0,
                lum: 0.0,
            },
            ..Default::default()
        };
        let dark = color_grade_px([0.1, 0.1, 0.1], &cg);
        let light = color_grade_px([0.9, 0.9, 0.9], &cg);
        // Darks gain blue (B rises above R). Highlights are ~unchanged.
        assert!(
            dark[2] > dark[0] + 0.02,
            "shadow tint bluened the darks: {dark:?}"
        );
        assert!(
            (light[0] - 0.9).abs() < 0.03 && (light[2] - 0.9).abs() < 0.03,
            "highlights ~unchanged by a shadows-only tint: {light:?}"
        );
    }

    #[test]
    fn global_tint_affects_all_luminances() {
        use crate::op::{ColorGrade, GradeWheel};
        let cg = ColorGrade {
            global: GradeWheel {
                hue: 120.0,
                sat: 1.0,
                lum: 0.0,
            }, // green
            ..Default::default()
        };
        let dark = color_grade_px([0.1, 0.1, 0.1], &cg);
        let light = color_grade_px([0.8, 0.8, 0.8], &cg);
        assert!(dark[1] > dark[0] + 0.02, "global greened the darks");
        assert!(
            light[1] > light[0] + 0.02,
            "global greened the highlights too"
        );
    }

    #[test]
    fn balance_shifts_the_region_split() {
        // With balance negative, the shadow region shrinks (pivot moves down), so a
        // mid-dark pixel leans more highlight; with balance positive it leans shadow.
        let (sh_lo, _, _) = grade_region_weights(0.4, 0.5, -0.6);
        let (sh_hi, _, _) = grade_region_weights(0.4, 0.5, 0.6);
        assert!(
            sh_hi > sh_lo,
            "positive balance raises the shadow weight at a fixed Y"
        );
    }

    #[test]
    fn blending_widens_region_overlap() {
        // At the extremes, wider blending pulls the shadow/highlight weights toward
        // 0.5 (more overlap); narrow blending pushes them apart.
        let (sh_wide, _, _) = grade_region_weights(0.25, 1.0, 0.0);
        let (sh_narrow, _, _) = grade_region_weights(0.25, 0.0, 0.0);
        assert!(
            sh_narrow > sh_wide,
            "narrow blending keeps low-Y firmly in shadows"
        );
    }

    #[test]
    fn grade_tint_is_zero_at_zero_sat() {
        assert_eq!(grade_tint(123.0, 0.0), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn lum_only_wheel_shifts_brightness_without_tint() {
        use crate::op::{ColorGrade, GradeWheel};
        let cg = ColorGrade {
            global: GradeWheel {
                hue: 0.0,
                sat: 0.0,
                lum: 0.5,
            },
            ..Default::default()
        };
        let c = color_grade_px([0.4, 0.4, 0.4], &cg);
        assert!(
            c[0] > 0.4 && (c[0] - c[1]).abs() < 1e-6 && (c[1] - c[2]).abs() < 1e-6,
            "uniform brighten, no tint: {c:?}"
        );
    }

    #[test]
    fn color_grade_uniform_identity_is_all_zero_tints() {
        let u = color_grade_uniform(None);
        assert_eq!(u.shadows, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(u.midtones, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(u.highlights, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(u.global, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(u.params, [0.5, 0.0, 0.0, 0.0]); // default blending/balance
        assert_eq!(std::mem::size_of::<ColorGradeUniform>() % 16, 0);
    }

    #[test]
    fn color_grade_uniform_prescales_tint_and_lum() {
        use crate::op::{ColorGrade, GradeWheel};
        let cg = ColorGrade {
            shadows: GradeWheel {
                hue: 240.0,
                sat: 1.0,
                lum: 0.4,
            },
            blending: 0.7,
            balance: -0.2,
            ..Default::default()
        };
        let u = color_grade_uniform(Some(cg));
        // Tint row = grade_tint(...) * GRADE_TINT_STRENGTH; lum = 0.4 * GRADE_LUM_STRENGTH.
        let t = grade_tint(240.0, 1.0);
        assert!((u.shadows[0] - t[0] * GRADE_TINT_STRENGTH).abs() < 1e-6);
        assert!((u.shadows[1] - t[1] * GRADE_TINT_STRENGTH).abs() < 1e-6);
        assert!((u.shadows[2] - t[2] * GRADE_TINT_STRENGTH).abs() < 1e-6);
        assert!((u.shadows[3] - 0.4 * GRADE_LUM_STRENGTH).abs() < 1e-6);
        assert_eq!(u.params, [0.7, -0.2, 0.0, 0.0]);
    }
}
