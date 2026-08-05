//! Pure CPU math turning UI op params into GPU shader uniforms, plus the
//! `#[repr(C)]` Pod uniform structs (layouts MIRROR the WGSL `struct P` in each
//! shader). Display-linear space; the sRGB OETF lives only in the display/blit
//! shader. No GPU here — fully unit-tested.

use crate::op::{Geometry, Hsl, LensCorrection, Sharpen};
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
    /// P4 Task 5: halo suppression (0..1). Occupies what was `pad[0]`, so the
    /// struct size and 16-byte alignment are UNCHANGED — load-bearing, because
    /// `SharpenNode` writes ONE buffer per dispatch and binds it to shaders
    /// whose WGSL `struct P` must match this layout byte-for-byte (the box-h/
    /// box-v/masked-apply passes never read this field, but declare it for
    /// layout match; growing this struct would desync them and corrupt the
    /// box passes).
    pub detail: f32,
    /// P4 Task 5: edge masking (0..1). Occupies what was `pad[1]`.
    pub masking: f32,
}

// The uniform's size is the GPU ABI, shared byte-for-byte across every sharpen
// shader's `struct P` (box-h, box-v, apply, apply-detail, apply-masked) —
// keep in lock-step (mirrors `NrUniform`/`GeometryUniform`'s own size asserts
// above). MUST stay exactly 16 bytes: `detail`/`masking` replaced `pad: [f32;
// 2]` rather than growing the struct (see the field docs above).
const _: () = assert!(std::mem::size_of::<SharpenUniform>() == 16);
const _: () = assert!(std::mem::size_of::<SharpenUniform>().is_multiple_of(16));

pub fn sharpen_uniform(op: Option<Sharpen>) -> SharpenUniform {
    let (amount, radius, detail, masking) = op
        .map(|s| (s.amount, s.radius, s.detail, s.masking))
        .unwrap_or((0.0, 0, 0.0, 0.0));
    SharpenUniform {
        amount,
        radius: radius.min(MAX_SHARPEN_RADIUS) as i32,
        detail,
        masking,
    }
}

/// Gradient normalization for the sharpen edge mask (`G`, design §4.3) — the
/// single named tuning knob for masking responsiveness, in the spirit of
/// `KEYSTONE_STRENGTH`. **Mirrored as `G` in `sharpen_apply_detail.wgsl`**: it
/// is a WGSL `const` there rather than a uniform field, precisely so
/// `SharpenUniform` stays 16 bytes. Change both together.
pub const SHARPEN_MASK_GRADIENT_NORM: f32 = 0.25;

/// Halo (pixels) a tiled full-res sharpen pass must over-fetch. Zero when the
/// op is absent or a no-op (amount 0). Consumed by Plan 3's tile producer.
/// Global-only — see `sharpen_halo_doc` for the whole-document (Phase 4 Task
/// 4, per-mask sharpen) version. P4 Task 5: active masking adds exactly one
/// extra pixel for the central-difference gradient sample (design §4.4) —
/// `detail`'s narrower `r/3` blur never widens the halo (`r` dominates), so
/// it contributes nothing here. The `MAX_SHARPEN_RADIUS` clamp is applied to
/// `radius` BEFORE the gradient pixel is added.
pub fn sharpen_halo(op: Option<Sharpen>) -> u32 {
    match op {
        Some(s) if s.amount != 0.0 => s.radius.min(MAX_SHARPEN_RADIUS) + (s.masking > 0.0) as u32,
        _ => 0,
    }
}

/// Halo (pixels) a tiled full-res sharpen pass must over-fetch, across the
/// WHOLE document: the max radius over the global `Sharpen` op (via
/// `sharpen_halo`, when active — `amount != 0.0`) and every VISIBLE mask
/// layer's own active sharpen (Phase 4 Task 4 — per-mask sharpen shares the
/// separable blur machinery but each layer/the global op can carry its own
/// radius, so the tiled halo must cover the LARGEST neighbourhood anyone will
/// actually blur at, or a per-mask sharpen would read past the haloed tile's
/// edge). Zero when nothing is active anywhere. Clamped to
/// `MAX_SHARPEN_RADIUS` per contributor (mirrors `sharpen_halo`/
/// `sharpen_uniform`'s own clamp). A hidden layer never counts, mirroring
/// `LocalAdjustments::is_identity`'s `visible_layers()` filter /
/// `EditDoc::dehaze_active_anywhere`'s same pattern. Use this (not the
/// global-only `sharpen_halo`) for any rebuild/halo decision that must be
/// aware of per-mask sharpen — `ferrolite-app`'s `needs_full_rebuild` and
/// `TileEditPipeline::new`'s construction-time halo both do.
pub fn sharpen_halo_doc(doc: &crate::op::OpStack) -> u32 {
    let mut max_r = sharpen_halo(doc.sharpen());
    for layer in doc.layers.iter().filter(|l| l.visible) {
        let s = layer.adjustments.sharpen;
        if s.amount != 0.0 {
            let r = s.radius.min(MAX_SHARPEN_RADIUS) + (s.masking > 0.0) as u32;
            max_r = max_r.max(r);
        }
    }
    max_r
}

/// GPU layout for one NR level's dispatch. Only the CURRENT level is live per
/// dispatch, so `thresholds[0]` is this level's luma threshold and
/// `thresholds[1]` its chroma threshold; `[2..8]` is reserved padding keeping
/// the struct 16-byte-aligned for WGSL uniform rules.
///
/// `pub(crate)` (final-review FIX 9): built and consumed entirely inside
/// `nr_node.rs`'s per-level dispatch loop — nothing outside this crate needs
/// the raw GPU layout. Narrow, don't widen, unless a real external consumer
/// shows up.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct NrUniform {
    pub thresholds: [f32; 8],
    /// 0 = identity (the node never dispatches in this state).
    pub active: i32,
    /// À trous hole spacing for this level: `2^level`.
    pub spacing: i32,
    pub level: i32,
    pub pad: f32,
}

// The uniform's size is the GPU ABI — keep in lock-step with the WGSL side
// (mirrors `GeometryUniform`'s own size assert above): 8 f32 thresholds (32B)
// + active/spacing/level/pad (16B) = 48B, already 16-byte-aligned.
const _: () = assert!(std::mem::size_of::<NrUniform>() == 48);
const _: () = assert!(std::mem::size_of::<NrUniform>().is_multiple_of(16));

/// Build the uniform for `level` of the à trous loop. `pub(crate)` — see
/// `NrUniform`'s doc. `level` must be `< NR_LEVELS`: `threshold_at` clamps it
/// defensively, but `spacing: 1 << level` below does not, so an out-of-range
/// `level` would silently produce a nonsensical (but not out-of-bounds)
/// spacing rather than panicking — asserted in debug builds since the sole
/// caller (`nr_node.rs`'s `0..NR_LEVELS` loop) should never pass one.
pub(crate) fn nr_uniform(nr: &crate::local::NoiseReduction, level: usize) -> NrUniform {
    debug_assert!(
        level < crate::nr::NR_LEVELS,
        "nr_uniform: level {level} out of range (NR_LEVELS = {})",
        crate::nr::NR_LEVELS
    );
    let mut thresholds = [0.0f32; 8];
    thresholds[0] = crate::nr::threshold_at(nr.luminance, nr.detail, level);
    thresholds[1] = crate::nr::threshold_at(nr.color, nr.color_detail, level);
    NrUniform {
        thresholds,
        active: (!nr.is_identity()) as i32,
        spacing: 1 << level,
        level: level as i32,
        pad: 0.0,
    }
}

/// Halo (pixels) a tiled NR pass must over-fetch. Zero unless NR is
/// [`is_active`](crate::local::NoiseReduction::is_active) — NOT merely
/// non-identity (final-review FIX 1): a detail-only edit (zero
/// `luminance`/`color`) is non-identity but dispatches nothing, so it must
/// contribute no halo either, or a detail-only drag would force a full
/// `TileEditPipeline` rebuild for zero visual effect. Mirrors
/// `sharpen_halo`'s contract otherwise.
pub fn nr_halo(nr: &crate::local::NoiseReduction) -> u32 {
    if nr.is_active() {
        crate::nr::nr_halo_px()
    } else {
        0
    }
}

/// Whole-document NR halo. NR is GLOBAL-ONLY (design §3.5) — it runs upstream of
/// where masks are composited — so unlike `sharpen_halo_doc` this deliberately
/// does NOT walk the layers. A layer's `noise_reduction` fields are never
/// applied, so they must contribute no halo.
pub fn nr_halo_doc(doc: &crate::op::OpStack) -> u32 {
    nr_halo(&doc.global.noise_reduction)
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
    /// Source-normalized clamp rect for `base_uv`: `[min_u, min_v, max_u, max_v]`
    /// -- the crop sub-rect (§ `geometry_uniform`'s `cx/cy/cw/ch`), inset by half
    /// a source texel on each side. Spec C2 part 2: the geometry sampler used to
    /// clamp to the WHOLE source texture, so a rotated crop's out-of-bounds
    /// corners smeared the FRAME's edge texel outward; clamping `base_uv` to
    /// THIS rect instead means an out-of-crop sample clamps to the crop's own
    /// edge. A full-frame (no-crop) `Geometry` gets the half-texel-inset FULL
    /// rect here, so un-cropped rendering (including rotation past the frame
    /// edge) is byte-identical to before this field existed.
    pub crop_bounds: [f32; 4],
    /// Rows of the row-major 3×3 output-px → source-px homography `H`, each
    /// padded to a vec4 (last lane 0) for WGSL uniform alignment. The shader
    /// samples `src = (H·[po, 1]).xy / (H·[po, 1]).z` (perspective divide) —
    /// spec C4 part 2, manual keystone. Built as `A_ext · D · W · N`: the
    /// keystone unit-square warp `W` (see `keystone_quad_homography`) acts on
    /// the CROP-LOCAL unit square the user sees (`N`/`D` normalize output px
    /// by / rescale back from the FULL output dims), and the existing affine
    /// crop/rotation `A_ext = [m|off; 0 0 1]` then consumes the
    /// keystone-warped coordinate. With `keystone_v == keystone_h == 0` the
    /// rows are set to EXACTLY the affine extension — `h2 = [0, 0, 1, 0]`, so
    /// the divide is by exactly 1.0 and the mapping stays BIT-identical to
    /// the pre-keystone affine path (guards every existing golden). `m`/`off`
    /// above keep carrying the keystone-FREE affine part for the CPU
    /// consumers that invert or re-apply it (`coord.rs`'s display↔source
    /// mapping, the fused dehaze recovery's source-UV sample) — under a
    /// non-zero keystone those treat the warp as identity (documented
    /// approximation, same tier as their existing lens-as-identity fallback).
    pub h0: [f32; 4],
    pub h1: [f32; 4],
    pub h2: [f32; 4],
}

// The uniform's size/offsets are the GPU ABI — keep in lock-step with WGSL
// `struct P` in geometry.wgsl (see also `geometry_uniform_size_is_16_byte_aligned`).
const _: () = assert!(std::mem::size_of::<GeometryUniform>() == 112);
const _: () = assert!(std::mem::size_of::<GeometryUniform>().is_multiple_of(16));

/// Manual keystone strength: at a full slider throw (`|k| = 1`) each corner of
/// the far edge is displaced OUTWARD along that edge by `0.5 * KEYSTONE_STRENGTH`
/// = 17.5% of the crop extent, i.e. the far edge's sampled span widens by
/// `1 + KEYSTONE_STRENGTH·|k|` — equivalently the DISPLAYED content on that
/// edge scales by `1 / (1 + KEYSTONE_STRENGTH·|k|)`. Tune keystone
/// responsiveness ONLY via this named constant.
pub const KEYSTONE_STRENGTH: f32 = 0.35;

/// The keystone warp `W` on the crop-local unit square: a row-major 3×3
/// homography mapping output-normalized coords → sample coords (both in the
/// crop-local unit square's frame; projective apply with a divide by row 2).
///
/// Corner model (spec C4, sign convention pinned by
/// `keystone_v_positive_widens_top_sampled_span`): `keystone_v = kv > 0`
/// widens the TOP edge's sampled span (converging verticals corrected;
/// kv < 0 the bottom's); `keystone_h` is the transpose (kh > 0 widens the
/// LEFT edge's sampled span). Each affected edge's two corners are displaced
/// OUTWARD along the edge by `0.5 · KEYSTONE_STRENGTH · |k|`:
///
/// ```text
///   dt = max(kv, 0)·K/2    db = max(-kv, 0)·K/2   (x displacement, top/bottom)
///   dl = max(kh, 0)·K/2    dr = max(-kh, 0)·K/2   (y displacement, left/right)
///
///   (0,0) → (-dt, -dl)     (1,0) → (1+dt, -dr)
///   (0,1) → (-db, 1+dl)    (1,1) → (1+db, 1+dr)
/// ```
///
/// A COMBINED kv + kh applies BOTH displacement sets to the four corners and
/// solves ONE homography from the result — they compose in a single 4-point
/// solve, NOT as two multiplied single-axis homographies (a product would add
/// cross terms the corner model does not ask for).
///
/// Closed-form 4-point solve (unit square → quad; Heckbert, *Fundamentals of
/// Texture Mapping and Image Warping*, §2.2.1 — closed form, no general DLT).
/// Derivation: write `H = [[a,b,c],[d,e,f],[g,h,1]]` with
/// `H(u,v) = ((a·u + b·v + c)/(g·u + h·v + 1), (d·u + e·v + f)/(g·u + h·v + 1))`.
/// The (0,0) corner gives `c = x00`, `f = y00` directly. Summing the four
/// corner constraints, the projective terms depend only on
/// `Σx = x00−x10+x11−x01`, `Σy = y00−y10+y11−y01` (both zero ⇔ the quad is a
/// parallelogram ⇔ the warp is affine, `g = h = 0`); eliminating `a,b,d,e`
/// from the (1,0)/(0,1)/(1,1) constraints leaves a 2×2 system in `g,h` with
/// `dx1 = x10−x11, dx2 = x01−x11, dy1 = y10−y11, dy2 = y01−y11`:
///
/// ```text
///   den = dx1·dy2 − dy1·dx2
///   g   = (Σx·dy2 − Σy·dx2) / den        h = (dx1·Σy − dy1·Σx) / den
///   a   = x10 − x00 + g·x10              b = x01 − x00 + h·x01
///   d   = y10 − y00 + g·y10              e = y01 − y00 + h·y01
/// ```
///
/// `den` cannot vanish here: for `|k| ≤ 1`, `K = 0.35` it is
/// `(dt−db)(dl−dr) − (1+2db)(1+2dr)` ∈ [−2.3, −0.97].
fn keystone_quad_homography(kv: f32, kh: f32) -> [[f32; 3]; 3] {
    let half = 0.5 * KEYSTONE_STRENGTH;
    let dt = kv.max(0.0) * half;
    let db = (-kv).max(0.0) * half;
    let dl = kh.max(0.0) * half;
    let dr = (-kh).max(0.0) * half;
    let (x00, y00) = (-dt, -dl);
    let (x10, y10) = (1.0 + dt, -dr);
    let (x01, y01) = (-db, 1.0 + dl);
    let (x11, y11) = (1.0 + db, 1.0 + dr);

    let sum_x = x00 - x10 + x11 - x01;
    let sum_y = y00 - y10 + y11 - y01;
    let dx1 = x10 - x11;
    let dx2 = x01 - x11;
    let dy1 = y10 - y11;
    let dy2 = y01 - y11;
    let den = dx1 * dy2 - dy1 * dx2;
    let g = (sum_x * dy2 - sum_y * dx2) / den;
    let h = (dx1 * sum_y - dy1 * sum_x) / den;
    let a = x10 - x00 + g * x10;
    let b = x01 - x00 + h * x01;
    let d = y10 - y00 + g * y10;
    let e = y01 - y00 + h * y01;
    [[a, b, x00], [d, e, y00], [g, h, 1.0]]
}

/// Row-major 3×3 matrix product `a · b`.
fn mat3_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

/// CPU mirror of `geometry.wgsl`'s projective output→source mapping: `po` is
/// the output-space coordinate the shader builds (`out_origin + gid + 0.5`);
/// the result is the SOURCE-pixel coordinate, before normalization by
/// `src_dims` and the `clamp_uv_to_crop_bounds` clamp. Kept in lock-step with
/// the WGSL (same expression shape, same evaluation order) so parity tests
/// can assert the zero-keystone mapping BIT-exactly against the old affine
/// `m·po + off` — the invariant that guards every existing golden.
pub fn geometry_src_px(u: &GeometryUniform, po: [f32; 2]) -> [f32; 2] {
    let hx = u.h0[0] * po[0] + u.h0[1] * po[1] + u.h0[2];
    let hy = u.h1[0] * po[0] + u.h1[1] * po[1] + u.h1[2];
    let hw = u.h2[0] * po[0] + u.h2[1] * po[1] + u.h2[2];
    [hx / hw, hy / hw]
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
    let geo = op.unwrap_or_default();

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

    // Pivot on the ROUNDED output extent, not the fractional crop_w_px/
    // crop_h_px: out_w/out_h are the actual pixel dims being sampled, so the
    // crop center used to derive `off` must agree with them. Using the
    // unrounded crop_w_px/crop_h_px here (as before) leaves a ≤0.5px
    // rounding remainder baked into every output texel's source coordinate,
    // smearing the last row/column outward past the true crop extent. For
    // an exact-pixel crop (out_w == crop_w_px, no remainder) this is a no-op.
    let out_center = [out_w as f32 * 0.5, out_h as f32 * 0.5];
    let crop_center = [cx * sw + out_w as f32 * 0.5, cy * sh + out_h as f32 * 0.5];
    let off = [
        crop_center[0] - (m[0] * out_center[0] + m[1] * out_center[1]),
        crop_center[1] - (m[2] * out_center[0] + m[3] * out_center[1]),
    ];

    let crop_bounds = crop_uv_bounds(cx, cy, cw, ch, sw, sh);

    // Spec C4 part 2 (manual keystone): generalize the affine to a projective
    // mapping. The affine extension `A_ext = [m|off; 0 0 1]`; keystone warps
    // the CROP-LOCAL unit square (the user perceives keystone relative to the
    // crop they see), so the full homography is `H = A_ext · D · W · N` — the
    // affine consumes the keystone-warped coordinate. Zero keystone takes
    // `A_ext` DIRECTLY (no matrix products), so `h2 = [0,0,1]` exactly and the
    // shader's divide is by exactly 1.0 — bit-identical to the pre-keystone
    // affine path (see `geometry_homography_zero_keystone_is_bit_identical_to_affine`).
    let kv = geo.keystone_v.clamp(-1.0, 1.0);
    let kh = geo.keystone_h.clamp(-1.0, 1.0);
    let a_ext = [[m[0], m[1], off[0]], [m[2], m[3], off[1]], [0.0, 0.0, 1.0]];
    let h = if kv == 0.0 && kh == 0.0 {
        a_ext
    } else {
        let ow = out_w as f32;
        let oh = out_h as f32;
        // N: output px → crop-local normalized; D: back to px. `po` includes
        // `out_origin` on the tile path, so W acts in FULL-output pixel space
        // exactly as the affine always has.
        let n_mat = [[1.0 / ow, 0.0, 0.0], [0.0, 1.0 / oh, 0.0], [0.0, 0.0, 1.0]];
        let d_mat = [[ow, 0.0, 0.0], [0.0, oh, 0.0], [0.0, 0.0, 1.0]];
        let w_unit = keystone_quad_homography(kv, kh);
        mat3_mul(a_ext, mat3_mul(d_mat, mat3_mul(w_unit, n_mat)))
    };

    (
        GeometryUniform {
            m,
            off,
            src_dims: [sw, sh],
            out_dims: [out_w as f32, out_h as f32],
            out_origin: [0.0, 0.0],
            crop_bounds,
            h0: [h[0][0], h[0][1], h[0][2], 0.0],
            h1: [h[1][0], h[1][1], h[1][2], 0.0],
            h2: [h[2][0], h[2][1], h[2][2], 0.0],
        },
        out_w,
        out_h,
    )
}

/// The source-normalized clamp rect `[min_u, min_v, max_u, max_v]` for a crop
/// `(cx, cy, cw, ch)` (already-clamped normalized fractions, as produced in
/// `geometry_uniform`) against a `(sw, sh)` source: the crop rect inset by half
/// a source texel on each side, so `base_uv` clamped to it samples the crop's
/// own edge texel rather than reading past it. Degenerates gracefully for a
/// crop narrower than one texel (min/max collapse to the rect's midline
/// instead of crossing over) so `min <= max` always holds.
fn crop_uv_bounds(cx: f32, cy: f32, cw: f32, ch: f32, sw: f32, sh: f32) -> [f32; 4] {
    let half_u = 0.5 / sw;
    let half_v = 0.5 / sh;
    let mid_u = cx + cw * 0.5;
    let mid_v = cy + ch * 0.5;
    let min_u = (cx + half_u).min(mid_u);
    let max_u = (cx + cw - half_u).max(mid_u);
    let min_v = (cy + half_v).min(mid_v);
    let max_v = (cy + ch - half_v).max(mid_v);
    [min_u, min_v, max_u, max_v]
}

/// CPU mirror of `geometry.wgsl`'s `base_uv` clamp: `clamp(uv, bounds.xy,
/// bounds.zw)`. Kept as a standalone pure fn (not folded into a full CPU
/// resample) so parity tests can assert the clamped coordinate directly,
/// matching the pattern of this module's other CPU references (e.g.
/// `sample_lut`, `hsl_bands_apply`) that mirror one WGSL step exactly.
pub fn clamp_uv_to_crop_bounds(uv: [f32; 2], crop_bounds: [f32; 4]) -> [f32; 2] {
    [
        uv[0].clamp(crop_bounds[0], crop_bounds[2]),
        uv[1].clamp(crop_bounds[1], crop_bounds[3]),
    ]
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
    // ── Phase 4 Task 2/3 (fused engine): dehaze recovery fused as the FIRST
    // step of `adjust()`, gated on `dehaze_amount_atmos.x != 0.0`. Zeroed by
    // `local_adjust_uniform` for every call site (Light stage, every mask
    // layer); `LocalAdjustmentsNode::evaluate_color` overwrites these five
    // fields on TWO kinds of dispatch: the global Color-stage pseudo-layer
    // (Task 2, driven by the global `Dehaze` op's amount) and a per-mask-layer
    // dispatch (Task 3, driven by THAT layer's own `dehaze.amount`) — in both
    // cases only when the driving amount is non-zero AND a real shared
    // transmission is bound. Every other dispatch (the Light stage, or a mask
    // layer whose own amount is 0) takes the identical zero-extra-work path
    // the old (now-retired) `DehazeRecoveryNode` used for `amount == 0`/no
    // transmission. Field shapes mirror that node's retired `RecoveryParams`
    // exactly (minus `t0`, hardcoded as a WGSL const — never user-adjustable —
    // and reflowed into vec4s since WGSL/std140 has no scalar/vec2 packing
    // here).
    /// x = dehaze amount (0 = inactive this dispatch); yzw = atmospheric
    /// light `A` (floored to `DEHAZE_ATMOS_MIN`).
    pub dehaze_amount_atmos: [f32; 4],
    /// Row-major 2×2 output→source mapping (mirrors `GeometryUniform::m`).
    pub dehaze_geo_m: [f32; 4],
    /// `[geo_off.x, geo_off.y, src_dims.x, src_dims.y]` (mirrors
    /// `GeometryUniform::off`/`src_dims`).
    pub dehaze_geo_off_src_dims: [f32; 4],
    /// `[frame_origin.x, frame_origin.y, full_dims.x, full_dims.y]` — this
    /// pass's `TileFrame` (haloed tile origin + full output dims at this LOD),
    /// making the source-UV sample LOD-independent exactly as the retired
    /// `DehazeRecoveryNode`'s `dehaze_recovery.wgsl` did.
    pub dehaze_frame: [f32; 4],
    /// `[out_dims.x, out_dims.y, has_transmission (0.0/1.0), pad]`. `out_dims`
    /// is the LEVEL-0 output dims (mirrors `GeometryUniform::out_dims`);
    /// `has_transmission` gates the shader's sample (0 = the node's 1×1
    /// neutral fallback is bound, so the recovery step is a no-op regardless
    /// of `dehaze_amount_atmos.x`).
    pub dehaze_out_dims_flags: [f32; 4],
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
        // Phase 4 Task 2/3: identity/inert by default at every call site — only
        // `LocalAdjustmentsNode::evaluate_color` overwrites these, and only for
        // the global Color-stage pseudo-layer's uniform (Task 2) or a
        // per-mask-layer's uniform whose own `dehaze.amount != 0.0` (Task 3) —
        // see the field doc on the struct.
        dehaze_amount_atmos: [0.0; 4],
        dehaze_geo_m: [0.0; 4],
        dehaze_geo_off_src_dims: [0.0; 4],
        dehaze_frame: [0.0; 4],
        dehaze_out_dims_flags: [0.0; 4],
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
/// HSV-style saturation measure `(max-min)/max(max, eps)`, clamped to `[0,1]`.
/// Stable for any brightness — no HSL round-trip, so no denominator
/// singularity at `l == 1.0` or negative saturation at `l > 1.0` (see
/// `rgb_to_hsl`). Used by vibrance's fade weight; mirrors the WGSL
/// `vibrance_weight` in `local_adjust.wgsl` exactly.
fn hsv_sat_measure(c: [f32; 3]) -> f32 {
    let mx = c[0].max(c[1].max(c[2]));
    let mn = c[0].min(c[1].min(c[2]));
    ((mx - mn) / mx.max(1e-4)).clamp(0.0, 1.0)
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

/// `light_color_apply` with the Phase 4 Task 2/3 dehaze recovery fused in as
/// the first step — mirrors `local_adjust.wgsl`'s `dehaze_recover_step` +
/// `adjust()` exactly (the shader applies the recovery step first regardless
/// of the `global_order`/mask-order flag, so this CPU reference does too).
/// `dehaze` is `Some((amount, atmos, t))` when recovery is active for THIS
/// call: `t` is the ALREADY-REFINED transmission (what the shader's `trans`
/// sample would return) — injected directly since a CPU caller has no GPU
/// transmission texture to sample, exactly as `RecoveryParams`'s retired GPU
/// parity tests injected a constant `q`. `None` (or `amount == 0.0`) is the
/// identity path, matching the shader's `dehaze_amount_atmos.x == 0.0` gate.
/// Meaningful for EITHER `global_order`: the shader populates the dehaze
/// fields on the global Color-stage pseudo-layer's uniform (Task 2,
/// `global_order = true`) AND on a per-mask-layer's uniform whose own
/// `dehaze.amount != 0.0` (Task 3, `global_order = false`) — see
/// `local_node.rs::evaluate_color`. A caller with `dehaze: None` is this
/// function with the identity path taken, so `light_color_apply` (below)
/// remains unaffected for every existing call site.
#[allow(dead_code)]
pub fn light_color_apply_with_dehaze(
    rgb: [f32; 3],
    a: &crate::local::AdjustmentSet,
    global_order: bool,
    dehaze: Option<(f32, [f32; 3], f32)>,
) -> [f32; 3] {
    let rgb = match dehaze {
        Some((amount, atmos, t)) if amount != 0.0 => {
            // Convert the injected transmission `t` back to the `dark`
            // (pre-omega dark-channel) input `dehaze_recover` expects, so its
            // internal `t = 1 - omega*dark` reconstructs exactly `t` (then
            // applies the SAME `.clamp(0,1)`/`t0`-floor the shader does).
            // Mirrors the conversion the retired `DehazeRecoveryNode`'s own
            // GPU-vs-CPU parity test used for its shader (which also consumes
            // an already-refined `q`/`t` directly).
            let dark = (1.0 - t) / crate::dehaze::DEHAZE_OMEGA;
            crate::dehaze::dehaze_recover(rgb, dark, atmos, amount)
        }
        _ => rgb,
    };
    light_color_apply(rgb, a, global_order)
}

/// CPU reference for the Light+Color point op. `local_adjust.wgsl` mirrors this
/// exactly (golden tolerance absorbs f16/driver drift). Order: exposure → tonal
/// region gains → {contrast, wb} in the order `global_order` selects → saturation
/// → hue (HSL round-trip, skipped in the achromatic-domain `l≈0/1` singularity)
/// → vibrance (HSV-measure luma-mix, no HSL round-trip) → curve/HSL/grade →
/// color swatch. Output clamped ≥0.
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
    // Hue rotation round-trips through HSL, whose `s = d / (1 - |2l-1|)`
    // formula has a removable-singularity denominator at `l == 1.0` (Inf) and
    // goes negative at `l > 1.0` — both reachable by ordinary scene-linear
    // bright pixels (e.g. an overexposed sky sample after exposure/WB/
    // contrast). Either regime turns `hsl_to_rgb`'s `q = l + s - l*s` into
    // `Inf - Inf` / a NaN propagation, which the GPU stores as NaN and renders
    // as a black pixel — this is the root cause of the "black pixel in bright
    // sky" bug for hue, mirrored exactly by vibrance below (same round trip,
    // same fix shape). An achromatic-domain pixel this close to the l==0/1
    // rail has no meaningful hue to rotate (hue is undefined at the achromatic
    // poles), so the rotation is simply skipped there — identity for this
    // step, not an approximation. Keep the threshold in lockstep with the
    // WGSL `adjust()`'s hue branch in `shaders/local_adjust.wgsl`.
    if u.hue_deg != 0.0 {
        let cc = [c[0].max(0.0), c[1].max(0.0), c[2].max(0.0)];
        let mx = cc[0].max(cc[1].max(cc[2]));
        let mn = cc[0].min(cc[1].min(cc[2]));
        let l = (mx + mn) * 0.5;
        let denom = 1.0 - (2.0 * l - 1.0).abs();
        if denom > 1e-4 {
            let mut hsl = rgb_to_hsl(cc);
            hsl[0] = (hsl[0] + u.hue_deg).rem_euclid(360.0);
            c = hsl_to_rgb(hsl);
        }
    }
    // Vibrance (Phase 3): a saturation boost that fades as a pixel approaches
    // full saturation. Reimplemented WITHOUT the HSL round trip (the original
    // formula shared hue's `l==1`/`l>1` singularity above — see the comment on
    // the hue branch — and produced NaN/black pixels on ordinary scene-linear
    // highlights, e.g. an overexposed sky sample after +1 EV). Mirrors the
    // existing `saturation` step's luma-mix pattern instead: measure "how
    // saturated" the pixel is with a stable HSV-style ratio
    // `(max-min)/max(max, eps)` (well-defined and bounded in `[0,1]` for ANY
    // brightness, no HSL detour), fade the boost as that measure rises toward
    // 1, then mix toward/away from luma exactly like `saturation` does. Gated
    // on non-zero so a zero-vibrance `AdjustmentSet` takes none of this and
    // stays bit-exact (required by `light_color_identity_is_a_no_op`). Keep in
    // lockstep with the WGSL `vibrance_weight`/vibrance branch in
    // `shaders/local_adjust.wgsl`.
    if a.vibrance != 0.0 {
        let w = 1.0 - hsv_sat_measure(c);
        let y3 = luma709(c);
        let factor = 1.0 + a.vibrance * w;
        for v in &mut c {
            *v = y3 + (*v - y3) * factor;
        }
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
            ..Default::default()
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
                radius: 4,
                ..Default::default()
            })),
            0
        );
        assert_eq!(
            sharpen_halo(Some(Sharpen {
                amount: 0.5,
                radius: 4,
                ..Default::default()
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
            ..Default::default()
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
    fn sharpen_halo_doc_is_max_of_global_and_visible_layer_radii() {
        use crate::local::{AdjustmentSet, MaskLayer};
        use crate::op::{EditDoc, Op, Sharpen};
        use ferrolite_mask::MaskDefinition;

        let layer = |amount: f32, radius: u32, visible: bool| MaskLayer {
            name: "l".into(),
            visible,
            mask: MaskDefinition::default(),
            adjustments: AdjustmentSet {
                sharpen: Sharpen {
                    amount,
                    radius,
                    ..Default::default()
                },
                ..Default::default()
            },
        };

        // Nothing active anywhere -> 0.
        assert_eq!(sharpen_halo_doc(&EditDoc::default()), 0);

        // Global only.
        let global_only = EditDoc::default().set_op(Op::Sharpen(Sharpen {
            amount: 0.5,
            radius: 4,
            ..Default::default()
        }));
        assert_eq!(sharpen_halo_doc(&global_only), 4);

        // A visible layer's own active sharpen ALONE (global inactive).
        let layer_only = EditDoc {
            layers: vec![layer(0.5, 7, true)],
            ..Default::default()
        };
        assert_eq!(sharpen_halo_doc(&layer_only), 7);

        // Max of global + a smaller layer radius.
        let mut mixed = global_only.clone();
        mixed.layers = vec![layer(0.5, 2, true)];
        assert_eq!(sharpen_halo_doc(&mixed), 4);

        // Max of global + a LARGER layer radius.
        let mut mixed2 = global_only.clone();
        mixed2.layers = vec![layer(0.5, 9, true)];
        assert_eq!(sharpen_halo_doc(&mixed2), 9);

        // A hidden layer never counts, even with a huge radius.
        let hidden = EditDoc {
            layers: vec![layer(0.5, 50, false)],
            ..Default::default()
        };
        assert_eq!(sharpen_halo_doc(&hidden), 0);

        // A layer with radius but zero amount contributes nothing.
        let zero_amount = EditDoc {
            layers: vec![layer(0.0, 50, true)],
            ..Default::default()
        };
        assert_eq!(sharpen_halo_doc(&zero_amount), 0);

        // Clamped to MAX_SHARPEN_RADIUS.
        let huge_layer = EditDoc {
            layers: vec![layer(0.5, u32::MAX, true)],
            ..Default::default()
        };
        assert_eq!(sharpen_halo_doc(&huge_layer), MAX_SHARPEN_RADIUS);
    }

    /// Gate 2 (design §7.2): the new fields default to zero, so an old sidecar
    /// and a fresh default are indistinguishable, and the render is unchanged.
    #[test]
    fn sharpen_new_fields_default_to_zero_identity() {
        let s = Sharpen::default();
        assert_eq!(s.detail, 0.0);
        assert_eq!(s.masking, 0.0);
        let u = sharpen_uniform(Some(s));
        assert_eq!(u.detail, 0.0);
        assert_eq!(u.masking, 0.0);
    }

    /// Masking adds exactly 1 px (the central-difference gradient) and only
    /// when it is actually active.
    #[test]
    fn sharpen_halo_adds_one_only_when_masking_is_active() {
        let plain = Sharpen {
            amount: 0.5,
            radius: 8,
            detail: 0.0,
            masking: 0.0,
        };
        assert_eq!(sharpen_halo(Some(plain)), 8, "no masking -> unchanged halo");
        let masked = Sharpen {
            amount: 0.5,
            radius: 8,
            detail: 0.0,
            masking: 0.4,
        };
        assert_eq!(
            sharpen_halo(Some(masked)),
            9,
            "masking -> +1 for the gradient"
        );
        // Detail's r/3 blur is strictly narrower than r, so it adds nothing.
        let detailed = Sharpen {
            amount: 0.5,
            radius: 8,
            detail: 1.0,
            masking: 0.0,
        };
        assert_eq!(sharpen_halo(Some(detailed)), 8, "r dominates r/3");
        // Inactive sharpen contributes nothing regardless of the new fields.
        let inactive = Sharpen {
            amount: 0.0,
            radius: 8,
            detail: 1.0,
            masking: 1.0,
        };
        assert_eq!(sharpen_halo(Some(inactive)), 0);
    }

    /// An old sidecar (no `detail`/`masking` keys) must deserialize to the
    /// exact pre-P4 behavior — the back-compat half of gate 2.
    #[test]
    fn sharpen_deserializes_pre_p4_payload_as_identity_extras() {
        let old = r#"{"amount":0.5,"radius":8}"#;
        let s: Sharpen = serde_json::from_str(old).expect("pre-P4 payload must load");
        assert_eq!(s.amount, 0.5);
        assert_eq!(s.radius, 8);
        assert_eq!(s.detail, 0.0, "absent detail -> identity");
        assert_eq!(s.masking, 0.0, "absent masking -> identity");
    }

    #[test]
    fn nr_halo_is_total_atrous_support_or_zero() {
        use crate::local::NoiseReduction;
        assert_eq!(
            nr_halo(&NoiseReduction::default()),
            0,
            "identity -> no halo"
        );
        let active = NoiseReduction {
            luminance: 0.5,
            ..Default::default()
        };
        assert_eq!(nr_halo(&active), crate::nr::nr_halo_px());
        assert_eq!(nr_halo(&active), 62, "L=5 -> 2*(2^5-1)");
        // Chroma-only NR still needs the full halo (same decomposition).
        let chroma = NoiseReduction {
            color: 0.5,
            ..Default::default()
        };
        assert_eq!(nr_halo(&chroma), 62);
    }

    /// Final-review FIX 1: `detail`/`color_detail` alone (zero
    /// `luminance`/`color`) is non-identity but INACTIVE — every threshold it
    /// could scale is already zero, so it dispatches nothing and must
    /// contribute no halo. Before this fix `nr_halo` keyed off `is_identity`,
    /// so this exact state (the very next slider a user touches after
    /// Luminance) forced a full `TileEditPipeline` rebuild for zero effect.
    #[test]
    fn nr_halo_is_zero_for_detail_only_noise_reduction() {
        use crate::local::NoiseReduction;
        let detail_only = NoiseReduction {
            detail: 0.1,
            ..Default::default()
        };
        assert!(
            !detail_only.is_identity(),
            "sanity: detail alone is not is_identity"
        );
        assert!(
            !detail_only.is_active(),
            "detail alone cannot move any threshold off zero"
        );
        assert_eq!(
            nr_halo(&detail_only),
            0,
            "detail-only must contribute no halo"
        );

        let color_detail_only = NoiseReduction {
            color_detail: 0.1,
            ..Default::default()
        };
        assert_eq!(nr_halo(&color_detail_only), 0, "color_detail-only: same");
    }

    #[test]
    fn nr_halo_doc_is_zero_unless_the_global_set_has_nr() {
        use crate::local::{AdjustmentSet, MaskLayer, NoiseReduction};
        use crate::op::{EditDoc, Sharpen};
        use ferrolite_mask::MaskDefinition;

        assert_eq!(nr_halo_doc(&EditDoc::default()), 0);
        let mut doc = EditDoc::default();
        doc.global.noise_reduction.luminance = 0.4;
        assert_eq!(nr_halo_doc(&doc), 62, "global NR contributes the halo");

        // NR is GLOBAL-ONLY (design §3.5): a REAL, VISIBLE mask layer carrying
        // its own non-zero NR must NOT contribute a halo -- this is the actual
        // guard on the load-bearing constraint (a doc with no layers at all
        // would pass this trivially and prove nothing).
        let masked = EditDoc {
            layers: vec![MaskLayer {
                name: "l".into(),
                visible: true,
                mask: MaskDefinition::default(),
                adjustments: AdjustmentSet {
                    noise_reduction: NoiseReduction {
                        luminance: 0.9,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        assert_eq!(
            nr_halo_doc(&masked),
            0,
            "per-mask NR is not applied, so it must contribute no halo"
        );

        // Contrast: the identical layer shape, but carrying an active sharpen
        // instead, DOES contribute via `sharpen_halo_doc` -- proving the zero
        // above is a deliberate global-only choice for NR specifically, not
        // an artifact of a doc whose layers never get walked at all.
        let sharpened = EditDoc {
            layers: vec![MaskLayer {
                name: "l".into(),
                visible: true,
                mask: MaskDefinition::default(),
                adjustments: AdjustmentSet {
                    sharpen: Sharpen {
                        amount: 0.5,
                        radius: 4,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        assert_eq!(sharpen_halo_doc(&sharpened), 4);
    }

    #[test]
    fn nr_uniform_is_inactive_at_identity() {
        use crate::local::NoiseReduction;
        let u = nr_uniform(&NoiseReduction::default(), 0);
        assert_eq!(u.active, 0);
        let u = nr_uniform(
            &NoiseReduction {
                luminance: 0.5,
                ..Default::default()
            },
            0,
        );
        assert_eq!(u.active, 1);
    }

    #[test]
    fn nr_uniform_carries_the_levels_spacing_and_thresholds() {
        use crate::local::NoiseReduction;
        let nr = NoiseReduction {
            luminance: 1.0,
            detail: 0.0,
            color: 0.5,
            color_detail: 0.0,
        };
        for level in 0..crate::nr::NR_LEVELS {
            let u = nr_uniform(&nr, level);
            assert_eq!(u.level, level as i32);
            assert_eq!(u.spacing, 1 << level, "spacing = 2^level");
            assert!(
                (u.thresholds[0] - crate::nr::threshold_at(1.0, 0.0, level)).abs() < 1e-9,
                "luma threshold at level {level}"
            );
            assert!(
                (u.thresholds[1] - crate::nr::threshold_at(0.5, 0.0, level)).abs() < 1e-9,
                "chroma threshold at level {level}"
            );
        }
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
        let (u, w, h) = geometry_uniform(
            Some(Geometry {
                crop: CropRect {
                    x: 0.25,
                    y: 0.25,
                    w: 0.5,
                    h: 0.5,
                },
                angle_deg: 0.0,
                aspect: Aspect::Free,
                ..Default::default()
            }),
            64,
            48,
        );
        assert_eq!((w, h), (32, 24));
        // Exact-pixel crop: crop_w_px/crop_h_px are already integers (32.0,
        // 24.0), so out_w/out_h round with zero remainder. The rounded-dims
        // fix must be a no-op here -- off is just the crop origin (16, 12),
        // identical to what the pre-fix formula also produced for this case.
        assert_eq!(u.off, [16.0, 12.0]);
    }

    #[test]
    fn geometry_uniform_rotation_sets_rotation_matrix() {
        use crate::op::{Aspect, CropRect, Geometry};
        let (u, _, _) = geometry_uniform(
            Some(Geometry {
                crop: CropRect::full(),
                angle_deg: 90.0,
                aspect: Aspect::Original,
                ..Default::default()
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
    fn geometry_uniform_fractional_crop_stays_in_bounds() {
        // Root cause (spec C2, part 1): out_w/out_h are rounded to whole
        // pixels, but the sampling matrix/offset must pivot around a crop
        // center derived from THOSE ROUNDED dims, not the fractional
        // crop_w_px/crop_h_px -- otherwise every output texel center is off
        // by the rounding remainder (up to ~0.5 source px), smearing the
        // last row/column outward past the true crop extent.
        use crate::op::{Aspect, CropRect, Geometry};

        let src_w = 4001u32;
        let src_h = 2999u32;
        let sw = src_w as f32;
        let sh = src_h as f32;
        // Deliberately fractional: crop_w_px/crop_h_px round with a
        // non-trivial remainder in both dimensions.
        let cx = 0.1003_f32;
        let cy = 0.2001_f32;
        let cw = 0.4997_f32;
        let ch = 0.5993_f32;

        for angle_deg in [0.0_f32, 12.5, -30.0] {
            let geo = Geometry {
                crop: CropRect {
                    x: cx,
                    y: cy,
                    w: cw,
                    h: ch,
                },
                angle_deg,
                aspect: Aspect::Free,
                ..Default::default()
            };
            let (u, out_w, out_h) = geometry_uniform(Some(geo), src_w, src_h);
            let out_w = out_w as f32;
            let out_h = out_h as f32;

            // The true pivot: crop origin + half of the ROUNDED output
            // extent (this is exactly what the fix must derive `m`/`off`
            // from), rotated by the same matrix the uniform reports.
            let ideal_center = [cx * sw + out_w * 0.5, cy * sh + out_h * 0.5];
            let theta = angle_deg.to_radians();
            let (s, c) = theta.sin_cos();
            let r = [c, -s, s, c]; // same row-major convention as `m`

            for &(ox, oy) in &[
                (0.0_f32, 0.0_f32),
                (out_w - 1.0, 0.0),
                (0.0, out_h - 1.0),
                (out_w - 1.0, out_h - 1.0),
            ] {
                // Output texel center, local to the output rect's own center.
                let local = [ox + 0.5 - out_w * 0.5, oy + 0.5 - out_h * 0.5];
                let ideal_src = [
                    ideal_center[0] + r[0] * local[0] + r[1] * local[1],
                    ideal_center[1] + r[2] * local[0] + r[3] * local[1],
                ];

                // What geometry_uniform's actual m/off place this corner at
                // (mirrors the WGSL shader's `sx = m.x*po.x + m.y*po.y + off.x`).
                let po = [ox + 0.5, oy + 0.5];
                let actual_src = [
                    u.m[0] * po[0] + u.m[1] * po[1] + u.off[0],
                    u.m[2] * po[0] + u.m[3] * po[1] + u.off[1],
                ];

                let dx = (actual_src[0] - ideal_src[0]).abs();
                let dy = (actual_src[1] - ideal_src[1]).abs();
                // Design ceiling is half a source texel; the fixed formula
                // should land within float epsilon of the ideal pivot.
                assert!(
                    dx <= 0.5 && dy <= 0.5,
                    "angle {angle_deg}: corner ({ox},{oy}) drifted ({dx}, {dy}) \
                     source px from the rounded-dims pivot"
                );
                assert!(
                    dx < 1e-3 && dy < 1e-3,
                    "angle {angle_deg}: corner ({ox},{oy}) off by ({dx}, {dy}) \
                     source px -- m/off must derive from the ROUNDED out_w/out_h, \
                     not the fractional crop_w_px/crop_h_px"
                );
            }
        }
    }

    /// Regression (crop display bug, 2026-07): every consumer of the
    /// geometry OUTPUT dims must derive the exact same WxH for the same
    /// (fractional) crop. The consumers, and the path each takes:
    ///
    /// * `geometry_uniform`'s returned `(out_w, out_h)` — baked into
    ///   `TileEditPipeline` (`out_dims()`, whose value the app pushes into
    ///   the sparse VT's logical size) via `edited_output_dims`.
    /// * the uniform's `out_dims: [f32; 2]` field — what the preview
    ///   `EditPipeline`'s geometry node allocates its output texture from
    ///   (`nodes.rs`: `u.out_dims[0] as u32`), i.e. the preview-tier dims.
    ///
    /// A 1px disagreement between any two (round vs truncate vs a second
    /// independent rounding) makes the preview and full tiers place the image
    /// differently, which presents as pan/zoom flicker and a wrong crop at
    /// rest on cropped images.
    #[test]
    fn fractional_crop_all_dims_consumers_agree() {
        use crate::op::{Aspect, CropRect, Geometry, Op, OpStack};

        // Sources + crops chosen so crop_w_px/crop_h_px land on awkward
        // fractions, including exact .5 (round-half) and near-integer cases.
        let cases: &[((u32, u32), CropRect, f32)] = &[
            // 0.500125 * 4000 = 2000.5 — exact round-half in width.
            (
                (4000, 3000),
                CropRect {
                    x: 0.1,
                    y: 0.1,
                    w: 0.500_125,
                    h: 0.333_4,
                },
                0.0,
            ),
            // Both axes fractional, odd source dims, with rotation.
            (
                (4001, 2999),
                CropRect {
                    x: 0.1003,
                    y: 0.2001,
                    w: 0.4997,
                    h: 0.5993,
                },
                12.5,
            ),
            // Near-integer remainder just below .5 (must round DOWN everywhere).
            (
                (6000, 4000),
                CropRect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.333_39,
                    h: 0.666_61,
                },
                -30.0,
            ),
        ];

        for &((src_w, src_h), crop, angle_deg) in cases {
            let geo = Geometry {
                crop,
                angle_deg,
                aspect: Aspect::Free,
                ..Default::default()
            };

            // Consumer 1: geometry_uniform's rounded ints (the tile/full tier).
            let (u, out_w, out_h) = geometry_uniform(Some(geo), src_w, src_h);

            // Consumer 2: the uniform's f32 out_dims field cast back the way
            // the preview geometry node does when allocating its output.
            let preview_w = (u.out_dims[0] as u32).max(1);
            let preview_h = (u.out_dims[1] as u32).max(1);

            // Consumer 3: edited_output_dims (export + sparse-VT logical size).
            let stack = OpStack::default().set_op(Op::Geometry(geo));
            let export = crate::edited_output_dims(&stack, src_w, src_h);

            assert_eq!(
                (preview_w, preview_h),
                (out_w, out_h),
                "src {src_w}x{src_h} crop {crop:?}: preview-tier dims (from the \
                 uniform's out_dims field) must equal geometry_uniform's rounded ints"
            );
            assert_eq!(
                export,
                (out_w, out_h),
                "src {src_w}x{src_h} crop {crop:?}: edited_output_dims must equal \
                 geometry_uniform's rounded ints"
            );
            // The crop always stays within the source, so the rounded output
            // extent can never exceed it — the invariant that lets the sparse
            // VT shrink its logical size in place (the full-source tile grid
            // stays a superset).
            assert!(
                out_w <= src_w && out_h <= src_h,
                "src {src_w}x{src_h} crop {crop:?}: out {out_w}x{out_h} exceeds source"
            );
        }
    }

    #[test]
    fn geometry_uniform_default_out_origin_is_zero() {
        let (u, _, _) = geometry_uniform(None, 64, 48);
        assert_eq!(u.out_origin, [0.0, 0.0]);
    }

    #[test]
    fn geometry_uniform_size_is_16_byte_aligned() {
        // WGSL uniform buffers require the whole struct's size to be a
        // multiple of its largest member's alignment (16, from `m: vec4`).
        assert_eq!(std::mem::size_of::<GeometryUniform>() % 16, 0);
        // Field offsets MIRROR `struct P` in geometry.wgsl exactly (vec4
        // align 16, vec2 align 8 — repr(C) f32 arrays land on the same
        // offsets with no implicit padding).
        assert_eq!(std::mem::size_of::<GeometryUniform>(), 112);
        assert_eq!(std::mem::offset_of!(GeometryUniform, m), 0);
        assert_eq!(std::mem::offset_of!(GeometryUniform, off), 16);
        assert_eq!(std::mem::offset_of!(GeometryUniform, src_dims), 24);
        assert_eq!(std::mem::offset_of!(GeometryUniform, out_dims), 32);
        assert_eq!(std::mem::offset_of!(GeometryUniform, out_origin), 40);
        assert_eq!(std::mem::offset_of!(GeometryUniform, crop_bounds), 48);
        assert_eq!(std::mem::offset_of!(GeometryUniform, h0), 64);
        assert_eq!(std::mem::offset_of!(GeometryUniform, h1), 80);
        assert_eq!(std::mem::offset_of!(GeometryUniform, h2), 96);
    }

    #[test]
    fn geometry_uniform_full_frame_crop_bounds_is_half_texel_inset() {
        // Spec C2 part 2: a full-frame (no crop) `Geometry` must populate
        // `crop_bounds` with the half-texel-inset FULL rect -- exactly what a
        // ClampToEdge + bilinear sampler already computes internally for an
        // out-of-[0,1] coordinate -- so un-cropped rendering (including
        // rotation past the frame edge) is byte-identical to before this
        // field existed.
        let (u, _, _) = geometry_uniform(None, 64, 48);
        let half_u = 0.5 / 64.0;
        let half_v = 0.5 / 48.0;
        assert!((u.crop_bounds[0] - half_u).abs() < 1e-6);
        assert!((u.crop_bounds[1] - half_v).abs() < 1e-6);
        assert!((u.crop_bounds[2] - (1.0 - half_u)).abs() < 1e-6);
        assert!((u.crop_bounds[3] - (1.0 - half_v)).abs() < 1e-6);
    }

    #[test]
    fn geometry_uniform_clamps_base_uv_to_crop_not_frame() {
        // The bug (spec C2 part 2): the geometry sampler used to clamp
        // `base_uv` against the WHOLE source texture, so a rotated crop's
        // out-of-bounds corners read (and, past the true frame edge,
        // duplicated) content from the FRAME's edge rather than the crop's
        // own edge. This CPU reference mirrors the WGSL clamp
        // (`clamp_uv_to_crop_bounds`) exactly and asserts the clamped
        // coordinate lands on the CROP rect's edge, not at the whole-texture
        // edge (0.0/1.0).
        use crate::op::{Aspect, CropRect, Geometry};

        let src_w = 200u32;
        let src_h = 200u32;
        let sw = src_w as f32;
        let sh = src_h as f32;

        // A crop with real margin on every side (60px on each edge of a
        // 200x200 frame) rotated 45 degrees, so a corner maps outside the
        // crop rect while remaining well inside the whole frame -- i.e. this
        // is NOT the old "reaches past the frame edge" case; it isolates the
        // "clamp to crop, not frame" behavior change on its own.
        let geo = Geometry {
            crop: CropRect {
                x: 0.3,
                y: 0.3,
                w: 0.4,
                h: 0.4,
            },
            angle_deg: 45.0,
            aspect: Aspect::Free,
            ..Default::default()
        };
        let (u, _, _) = geometry_uniform(Some(geo), src_w, src_h);

        // The output's top-left texel center, mapped through the uniform's
        // own m/off (mirrors the WGSL `sx`/`sy` computation exactly).
        let po = [0.5_f32, 0.5];
        let sx = u.m[0] * po[0] + u.m[1] * po[1] + u.off[0];
        let sy = u.m[2] * po[0] + u.m[3] * po[1] + u.off[1];
        let raw_uv = [sx / sw, sy / sh];

        // Sanity: this corner's raw coordinate must actually fall outside the
        // crop rect (below `min_v`) for the assertion below to be meaningful,
        // while still landing inside the full [0,1] frame -- proving the
        // clamp is doing real work even with frame slack to spare.
        assert!(
            raw_uv[1] < u.crop_bounds[1],
            "test setup: expected the rotated corner ({raw_uv:?}) below the \
             crop's min_v ({}); adjust the fixture",
            u.crop_bounds[1]
        );
        assert!(
            raw_uv[1] > 0.0,
            "test setup: corner should stay inside the whole frame (raw {raw_uv:?})"
        );

        let clamped = clamp_uv_to_crop_bounds(raw_uv, u.crop_bounds);

        // Clamped to the CROP's own edge...
        assert!(
            (clamped[1] - u.crop_bounds[1]).abs() < 1e-6,
            "clamped v {} did not land on the crop edge {}",
            clamped[1],
            u.crop_bounds[1]
        );
        // ...NOT the whole source texture's edge (0.0) -- the bug this task
        // fixes.
        assert!(
            clamped[1] > 0.01,
            "clamped to the FRAME edge (~0.0) instead of the crop edge ({})",
            u.crop_bounds[1]
        );
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

    // ── Plan `crop-overhaul` C4 Task 5: keystone homography ──

    /// Source-px x-span the output row at `y_px` samples, edge to edge.
    fn sampled_span_x(u: &GeometryUniform, y_px: f32, out_w: f32) -> f32 {
        let l = geometry_src_px(u, [0.0, y_px])[0];
        let r = geometry_src_px(u, [out_w, y_px])[0];
        r - l
    }

    /// Source-px y-span the output column at `x_px` samples, edge to edge.
    fn sampled_span_y(u: &GeometryUniform, x_px: f32, out_h: f32) -> f32 {
        let t = geometry_src_px(u, [x_px, 0.0])[1];
        let b = geometry_src_px(u, [x_px, out_h])[1];
        b - t
    }

    #[test]
    fn geometry_homography_zero_keystone_is_bit_identical_to_affine() {
        // Brief test (a): with keystone == 0 the homography rows must carry
        // EXACTLY the affine [m|off] extension (h2 = [0,0,1,0] bit-exact, so
        // the perspective divide is by exactly 1.0) and the mapping must
        // reproduce the pre-keystone affine `m·po + off` BIT-identically for
        // a grid of output coords — this is what guards every existing
        // zero-keystone golden.
        use crate::op::{Aspect, CropRect, Geometry};
        let geo = Geometry {
            crop: CropRect {
                x: 0.1003,
                y: 0.2001,
                w: 0.4997,
                h: 0.5993,
            },
            angle_deg: 12.5,
            aspect: Aspect::Free,
            ..Default::default()
        };
        let (u, out_w, out_h) = geometry_uniform(Some(geo), 4001, 2999);
        assert_eq!(
            u.h2.map(f32::to_bits),
            [0.0f32, 0.0, 1.0, 0.0].map(f32::to_bits),
            "zero keystone: h2 must be exactly [0,0,1,0] (not even -0.0)"
        );
        assert_eq!(
            u.h0.map(f32::to_bits)[..3],
            [u.m[0], u.m[1], u.off[0]].map(f32::to_bits)
        );
        assert_eq!(
            u.h1.map(f32::to_bits)[..3],
            [u.m[2], u.m[3], u.off[1]].map(f32::to_bits)
        );

        // Grid of output coords, including tile-style negative/out-of-rect
        // coords (`po = out_origin + gid + 0.5` may leave the output rect on
        // the haloed tile path).
        for iy in -2..=9 {
            for ix in -2..=9 {
                let po = [
                    ix as f32 / 8.0 * out_w as f32 + 0.5,
                    iy as f32 / 8.0 * out_h as f32 + 0.5,
                ];
                let affine = [
                    u.m[0] * po[0] + u.m[1] * po[1] + u.off[0],
                    u.m[2] * po[0] + u.m[3] * po[1] + u.off[1],
                ];
                let hom = geometry_src_px(&u, po);
                assert_eq!(
                    hom.map(f32::to_bits),
                    affine.map(f32::to_bits),
                    "zero-keystone homography drifted from the affine at po {po:?}"
                );
            }
        }
    }

    #[test]
    fn keystone_v_positive_widens_top_sampled_span() {
        // Brief test (b), the pinned sign convention: kv > 0 must map the TOP
        // output edge's sampled x-span WIDER than the bottom's (converging
        // verticals corrected). Quantitatively (full frame, no rotation): the
        // top edge's corners are displaced outward by 0.5·K·kv each, so its
        // span is (1 + K·kv)·out_w while the bottom edge stays out_w.
        use crate::op::Geometry;
        let kv = 0.5f32;
        let geo = Geometry {
            keystone_v: kv,
            ..Default::default()
        };
        let (u, out_w, out_h) = geometry_uniform(Some(geo), 64, 48);
        let ow = out_w as f32;
        let top = sampled_span_x(&u, 0.0, ow);
        let bottom = sampled_span_x(&u, out_h as f32, ow);
        assert!(
            top > bottom,
            "kv > 0 must widen the TOP edge's sampled span (top {top}, bottom {bottom})"
        );
        assert!((top - ow * (1.0 + KEYSTONE_STRENGTH * kv)).abs() < 1e-3);
        assert!((bottom - ow).abs() < 1e-3);

        // And the mirror: kv < 0 widens the BOTTOM edge instead.
        let geo_neg = Geometry {
            keystone_v: -kv,
            ..Default::default()
        };
        let (u_neg, _, _) = geometry_uniform(Some(geo_neg), 64, 48);
        let top_neg = sampled_span_x(&u_neg, 0.0, ow);
        let bottom_neg = sampled_span_x(&u_neg, out_h as f32, ow);
        assert!(
            bottom_neg > top_neg,
            "kv < 0 must widen the BOTTOM edge's sampled span"
        );
    }

    #[test]
    fn keystone_h_positive_widens_left_sampled_span() {
        // keystone_h is keystone_v transposed: kh > 0 widens the LEFT output
        // edge's sampled y-span (and kh < 0 the right's).
        use crate::op::Geometry;
        let kh = 0.5f32;
        let geo = Geometry {
            keystone_h: kh,
            ..Default::default()
        };
        let (u, out_w, out_h) = geometry_uniform(Some(geo), 64, 48);
        let oh = out_h as f32;
        let left = sampled_span_y(&u, 0.0, oh);
        let right = sampled_span_y(&u, out_w as f32, oh);
        assert!(
            left > right,
            "kh > 0 must widen the LEFT edge's sampled span (left {left}, right {right})"
        );
        assert!((left - oh * (1.0 + KEYSTONE_STRENGTH * kh)).abs() < 1e-3);
        assert!((right - oh).abs() < 1e-3);

        let geo_neg = Geometry {
            keystone_h: -kh,
            ..Default::default()
        };
        let (u_neg, _, _) = geometry_uniform(Some(geo_neg), 64, 48);
        assert!(
            sampled_span_y(&u_neg, out_w as f32, oh) > sampled_span_y(&u_neg, 0.0, oh),
            "kh < 0 must widen the RIGHT edge's sampled span"
        );
    }

    #[test]
    fn keystone_combined_corners_compose_in_one_solve() {
        // Combined kv + kh: BOTH displacement sets apply to the four corners
        // of ONE 4-point solve (not two multiplied homographies). With crop +
        // rotation composed in, each output-rect corner must land exactly on
        // the affine (m/off) image of its displaced crop-local corner.
        use crate::op::{Aspect, CropRect, Geometry};
        let (kv, kh) = (0.5f32, -0.3f32);
        let geo = Geometry {
            crop: CropRect {
                x: 0.1,
                y: 0.1,
                w: 0.8,
                h: 0.8,
            },
            angle_deg: 10.0,
            aspect: Aspect::Free,
            keystone_v: kv,
            keystone_h: kh,
        };
        let (u, out_w, out_h) = geometry_uniform(Some(geo), 64, 48);
        let (ow, oh) = (out_w as f32, out_h as f32);
        let half = 0.5 * KEYSTONE_STRENGTH;
        let (dt, db) = (kv.max(0.0) * half, (-kv).max(0.0) * half);
        let (dl, dr) = (kh.max(0.0) * half, (-kh).max(0.0) * half);
        let corners = [
            ([0.0f32, 0.0f32], [-dt, -dl]),
            ([1.0, 0.0], [1.0 + dt, -dr]),
            ([0.0, 1.0], [-db, 1.0 + dl]),
            ([1.0, 1.0], [1.0 + db, 1.0 + dr]),
        ];
        for (out_n, q) in corners {
            let po = [out_n[0] * ow, out_n[1] * oh];
            let q_px = [q[0] * ow, q[1] * oh];
            let expected = [
                u.m[0] * q_px[0] + u.m[1] * q_px[1] + u.off[0],
                u.m[2] * q_px[0] + u.m[3] * q_px[1] + u.off[1],
            ];
            let got = geometry_src_px(&u, po);
            assert!(
                (got[0] - expected[0]).abs() < 1e-3 && (got[1] - expected[1]).abs() < 1e-3,
                "corner {out_n:?}: got {got:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn keystone_quad_homography_maps_unit_corners_to_displaced_corners() {
        // Validates the closed-form solve itself: W applied projectively to
        // each unit-square corner must reproduce the displaced corner.
        let (kv, kh) = (0.7f32, -0.4f32);
        let w = keystone_quad_homography(kv, kh);
        let apply = |p: [f32; 2]| -> [f32; 2] {
            let x = w[0][0] * p[0] + w[0][1] * p[1] + w[0][2];
            let y = w[1][0] * p[0] + w[1][1] * p[1] + w[1][2];
            let z = w[2][0] * p[0] + w[2][1] * p[1] + w[2][2];
            [x / z, y / z]
        };
        let half = 0.5 * KEYSTONE_STRENGTH;
        let (dt, db) = (kv.max(0.0) * half, (-kv).max(0.0) * half);
        let (dl, dr) = (kh.max(0.0) * half, (-kh).max(0.0) * half);
        let cases = [
            ([0.0f32, 0.0f32], [-dt, -dl]),
            ([1.0, 0.0], [1.0 + dt, -dr]),
            ([0.0, 1.0], [-db, 1.0 + dl]),
            ([1.0, 1.0], [1.0 + db, 1.0 + dr]),
        ];
        for (corner, expected) in cases {
            let got = apply(corner);
            assert!(
                (got[0] - expected[0]).abs() < 1e-5 && (got[1] - expected[1]).abs() < 1e-5,
                "unit corner {corner:?}: got {got:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn keystone_tile_uniform_inherits_homography_rows() {
        // The tile head spreads `..base`, so the keystone homography (built
        // from the FULL output dims) must ride along unchanged; only
        // out_dims/out_origin differ.
        use crate::op::Geometry;
        let geo = Geometry {
            keystone_v: 0.5,
            keystone_h: -0.3,
            ..Default::default()
        };
        let (base, _, _) = geometry_uniform(Some(geo), 600, 500);
        let tile = geometry_tile_uniform(Some(geo), 600, 500, (254.0, -2.0), 260);
        assert_eq!(tile.h0, base.h0);
        assert_eq!(tile.h1, base.h1);
        assert_eq!(tile.h2, base.h2);
        assert_eq!(tile.out_origin, [254.0, -2.0]);
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
                ..Default::default()
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
        // formula), so it never enters the luma-mix math (nor, historically,
        // the rgb2hsl/hsl2rgb round trip it once used). Compared with the
        // same tolerance as `light_color_identity_is_a_no_op` (the
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
    /// (1.74, 0.17, 0.17) has HSL s ≈ 18.7 under the old HSL-based formula).
    /// history: an earlier version weighted the `(1 - s)` fade term by the RAW
    /// (unclamped) HSL saturation, so at s ≈ 18.7 the factor `1 + v*(1-s)` went
    /// hugely negative for any non-zero vibrance, snapping the pixel to flat
    /// grey — worse (more grey) for negative vibrance, i.e. negative vibrance
    /// BOOSTED such a pixel's spread instead of reducing it. A later revision
    /// clamped that fade weight to `[0,1]`, which fixed the grey-snap but kept
    /// the HSL round trip — and inherited ITS OWN singularity: HSL saturation's
    /// `s = d / (1 - |2l-1|)` denominator hits exactly 0 at `l == 1.0` (Inf/NaN,
    /// the "black pixel in bright sky" bug this fix addresses). The CURRENT
    /// implementation drops the HSL round trip entirely in favor of an
    /// HSV-style measure `(max-min)/max(max, eps)` (see `hsv_sat_measure`),
    /// which is bounded in `[0,1]` for any brightness with no denominator that
    /// can reach zero — so the pixel's channel spread degrades gracefully
    /// rather than collapsing, for every scene-linear brightness, not just the
    /// ones this fixture happens to cover.
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

    /// ROOT-CAUSE CONFIRMATION (pre-fix): `rgb_to_hsl`'s `s = d / (1 - |2l-1|)`
    /// has a removable-singularity denominator at `l == 1.0` (a scene-linear
    /// bright pixel, e.g. an overexposed sky sample) and goes NEGATIVE at
    /// `l > 1.0`. Vibrance's rgb2hsl/hsl2rgb round trip inherits both: at
    /// `l == 1` the denominator is exactly 0, so `s` is `Inf`/`NaN`, and the
    /// vibrance formula's `s' = s * (1 + v*(1-w))` stays non-finite through
    /// `hsl_to_rgb`, which is stored to the GPU texture as NaN and rendered as
    /// black. This test is the failing reproduction demanding finite output
    /// everywhere; it must be green only once vibrance no longer round-trips
    /// through HSL for its saturation measure.
    #[test]
    fn vibrance_is_finite_on_l_equals_one_and_l_greater_than_one_pixels() {
        use crate::local::AdjustmentSet;

        let a = AdjustmentSet {
            vibrance: 0.3,
            ..Default::default()
        };

        // l == (1.05+0.95)/2 == 1.0 exactly -> pre-fix denominator (1-|2l-1|) == 0.
        // `global_order = true` is the relevant call shape here: it's the path the
        // GLOBAL Vibrance slider actually takes (`color_segment()`'s pseudo-layer
        // dispatch, full coverage, global order) and — critically — it is the ONLY
        // path that skips the final `max(0.0)` floor clamp, so a NaN produced here
        // reaches the GPU texture unmasked. The mask-order path (`global_order =
        // false`) launders the same NaN to 0.0 via that floor clamp (`NaN.max(0.0)
        // == 0.0` in Rust/IEEE-754), which is finite but is the same underlying
        // black-pixel bug wearing a different hat — so both are asserted.
        let l_eq_1 = [1.05, 0.95, 0.97];
        let out_l_eq_1_global = light_color_apply(l_eq_1, &a, true);
        let out_l_eq_1_mask = light_color_apply(l_eq_1, &a, false);
        assert!(
            out_l_eq_1_global.iter().all(|v| v.is_finite()),
            "l==1.0 pixel must stay finite through vibrance (global order): \
             in={l_eq_1:?} out={out_l_eq_1_global:?}"
        );
        assert!(
            out_l_eq_1_mask.iter().all(|v| v.is_finite()),
            "l==1.0 pixel must stay finite through vibrance (mask order): \
             in={l_eq_1:?} out={out_l_eq_1_mask:?}"
        );

        // l == (1.4+1.2)/2 == 1.3 > 1.0 -> pre-fix denominator negative -> s negative.
        let l_gt_1 = [1.4, 1.2, 1.3];
        let out_l_gt_1_global = light_color_apply(l_gt_1, &a, true);
        let out_l_gt_1_mask = light_color_apply(l_gt_1, &a, false);
        assert!(
            out_l_gt_1_global.iter().all(|v| v.is_finite()),
            "l>1.0 pixel must stay finite through vibrance (global order): \
             in={l_gt_1:?} out={out_l_gt_1_global:?}"
        );
        assert!(
            out_l_gt_1_mask.iter().all(|v| v.is_finite()),
            "l>1.0 pixel must stay finite through vibrance (mask order): \
             in={l_gt_1:?} out={out_l_gt_1_mask:?}"
        );
    }

    /// ROOT-CAUSE CONFIRMATION (pre-fix, hue path): the pre-existing hue step
    /// shares the SAME `rgb_to_hsl`/`hsl_to_rgb` round trip and therefore the
    /// same `l==1.0` (Inf saturation) / `l>1.0` (negative saturation) singularity
    /// — it does not need `vibrance` set at all, a non-zero `hue` alone is enough.
    #[test]
    fn hue_is_finite_on_l_equals_one_and_l_greater_than_one_pixels() {
        use crate::local::AdjustmentSet;

        let a = AdjustmentSet {
            hue: 0.1, // -> hue_deg = 0.1 * 180 = 18 deg
            ..Default::default()
        };

        let l_eq_1 = [1.05, 0.95, 0.97];
        let out_l_eq_1_global = light_color_apply(l_eq_1, &a, true);
        let out_l_eq_1_mask = light_color_apply(l_eq_1, &a, false);
        assert!(
            out_l_eq_1_global.iter().all(|v| v.is_finite()),
            "l==1.0 pixel must stay finite through hue (global order): \
             in={l_eq_1:?} out={out_l_eq_1_global:?}"
        );
        assert!(
            out_l_eq_1_mask.iter().all(|v| v.is_finite()),
            "l==1.0 pixel must stay finite through hue (mask order): \
             in={l_eq_1:?} out={out_l_eq_1_mask:?}"
        );

        let l_gt_1 = [1.4, 1.2, 1.3];
        let out_l_gt_1_global = light_color_apply(l_gt_1, &a, true);
        let out_l_gt_1_mask = light_color_apply(l_gt_1, &a, false);
        assert!(
            out_l_gt_1_global.iter().all(|v| v.is_finite()),
            "l>1.0 pixel must stay finite through hue (global order): \
             in={l_gt_1:?} out={out_l_gt_1_global:?}"
        );
        assert!(
            out_l_gt_1_mask.iter().all(|v| v.is_finite()),
            "l>1.0 pixel must stay finite through hue (mask order): \
             in={l_gt_1:?} out={out_l_gt_1_mask:?}"
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
