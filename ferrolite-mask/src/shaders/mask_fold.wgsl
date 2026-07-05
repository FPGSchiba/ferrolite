// Two-input mask fold: out = op(acc, b) per pixel, where op is chosen by `mode`
// (0=Add=max, 1=Subtract=acc*(1-b), 2=Intersect=min). Mirrors the CPU
// `composite_scalar` operators exactly. Inputs read via textureLoad (R32Float,
// non-filterable); output is a fresh R32Float storage texture.
@group(0) @binding(0) var acc_tex: texture_2d<f32>;
@group(0) @binding(1) var b_tex: texture_2d<f32>;
@group(0) @binding(2) var out_tex: texture_storage_2d<r32float, write>;
struct P { mode: u32, pad0: u32, pad1: u32, pad2: u32 };
@group(0) @binding(3) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(out_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let a = textureLoad(acc_tex, xy, 0).r;
    let b = textureLoad(b_tex, xy, 0).r;
    var m = a;
    if (p.mode == 0u) { m = max(a, b); }
    else if (p.mode == 1u) { m = a * (1.0 - b); }
    else { m = min(a, b); }
    textureStore(out_tex, xy, vec4<f32>(m, 0.0, 0.0, 1.0));
}
