// Unsharp mask: out = src + amount * (src - boxblur(src, radius)). The
// neighborhood op. Reuses the point-op bind layout (0 = src, 1 = dst, 2 = uniform).
// At preview-res a single-pass box blur is enough; Plan 3 adds the tiled halo.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P { amount: f32, radius: i32, pad0: f32, pad1: f32 };
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

    var sum = vec3<f32>(0.0);
    var n = 0.0;
    for (var dy = -p.radius; dy <= p.radius; dy = dy + 1) {
        for (var dx = -p.radius; dx <= p.radius; dx = dx + 1) {
            let q = clamp(xy + vec2<i32>(dx, dy), vec2<i32>(0, 0), dims - vec2<i32>(1, 1));
            sum = sum + textureLoad(src, q, 0).rgb;
            n = n + 1.0;
        }
    }
    let blur = sum / n;
    let sharp = c.rgb + p.amount * (c.rgb - blur);
    textureStore(dst, xy, vec4<f32>(max(sharp, vec3<f32>(0.0)), c.a));
}
