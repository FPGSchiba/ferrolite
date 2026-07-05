// Luma-range mask: a smooth band over the input's luma. Luma is the Rec.709
// weighted sum of the input color (working-space linear). The mask ramps up
// across `softness` below `lo`, is 1.0 inside [lo, hi], and ramps down across
// `softness` above `hi`. Analytic per pixel -> zero halo. The input is read via
// textureLoad (non-filterable), so it accepts any float color texture.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var out_tex: texture_storage_2d<r32float, write>;
struct P { lo: f32, hi: f32, softness: f32, pad: f32 };
@group(0) @binding(2) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(out_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    let luma = dot(c.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let s = max(p.softness, 1e-6);
    let lower = smoothstep(p.lo - s, p.lo, luma);
    let upper = 1.0 - smoothstep(p.hi, p.hi + s, luma);
    let m = clamp(min(lower, upper), 0.0, 1.0);
    textureStore(out_tex, xy, vec4<f32>(m, 0.0, 0.0, 1.0));
}
