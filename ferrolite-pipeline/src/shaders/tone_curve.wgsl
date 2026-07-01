// Tone curve: per-channel 256-entry display-linear LUT with linear
// interpolation between entries (so an identity ramp is exactly identity).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<storage, read> lut: array<f32, 256>;

fn apply_lut(v: f32) -> f32 {
    let x = clamp(v, 0.0, 1.0) * 255.0;
    let i0 = u32(floor(x));
    let i1 = min(i0 + 1u, 255u);
    let f = x - floor(x);
    return mix(lut[i0], lut[i1], f);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    let rgb = vec3<f32>(apply_lut(c.r), apply_lut(c.g), apply_lut(c.b));
    textureStore(dst, xy, vec4<f32>(rgb, c.a));
}
