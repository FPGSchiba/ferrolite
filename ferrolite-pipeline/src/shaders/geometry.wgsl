// Geometry: crop + rotate as a bilinear sampling transform. Output dims differ
// from input dims, so this is NOT a point op — it has its own bind layout
// (0 = src texture, 1 = dst storage, 2 = uniform, 3 = sampler). Uses
// textureSampleLevel (compute has no implicit derivatives).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P {
    m: vec4<f32>,         // row-major 2x2: m00,m01,m10,m11
    off: vec2<f32>,
    src_dims: vec2<f32>,
    out_dims: vec2<f32>,
    pad: vec2<f32>,
};
@group(0) @binding(2) var<uniform> p: P;
@group(0) @binding(3) var samp: sampler;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ow = u32(p.out_dims.x);
    let oh = u32(p.out_dims.y);
    if (gid.x >= ow || gid.y >= oh) { return; }
    let po = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5);
    let sx = p.m.x * po.x + p.m.y * po.y + p.off.x;
    let sy = p.m.z * po.x + p.m.w * po.y + p.off.y;
    let uv = vec2<f32>(sx, sy) / p.src_dims;
    let c = textureSampleLevel(src, samp, uv, 0.0);
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), c);
}
