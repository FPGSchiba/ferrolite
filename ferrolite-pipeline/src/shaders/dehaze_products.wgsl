// Dehaze transmission pass 4/8: pointwise products feeding the guided filter's
// correlation terms. gg = guide*guide; gp = guide*praw. Mirrors
// `transmission_map`'s `gg`/`gp` computation exactly. Pure elementwise (no
// neighbourhood, no uniform needed).
@group(0) @binding(0) var guide: texture_2d<f32>;
@group(0) @binding(1) var praw: texture_2d<f32>;
@group(0) @binding(2) var gg_out: texture_storage_2d<r32float, write>;
@group(0) @binding(3) var gp_out: texture_storage_2d<r32float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(guide));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let g = textureLoad(guide, xy, 0).r;
    let pr = textureLoad(praw, xy, 0).r;
    textureStore(gg_out, xy, vec4<f32>(g * g, 0.0, 0.0, 0.0));
    textureStore(gp_out, xy, vec4<f32>(g * pr, 0.0, 0.0, 0.0));
}
