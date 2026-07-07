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
};
@group(0) @binding(3) var<uniform> p: P;

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
    c = (c - vec3<f32>(p.contrast_pivot)) * p.contrast_gain + vec3<f32>(p.contrast_pivot);
    c = c * p.wb_mul;
    let y2 = luma709(c);
    c = vec3<f32>(y2) + (c - vec3<f32>(y2)) * p.saturation;
    if (p.hue_deg != 0.0) {
        var hsl = rgb2hsl(max(c, vec3<f32>(0.0)));
        hsl.x = hsl.x + p.hue_deg;
        hsl.x = hsl.x - floor(hsl.x / 360.0) * 360.0;
        c = hsl2rgb(hsl);
    }
    if (p.color_amount != 0.0) { c = c + (p.color_rgb - c) * p.color_amount; }
    return max(c, vec3<f32>(0.0));
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    let m = textureLoad(mask, xy, 0).r;
    let out = mix(c.rgb, adjust(c.rgb), clamp(m, 0.0, 1.0));
    textureStore(dst, xy, vec4<f32>(out, c.a));
}
