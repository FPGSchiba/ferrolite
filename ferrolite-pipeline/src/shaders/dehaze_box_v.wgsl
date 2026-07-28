// Dehaze transmission pass 6/8: separable normalized box average, VERTICAL
// half (paired with `dehaze_box_h`). Reused for all six guided-filter box
// outputs (mean_g, mean_p, corr_g, corr_gp, mean_a, mean_b). Mirrors
// `box_blur_separable`'s vertical loop exactly.
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
    var s = 0.0;
    for (var dy = -r; dy <= r; dy = dy + 1) {
        let q = clamp(xy + vec2<i32>(0, dy), vec2<i32>(0, 0), dims - vec2<i32>(1, 1));
        s = s + textureLoad(src, q, 0).r;
    }
    let n = f32(2 * r + 1);
    textureStore(dst, xy, vec4<f32>(s / n, 0.0, 0.0, 0.0));
}
