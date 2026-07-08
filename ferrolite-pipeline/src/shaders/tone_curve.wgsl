// Tone curve: three packed 256-entry display-linear LUTs (R,G,B rows) with
// linear interpolation between entries (identity ramp ⇒ exact identity).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
// 3 channels × 256 entries, row-major: R=[0,256), G=[256,512), B=[512,768).
@group(0) @binding(2) var<storage, read> lut: array<f32, 768>;

fn apply_lut(v: f32, ch: u32) -> f32 {
    let base = ch * 256u;
    // Preserve out-of-[0,1] values (P2 §5.3): extrapolate from the endpoints with
    // unit slope so highlights >1 and negatives pass through, instead of clamping.
    if (v < 0.0) { return lut[base] + v; }
    if (v > 1.0) { return lut[base + 255u] + (v - 1.0); }
    let x = v * 255.0;
    let i0 = u32(floor(x));
    let i1 = min(i0 + 1u, 255u);
    let f = x - floor(x);
    return mix(lut[base + i0], lut[base + i1], f);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    let rgb = vec3<f32>(apply_lut(c.r, 0u), apply_lut(c.g, 1u), apply_lut(c.b, 2u));
    textureStore(dst, xy, vec4<f32>(rgb, c.a));
}
