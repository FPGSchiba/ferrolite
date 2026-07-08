// Color grading: per-pixel hue-sat tint + luminance offset per tonal region.
// Point op (point-op bind layout). Tints/lum are PRE-SCALED on the CPU
// (color_grade_uniform), so this shader adds them directly — the per-pixel math
// mirrors uniforms.rs `color_grade_px` exactly. Not clamped: out-of-[0,1] values
// pass through (identity grade ⇒ exact pass-through), P2 §5.3.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P {
    shadows: vec4<f32>,    // xyz = tint (pre-scaled), w = lum (pre-scaled)
    midtones: vec4<f32>,
    highlights: vec4<f32>,
    global: vec4<f32>,
    params: vec4<f32>,     // x = blending, y = balance
};
@group(0) @binding(2) var<uniform> p: P;

fn smoothstep_f(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);

    let y = dot(c.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let pivot = 0.5 + 0.5 * p.params.y;   // balance
    let width = 0.15 + 0.35 * p.params.x; // blending
    let w_hi = smoothstep_f(pivot - width, pivot + width, y);
    let w_sh = 1.0 - w_hi;
    let w_mid = 4.0 * w_sh * w_hi;

    let tint = w_sh * p.shadows.xyz + w_mid * p.midtones.xyz
             + w_hi * p.highlights.xyz + p.global.xyz;
    let lum = w_sh * p.shadows.w + w_mid * p.midtones.w
            + w_hi * p.highlights.w + p.global.w;

    let out_rgb = c.rgb + tint + vec3<f32>(lum);
    textureStore(dst, xy, vec4<f32>(out_rgb, c.a));
}
