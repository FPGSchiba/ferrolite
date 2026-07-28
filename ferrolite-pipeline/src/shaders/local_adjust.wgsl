// Local Light+Color point op, blended by a mask. Mirrors uniforms::light_color_apply
// exactly. `dst[xy] = mix(src[xy], adjusted(src[xy]), mask[xy])`, so a mask value of 0
// leaves the pixel untouched and 1 applies the full adjustment. The mask is composited
// at the SAME resolution as `src` (whole image for preview, one tile for the tiled
// tier), so it is sampled 1:1 with no origin/LOD offset.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var mask: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;
struct P {
    exposure_gain: f32, contrast_gain: f32, highlights: f32, shadows: f32,
    whites: f32, blacks: f32, saturation: f32, hue_deg: f32,
    wb_mul: vec3<f32>, color_amount: f32,
    color_rgb: vec3<f32>, contrast_pivot: f32,
    // Phase 2b: per-layer curve/HSL/grade (identity when the layer leaves them
    // default). Field order MIRRORS `uniforms::LocalAdjustUniform` exactly.
    hsl_bands: array<vec4<f32>, 8>,
    grade_shadows: vec4<f32>,
    grade_midtones: vec4<f32>,
    grade_highlights: vec4<f32>,
    grade_global: vec4<f32>,
    grade_params: vec4<f32>,
    // x = curve active, y = hsl active, z = grade active, w = pad.
    active_flags: vec4<f32>,
    // Phase 3 (fused layer engine): x = 1.0 global order (WB before contrast),
    // 0.0 mask order (contrast before WB, the historical default); y = 1.0
    // force full coverage (skip the mask sample entirely, m = 1.0); z =
    // vibrance amount (0 = identity); w = pad.
    order_and_coverage: vec4<f32>,
};
@group(0) @binding(3) var<uniform> p: P;
// Phase 2b: per-layer 3x256 tone-curve LUT (R,G,B rows), same packing + binding
// style as `tone_curve.wgsl`'s LUT (a fresh small storage buffer per layer).
@group(0) @binding(4) var<storage, read> lut: array<f32, 768>;

fn luma709(c: vec3<f32>) -> f32 { return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722)); }

fn rgb2hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b)); let mn = min(c.r, min(c.g, c.b));
    let l = (mx + mn) * 0.5; let d = mx - mn;
    var h = 0.0; var s = 0.0;
    if (d > 1e-6) {
        s = d / (1.0 - abs(2.0 * l - 1.0));
        if (mx == c.r) { h = ((c.g - c.b) / d) % 6.0; }
        else if (mx == c.g) { h = (c.b - c.r) / d + 2.0; }
        else { h = (c.r - c.g) / d + 4.0; }
        h = h * 60.0; if (h < 0.0) { h = h + 360.0; }
    }
    return vec3<f32>(h, s, l);
}
fn hue2rgb(pp: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in; if (t < 0.0) { t = t + 1.0; } if (t > 1.0) { t = t - 1.0; }
    if (t < 1.0 / 6.0) { return pp + (q - pp) * 6.0 * t; }
    if (t < 1.0 / 2.0) { return q; }
    if (t < 2.0 / 3.0) { return pp + (q - pp) * (2.0 / 3.0 - t) * 6.0; }
    return pp;
}
fn hsl2rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x / 360.0; let s = hsl.y; let l = hsl.z;
    if (s <= 1e-6) { return vec3<f32>(l, l, l); }
    var q = l + s - l * s; if (l < 0.5) { q = l * (1.0 + s); }
    let pp = 2.0 * l - q;
    return vec3<f32>(hue2rgb(pp, q, h + 1.0 / 3.0), hue2rgb(pp, q, h), hue2rgb(pp, q, h - 1.0 / 3.0));
}

// ── Phase 2b: per-layer tone curve, ported verbatim from tone_curve.wgsl's
// `apply_lut` (same clamping/extrapolation), reading THIS pass's per-layer LUT.
fn apply_lut(v: f32, ch: u32) -> f32 {
    let base = ch * 256u;
    if (v < 0.0) { return lut[base] + v; }
    if (v > 1.0) { return lut[base + 255u] + (v - 1.0); }
    let x = v * 255.0;
    let i0 = u32(floor(x));
    let i1 = min(i0 + 1u, 255u);
    let f = x - floor(x);
    return mix(lut[base + i0], lut[base + i1], f);
}

fn curve_sample(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(apply_lut(c.r, 0u), apply_lut(c.g, 1u), apply_lut(c.b, 2u));
}

// ── Phase 2b: per-layer 8-band HSL, ported verbatim from hsl.wgsl (same
// constants/falloff/out-of-gamut excess bypass), reading `p.hsl_bands`.
const MAX_HUE_SHIFT: f32 = 30.0; // degrees per unit band.hue

fn band_center(i: u32) -> f32 {
    var centers = array<f32, 8>(0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0);
    return centers[i];
}

fn band_weight(hue: f32, center: f32) -> f32 {
    var d = abs(hue - center);
    if (d > 180.0) { d = 360.0 - d; }
    return max(0.0, 1.0 - d / 60.0);
}

fn hsl_bands_apply(c: vec3<f32>) -> vec3<f32> {
    let in_gamut = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let excess = c - in_gamut;
    let hsl = rgb2hsl(in_gamut);

    var hue_acc = 0.0;
    var sat_acc = 0.0;
    var lum_acc = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let w = band_weight(hsl.x, band_center(i));
        hue_acc = hue_acc + w * p.hsl_bands[i].x;
        sat_acc = sat_acc + w * p.hsl_bands[i].y;
        lum_acc = lum_acc + w * p.hsl_bands[i].z;
    }

    var out_hsl = hsl;
    out_hsl.x = hsl.x + hue_acc * MAX_HUE_SHIFT;
    if (out_hsl.x < 0.0) { out_hsl.x = out_hsl.x + 360.0; }
    if (out_hsl.x >= 360.0) { out_hsl.x = out_hsl.x - 360.0; }
    out_hsl.y = clamp(hsl.y * (1.0 + sat_acc), 0.0, 1.0);
    out_hsl.z = clamp(hsl.z * (1.0 + lum_acc), 0.0, 1.0);

    return hsl2rgb(out_hsl) + excess;
}

// ── Phase 2b: per-layer color grade, ported verbatim from color_grade.wgsl's
// per-pixel kernel, reading `p.grade_*` (tints/lum pre-scaled on the CPU).
fn smoothstep_f(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn grade_apply(c: vec3<f32>) -> vec3<f32> {
    let y = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let pivot = 0.5 + 0.5 * p.grade_params.y;   // balance
    let width = 0.15 + 0.35 * p.grade_params.x; // blending
    let w_hi = smoothstep_f(pivot - width, pivot + width, y);
    let w_sh = 1.0 - w_hi;
    let w_mid = 4.0 * w_sh * w_hi;

    let tint = w_sh * p.grade_shadows.xyz + w_mid * p.grade_midtones.xyz
             + w_hi * p.grade_highlights.xyz + p.grade_global.xyz;
    let lum = w_sh * p.grade_shadows.w + w_mid * p.grade_midtones.w
            + w_hi * p.grade_highlights.w + p.grade_global.w;

    return c + tint + vec3<f32>(lum);
}

fn adjust(rgb: vec3<f32>) -> vec3<f32> {
    var c = rgb * p.exposure_gain;
    let y = luma709(c);
    let hi = smoothstep(0.5, 1.0, y);
    let sh = 1.0 - smoothstep(0.0, 0.5, y);
    let wh = smoothstep(0.7, 1.0, y);
    let bl = 1.0 - smoothstep(0.0, 0.3, y);
    let region = (1.0 + p.highlights * hi) * (1.0 + p.shadows * sh)
        * (1.0 + p.whites * wh) * (1.0 + p.blacks * bl);
    c = c * region;
    // Phase 3: the WB↔contrast order is stage-selected. Global (light-engine /
    // color-pseudo-layer) order applies WB before contrast; the mask order
    // (historical, unchanged) applies contrast before WB — the two do not
    // commute, so each stage keeps its own order for parity.
    if (p.order_and_coverage.x != 0.0) {
        c = c * p.wb_mul;
        c = (c - vec3<f32>(p.contrast_pivot)) * p.contrast_gain + vec3<f32>(p.contrast_pivot);
    } else {
        c = (c - vec3<f32>(p.contrast_pivot)) * p.contrast_gain + vec3<f32>(p.contrast_pivot);
        c = c * p.wb_mul;
    }
    let y2 = luma709(c);
    c = vec3<f32>(y2) + (c - vec3<f32>(y2)) * p.saturation;
    if (p.hue_deg != 0.0) {
        var hsl = rgb2hsl(max(c, vec3<f32>(0.0)));
        hsl.x = hsl.x + p.hue_deg;
        hsl.x = hsl.x - floor(hsl.x / 360.0) * 360.0;
        c = hsl2rgb(hsl);
    }
    // Vibrance (Phase 3, new — both scopes): fades out as a pixel approaches
    // full saturation. Slots after hue, before the tone curve; gated on
    // non-zero so a zero-vibrance layer never enters the round trip.
    if (p.order_and_coverage.z != 0.0) {
        var hsl = rgb2hsl(max(c, vec3<f32>(0.0)));
        let v = p.order_and_coverage.z;
        hsl.y = clamp(hsl.y * (1.0 + v * (1.0 - hsl.y)), 0.0, 1.0);
        c = hsl2rgb(hsl);
    }
    // Phase 2b: per-layer tone curve (LUT), HSL bands, color grade — ported from
    // the global curve/hsl/color_grade passes; identity-skipped via flags.
    if (p.active_flags.x != 0.0) { c = curve_sample(c); }
    if (p.active_flags.y != 0.0) { c = hsl_bands_apply(c); }
    if (p.active_flags.z != 0.0) { c = grade_apply(c); }
    if (p.color_amount != 0.0) { c = c + (p.color_rgb - c) * p.color_amount; }
    return max(c, vec3<f32>(0.0));
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    // Phase 3: a full-coverage pseudo-layer (the global light/color segments)
    // skips the mask fetch ENTIRELY rather than sampling then overriding —
    // the bound mask texture for these dispatches may be a small placeholder,
    // and an out-of-bounds `textureLoad` at larger `xy` must never be reached.
    var m = 1.0;
    if (p.order_and_coverage.y == 0.0) {
        m = textureLoad(mask, xy, 0).r;
    }
    let out = mix(c.rgb, adjust(c.rgb), clamp(m, 0.0, 1.0));
    textureStore(dst, xy, vec4<f32>(out, c.a));
}
