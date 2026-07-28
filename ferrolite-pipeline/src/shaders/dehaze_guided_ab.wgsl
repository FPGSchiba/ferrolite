// Dehaze transmission pass 7/8: guided-filter linear coefficients.
//   var_g  = corr_g  - mean_g*mean_g
//   cov_gp = corr_gp - mean_g*mean_p
//   a = cov_gp / (var_g + eps); b = mean_p - a*mean_g
// Mirrors `transmission_map`'s `av`/`bv` loop exactly.
@group(0) @binding(0) var mean_g: texture_2d<f32>;
@group(0) @binding(1) var mean_p: texture_2d<f32>;
@group(0) @binding(2) var corr_g: texture_2d<f32>;
@group(0) @binding(3) var corr_gp: texture_2d<f32>;
@group(0) @binding(4) var a_out: texture_storage_2d<r32float, write>;
@group(0) @binding(5) var b_out: texture_storage_2d<r32float, write>;
struct P {
    radius: i32,
    pad0: i32,
    pad1: vec2<i32>,
    atmos: vec4<f32>,
    omega: f32,
    eps: f32,
    pad2: vec2<f32>,
};
@group(0) @binding(6) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(mean_g));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let mg = textureLoad(mean_g, xy, 0).r;
    let mp = textureLoad(mean_p, xy, 0).r;
    let cg = textureLoad(corr_g, xy, 0).r;
    let cgp = textureLoad(corr_gp, xy, 0).r;
    let var_g = cg - mg * mg;
    let cov_gp = cgp - mg * mp;
    let a = cov_gp / (var_g + p.eps);
    let b = mp - a * mg;
    textureStore(a_out, xy, vec4<f32>(a, 0.0, 0.0, 0.0));
    textureStore(b_out, xy, vec4<f32>(b, 0.0, 0.0, 0.0));
}
