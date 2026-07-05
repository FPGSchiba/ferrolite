// Color-range mask: smooth selection by color distance to the nearest sample.
// For each pixel, the minimum Euclidean distance (in linear RGB) to any of the
// `count` samples is computed; the mask is 1 when that distance <= tolerance and
// ramps to 0 across `softness` beyond it. Analytic per pixel -> zero halo. Up to
// MAX_COLOR_SAMPLES (8) samples; input read via textureLoad (non-filterable).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var out_tex: texture_storage_2d<r32float, write>;
struct P {
    samples: array<vec4<f32>, 8>,
    count: f32,
    tolerance: f32,
    softness: f32,
    pad: f32,
};
@group(0) @binding(2) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(out_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0).rgb;
    let n = i32(p.count);
    var best = 1e9;
    for (var i = 0; i < n; i = i + 1) {
        let d = distance(c, p.samples[i].rgb);
        best = min(best, d);
    }
    let s = max(p.softness, 1e-6);
    let m = 1.0 - smoothstep(p.tolerance, p.tolerance + s, best);
    textureStore(out_tex, xy, vec4<f32>(clamp(m, 0.0, 1.0), 0.0, 0.0, 1.0));
}
