// Separable sharpen pass 3/3: the unsharp-mask apply — `out = src +
// amount*(src - blur)`, clamped non-negative, alpha passed through unchanged.
// Reads the ORIGINAL src (binding 0) and the separable box blur produced by
// `sharpen_box_h.wgsl` -> `sharpen_box_v.wgsl` (binding 1) — mirrors
// `sharpen.wgsl`'s final combine step exactly, just fed a separable blur
// instead of the fused 2D one. The node's `evaluate` never dispatches this
// shader when `amount == 0 || radius <= 0` (identity passthrough — see
// `sharpen_node.rs`), so no in-shader identity branch is needed here (unlike
// the old fused `sharpen.wgsl`, which had to branch because it was the only
// pass).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var blur: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;
struct P { amount: f32, radius: i32, pad0: f32, pad1: f32 };
@group(0) @binding(3) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(src));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));

    let c = textureLoad(src, xy, 0);
    let b = textureLoad(blur, xy, 0).rgb;
    let sharp = c.rgb + p.amount * (c.rgb - b);
    textureStore(dst, xy, vec4<f32>(max(sharp, vec3<f32>(0.0)), c.a));
}
