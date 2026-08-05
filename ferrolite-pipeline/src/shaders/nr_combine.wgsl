// NR final pass: reconstruct `acc + approx` (the coarsest residual) and convert
// YCbCr -> working RGB. Mirrors `nr::ycbcr_to_rgb` exactly.
@group(0) @binding(0) var acc: texture_2d<f32>;
@group(0) @binding(1) var approx: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(acc));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));

    let a = textureLoad(approx, xy, 0);
    let ycc = textureLoad(acc, xy, 0).rgb + a.rgb;
    let y = ycc.r; let cb = ycc.g; let cr = ycc.b;
    let r = cr + y;
    let b = cb + y;
    let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    textureStore(dst, xy, vec4<f32>(max(vec3<f32>(r, g, b), vec3<f32>(0.0)), a.a));
}
