// Dehaze (Dark Channel Prior, He et al.). Neighbourhood op: the dark channel is a
// local min over a `radius` patch of the NORMALIZED image min(rgb / A). Mirrors
// `dehaze::dehaze_recover` exactly. A (atmospheric light) is a whole-image
// constant supplied as a uniform (design §5.3), NOT estimated per tile.
// Reuses the point-op bind layout (0 = src, 1 = dst, 2 = uniform).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P { amount: f32, radius: i32, omega: f32, t0: f32, atmos: vec4<f32> };
@group(0) @binding(2) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(src));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);

    if (p.amount == 0.0 || p.radius <= 0) {
        textureStore(dst, xy, c);
        return;
    }

    let a = p.atmos.rgb;
    // Local dark channel of the normalized image I/A over the patch.
    var dark = 1.0;
    for (var dy = -p.radius; dy <= p.radius; dy = dy + 1) {
        for (var dx = -p.radius; dx <= p.radius; dx = dx + 1) {
            let q = clamp(xy + vec2<i32>(dx, dy), vec2<i32>(0, 0), dims - vec2<i32>(1, 1));
            let n = textureLoad(src, q, 0).rgb / a;
            let m = min(n.r, min(n.g, n.b));
            dark = min(dark, m);
        }
    }

    let t = clamp(1.0 - p.omega * dark, 0.0, 1.0);
    let te = max(t, p.t0);
    let j = (c.rgb - a) / te + a;          // remove-haze
    let hazed = a + (c.rgb - a) * t;       // add-haze (toward A)
    var out = c.rgb;
    if (p.amount >= 0.0) {
        out = c.rgb + p.amount * (j - c.rgb);
    } else {
        out = c.rgb + (-p.amount) * (hazed - c.rgb);
    }
    textureStore(dst, xy, vec4<f32>(out, c.a));
}
