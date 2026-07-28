// Dehaze transmission pass 2/8: separable block-min, HORIZONTAL half.
// dcH(p) = min over dx in [-radius,radius] of dc0(p+dx, p.y). Clamp-to-edge.
// Mirrors `min_filter_separable`'s horizontal loop exactly. Generic single
// R32Float plane in/out; the node reuses this pipeline nowhere else (min_v
// folds in the ω transform so it is a distinct shader) but shares the same
// bind-group-layout shape as `dehaze_min_v`/`dehaze_box_h`/`dehaze_box_v`.
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
    for (var dx = -r; dx <= r; dx = dx + 1) {
        let q = clamp(xy + vec2<i32>(dx, 0), vec2<i32>(0, 0), dims - vec2<i32>(1, 1));
        m = min(m, textureLoad(src, q, 0).r);
    }
    textureStore(dst, xy, vec4<f32>(m, 0.0, 0.0, 0.0));
}
