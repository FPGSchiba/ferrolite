// HSL: 8-band hue/sat/lum adjustment. Point op (reuses the point-op bind layout:
// 0 = src texture, 1 = dst storage texture, 2 = uniform). Display-linear input is
// clamped to [0,1] for the HSL round-trip (a documented Spec-3 placeholder).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P { bands: array<vec4<f32>, 8> };
@group(0) @binding(2) var<uniform> p: P;

const MAX_HUE_SHIFT: f32 = 30.0; // degrees per unit band.hue

fn band_center(i: u32) -> f32 {
    // red, orange, yellow, green, aqua, blue, purple, magenta
    var centers = array<f32, 8>(0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0);
    return centers[i];
}

fn rgb2hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let l = (mx + mn) * 0.5;
    var h = 0.0;
    var s = 0.0;
    let d = mx - mn;
    if (d > 1e-6) {
        s = d / (1.0 - abs(2.0 * l - 1.0));
        if (mx == c.r) {
            h = ((c.g - c.b) / d) % 6.0;
        } else if (mx == c.g) {
            h = (c.b - c.r) / d + 2.0;
        } else {
            h = (c.r - c.g) / d + 4.0;
        }
        h = h * 60.0;
        if (h < 0.0) { h = h + 360.0; }
    }
    return vec3<f32>(h, s, l);
}

fn hue2rgb(pp: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) { t = t + 1.0; }
    if (t > 1.0) { t = t - 1.0; }
    if (t < 1.0 / 6.0) { return pp + (q - pp) * 6.0 * t; }
    if (t < 1.0 / 2.0) { return q; }
    if (t < 2.0 / 3.0) { return pp + (q - pp) * (2.0 / 3.0 - t) * 6.0; }
    return pp;
}

fn hsl2rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x / 360.0;
    let s = hsl.y;
    let l = hsl.z;
    if (s <= 1e-6) { return vec3<f32>(l, l, l); }
    var q = l + s - l * s;
    if (l < 0.5) { q = l * (1.0 + s); }
    let pp = 2.0 * l - q;
    return vec3<f32>(
        hue2rgb(pp, q, h + 1.0 / 3.0),
        hue2rgb(pp, q, h),
        hue2rgb(pp, q, h - 1.0 / 3.0),
    );
}

fn band_weight(hue: f32, center: f32) -> f32 {
    var d = abs(hue - center);
    if (d > 180.0) { d = 360.0 - d; }
    return max(0.0, 1.0 - d / 60.0);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    let hsl = rgb2hsl(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));

    var hue_acc = 0.0;
    var sat_acc = 0.0;
    var lum_acc = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let w = band_weight(hsl.x, band_center(i));
        hue_acc = hue_acc + w * p.bands[i].x;
        sat_acc = sat_acc + w * p.bands[i].y;
        lum_acc = lum_acc + w * p.bands[i].z;
    }

    var out_hsl = hsl;
    out_hsl.x = hsl.x + hue_acc * MAX_HUE_SHIFT;
    if (out_hsl.x < 0.0) { out_hsl.x = out_hsl.x + 360.0; }
    if (out_hsl.x >= 360.0) { out_hsl.x = out_hsl.x - 360.0; }
    out_hsl.y = clamp(hsl.y * (1.0 + sat_acc), 0.0, 1.0);
    out_hsl.z = clamp(hsl.z * (1.0 + lum_acc), 0.0, 1.0);

    let rgb = hsl2rgb(out_hsl);
    textureStore(dst, xy, vec4<f32>(max(rgb, vec3<f32>(0.0)), c.a));
}
