// Dehaze transmission pass 3/8: separable block-min, VERTICAL half, with the
// raw-transmission transform folded in (avoids a 4th tiny pass):
//   dc(p)   = min over dy in [-radius,radius] of dcH(p.x, p.y+dy)
//   praw(p) = clamp(1 - omega*dc(p), 0, 1)
// Mirrors `min_filter_separable`'s vertical loop + `transmission_map` step 3.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<r32float, write>;
struct P {
    radius: i32,
    pad0: i32,
    pad1: vec2<i32>,
    atmos: vec4<f32>,
    omega: f32,
    eps: f32,
    pad2: vec2<f32>,
};
@group(0) @binding(2) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(src));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let r = p.radius;
    var m = 3.4e38;
    for (var dy = -r; dy <= r; dy = dy + 1) {
        let q = clamp(xy + vec2<i32>(0, dy), vec2<i32>(0, 0), dims - vec2<i32>(1, 1));
        m = min(m, textureLoad(src, q, 0).r);
    }
    let praw = clamp(1.0 - p.omega * m, 0.0, 1.0);
    textureStore(dst, xy, vec4<f32>(praw, 0.0, 0.0, 0.0));
}
