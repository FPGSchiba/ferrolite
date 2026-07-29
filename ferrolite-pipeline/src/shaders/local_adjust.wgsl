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
    // Phase 4 Task 2/3: dehaze recovery fused as the FIRST step of `adjust()`,
    // ported EXACTLY from the retired `DehazeRecoveryNode`'s
    // `dehaze_recovery.wgsl`. Zero (identity) at every call site except the
    // global Color-stage pseudo-layer's own dispatch (Task 2) and a per-mask
    // layer whose OWN `dehaze.amount != 0.0` (Task 3) — see
    // `dehaze_recover_step`'s doc.
    dehaze_amount_atmos: vec4<f32>,   // x = amount, yzw = atmos
    dehaze_geo_m: vec4<f32>,          // row-major 2x2 output->source mapping
    dehaze_geo_off_src_dims: vec4<f32>, // xy = geo_off, zw = src_dims
    dehaze_frame: vec4<f32>,          // xy = frame_origin, zw = full_dims
    dehaze_out_dims_flags: vec4<f32>, // xy = out_dims, z = has_transmission, w = pad
};
@group(0) @binding(3) var<uniform> p: P;
// Phase 2b: per-layer 3x256 tone-curve LUT (R,G,B rows), same packing + binding
// style as `tone_curve.wgsl`'s LUT (a fresh small storage buffer per layer).
@group(0) @binding(4) var<storage, read> lut: array<f32, 768>;
// Phase 4 Task 2: the shared whole-image dehaze transmission (source space,
// possibly mip-mapped) + its sampler — mirrors the retired `DehazeRecoveryNode`'s
// bindings 1/4 exactly. Always bound (a 1x1 neutral fallback when dehaze has no
// real transmission yet — see `LocalAdjustmentsNode::set_shared_transmission`),
// so every dispatch (Light stage, mask layers, global pseudo-layer) validates
// regardless of whether this pass ever samples it.
@group(0) @binding(5) var dehaze_trans: texture_2d<f32>;
@group(0) @binding(6) var dehaze_samp: sampler;

// Transmission floor (design §5.2 step 4): avoids divide-by-~0 noise blow-up.
// Mirrors `crate::dehaze::DEHAZE_T0` — a fixed internal constant, never
// user-adjustable, so it is not worth a uniform field.
const DEHAZE_T0: f32 = 0.1;

fn luma709(c: vec3<f32>) -> f32 { return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722)); }

// HSV-style saturation measure (max-min)/max(max, eps), clamped to [0,1] —
// stable for any brightness (no HSL round-trip, so no denominator singularity
// at l==1.0 / negative saturation at l>1.0, see rgb2hsl below). Used by
// vibrance's fade weight; mirrors `uniforms.rs`'s `hsv_sat_measure` exactly.
fn vibrance_weight(c: vec3<f32>) -> f32 {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    return clamp((mx - mn) / max(mx, 1e-4), 0.0, 1.0);
}

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

// Phase 4 Task 2/3: dehaze recovery, ported EXACTLY from the retired
// `DehazeRecoveryNode`'s `dehaze_recovery.wgsl` (source-UV mapping incl. the
// LOD-independent `frame_origin`/`full_dims` normalization, the mip-aware
// transmission LOD pick, and the `t0` floor). Identity when `dehaze_amount_atmos.x
// == 0.0` (no active Dehaze op for THIS dispatch — the global pseudo-layer's
// op or a mask layer's own `dehaze.amount`) or `has_transmission == 0.0` (the
// node's 1x1 neutral fallback is bound — no real transmission computed/bound
// yet), so the Light stage (never populates these fields) and any mask layer
// with a zero (identity) dehaze amount take this cheap early-out and pay for
// nothing beyond the branch.
fn dehaze_recover_step(rgb: vec3<f32>, xy: vec2<i32>) -> vec3<f32> {
    let amount = p.dehaze_amount_atmos.x;
    let has_transmission = p.dehaze_out_dims_flags.z;
    if (amount == 0.0 || has_transmission == 0.0) {
        return rgb;
    }
    let full_dims = p.dehaze_frame.zw;
    if (full_dims.x <= 0.0 || full_dims.y <= 0.0) {
        // Can't-happen guard (mirrors the retired node's shader): avoids a
        // divide-by-zero rather than assuming a real `TileFrame` is set.
        return rgb;
    }
    let a = p.dehaze_amount_atmos.yzw;
    let frame_origin = p.dehaze_frame.xy;
    let out_dims = p.dehaze_out_dims_flags.xy;
    let geo_off = p.dehaze_geo_off_src_dims.xy;
    let src_dims = p.dehaze_geo_off_src_dims.zw;
    let geo_m = p.dehaze_geo_m;

    // LOD-independent output-pixel coordinate (see the retired
    // `dehaze_recovery.wgsl`'s file doc): normalize by this LOD's full output
    // dims, then re-expand to the level-0 output dims the geometry mapping
    // expects.
    let out_norm = (frame_origin + vec2<f32>(f32(xy.x), f32(xy.y))) / full_dims;
    let out_px_l0 = out_norm * out_dims;
    let src = vec2<f32>(
        geo_m.x * out_px_l0.x + geo_m.y * out_px_l0.y + geo_off.x,
        geo_m.z * out_px_l0.x + geo_m.w * out_px_l0.y + geo_off.y,
    );
    let uv = src / src_dims;

    // LOD-aware transmission fetch (mirrors `transmission_sample_lod`): pick
    // the band-limited mip level when this LOD's output is coarser than the
    // transmission map, so a zoomed-out tile doesn't undersample the sharp
    // guided-refined edges into ringing.
    let trans_dims = vec2<f32>(textureDimensions(dehaze_trans, 0));
    let ratio = max(trans_dims.x / full_dims.x, trans_dims.y / full_dims.y);
    let lod = max(0.0, log2(ratio));
    let t = clamp(textureSampleLevel(dehaze_trans, dehaze_samp, uv, lod).r, 0.0, 1.0);
    let te = max(t, DEHAZE_T0);
    let j = (rgb - a) / te + a;
    let hazed = a + (rgb - a) * t;
    if (amount >= 0.0) {
        return rgb + amount * (j - rgb);
    }
    return rgb + (-amount) * (hazed - rgb);
}

fn adjust(rgb_in: vec3<f32>, xy: vec2<i32>) -> vec3<f32> {
    var c = dehaze_recover_step(rgb_in, xy) * p.exposure_gain;
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
    // Hue rotation round-trips through HSL, whose `s = d / (1 - |2l-1|)`
    // formula has a removable-singularity denominator at `l == 1.0` (Inf) and
    // goes negative at `l > 1.0` — both reachable by ordinary scene-linear
    // bright pixels. Either regime turns `hsl2rgb`'s `q = l + s - l*s` into
    // NaN, which is stored to the output texture and renders as a black pixel
    // — the root cause of the "black pixel in bright sky" bug for hue,
    // mirrored exactly by vibrance below (same round trip, same fix shape). An
    // achromatic-domain pixel this close to the l==0/1 rail has no meaningful
    // hue to rotate, so the rotation is simply skipped there (identity for
    // this step). Keep the threshold in lockstep with the CPU reference's hue
    // branch in `uniforms.rs`'s `light_color_apply`.
    if (p.hue_deg != 0.0) {
        let cc = max(c, vec3<f32>(0.0));
        let mx = max(cc.r, max(cc.g, cc.b));
        let mn = min(cc.r, min(cc.g, cc.b));
        let l = (mx + mn) * 0.5;
        let denom = 1.0 - abs(2.0 * l - 1.0);
        if (denom > 1e-4) {
            var hsl = rgb2hsl(cc);
            hsl.x = hsl.x + p.hue_deg;
            hsl.x = hsl.x - floor(hsl.x / 360.0) * 360.0;
            c = hsl2rgb(hsl);
        }
    }
    // Vibrance (Phase 3): a saturation boost that fades as a pixel approaches
    // full saturation. Reimplemented WITHOUT the HSL round trip (the original
    // formula shared hue's l==1/l>1 singularity above and produced NaN/black
    // pixels on ordinary scene-linear highlights). Mirrors the `saturation`
    // step's luma-mix pattern instead: measure "how saturated" the pixel is
    // with a stable HSV-style ratio (`vibrance_weight`, bounded in [0,1] for
    // any brightness, no HSL detour), fade the boost as that measure rises
    // toward 1, then mix toward/away from luma exactly like `saturation` does.
    // Gated on non-zero so a zero-vibrance layer takes none of this math. Keep
    // in lockstep with the CPU reference's vibrance branch in `uniforms.rs`'s
    // `light_color_apply`.
    if (p.order_and_coverage.z != 0.0) {
        let v = p.order_and_coverage.z;
        let w = 1.0 - vibrance_weight(c);
        let y3 = luma709(c);
        c = vec3<f32>(y3) + (c - vec3<f32>(y3)) * (1.0 + v * w);
    }
    // Phase 2b: per-layer tone curve (LUT), HSL bands, color grade — ported from
    // the global curve/hsl/color_grade passes; identity-skipped via flags.
    if (p.active_flags.x != 0.0) { c = curve_sample(c); }
    if (p.active_flags.y != 0.0) { c = hsl_bands_apply(c); }
    if (p.active_flags.z != 0.0) { c = grade_apply(c); }
    if (p.color_amount != 0.0) { c = c + (p.color_rgb - c) * p.color_amount; }
    // Phase 3 (Task 3 parity fix): the floor clamp below is pre-Phase-3 per-mask
    // (mask-order) behavior, kept for every real mask layer. The global-order
    // (pseudo-layer) dispatches now sharing this function did NOT exist before
    // Phase 3 as a single fused pass — their pre-fusion equivalents were the
    // standalone exposure/white-balance/contrast/tone-curve/hsl/color-grade
    // passes, none of which ever clamped (each is a pure, unclamped transform;
    // see `light_trio`/`curve_hsl_grade`/`wb_contrast_both`'s committed parity
    // goldens, which legitimately carry pixels down to ~-0.09 in scene-linear
    // space). Clamping here for `order_and_coverage.x != 0.0` (global order)
    // would silently floor those excursions and break parity. `global_order`
    // and `full_coverage` are always equal at every real call site (both true
    // for the two pseudo-layer dispatches, both false for every per-mask
    // layer), so gating on either is equivalent; `global_order` is used here
    // since it's the flag this function already branches on just above.
    if (p.order_and_coverage.x != 0.0) {
        return c;
    }
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
    let out = mix(c.rgb, adjust(c.rgb, xy), clamp(m, 0.0, 1.0));
    textureStore(dst, xy, vec4<f32>(out, c.a));
}
