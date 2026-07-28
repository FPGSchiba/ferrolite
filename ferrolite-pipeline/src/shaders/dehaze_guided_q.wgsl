// Dehaze transmission pass 8/8: combine the guided-filter coefficient means
// with the guide into the final refined transmission q, written into all
// four channels of the rgba16float output so downstream (the recovery node,
// QS-Task 3) can read `.r`. Mirrors `transmission_map`'s final map exactly:
//   q(p) = clamp(mean_a(p)*guide(p) + mean_b(p), 0, 1)
@group(0) @binding(0) var mean_a: texture_2d<f32>;
@group(0) @binding(1) var mean_b: texture_2d<f32>;
@group(0) @binding(2) var guide: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(guide));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let ma = textureLoad(mean_a, xy, 0).r;
    let mb = textureLoad(mean_b, xy, 0).r;
    let g = textureLoad(guide, xy, 0).r;
    let q = clamp(ma * g + mb, 0.0, 1.0);
    textureStore(dst, xy, vec4<f32>(q, q, q, q));
}
