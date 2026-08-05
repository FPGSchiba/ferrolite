// Separable sharpen pass 3b (Phase 4 Task 4): a PER-MASK-LAYER masked apply,
// dispatched once per active mask layer AFTER the (optional) global apply
// (sharpen_apply.wgsl, unchanged). Adds THIS layer's own unsharp-mask detail
// term onto the running accumulator:
//
//     out = accum + mask * amount * (orig_src - blur)
//
// `accum` (binding 0) is the running total so far (the global apply's output,
// or the node's original input `src` if the global op is inactive). `orig_src`
// (binding 1) is the SharpenNode's ORIGINAL input — the SAME texture for
// every layer AND the global pass, never the running accumulator — so each
// layer's contribution is a fixed-base delta term. Summing fixed-base deltas
// is commutative, so applying layers in any order yields the same total
// (order-independent, unlike the Color engine's per-layer point-op chain,
// which reads the PREVIOUS layer's output as its own input). `blur` (binding
// 2) is THIS layer's own radius's separable box blur (see
// sharpen_node.rs::encode_blur) — one blur is computed per DISTINCT radius
// across the whole evaluate (global + every active layer), so this may be a
// texture shared with the global pass or another layer at the same radius.
// `mask` (binding 3, R32Float, non-filterable) is the Color engine's own
// composited mask for this layer (see `SharedMasks` in local_node.rs),
// sampled 1:1 (no origin/LOD offset — same resolution as `accum`/`orig_src`).
// Result clamped non-negative, matching sharpen_apply.wgsl's own clamp
// (applied cumulatively, so the running accumulator never goes negative).
@group(0) @binding(0) var accum: texture_2d<f32>;
@group(0) @binding(1) var orig_src: texture_2d<f32>;
@group(0) @binding(2) var blur: texture_2d<f32>;
@group(0) @binding(3) var mask: texture_2d<f32>;
@group(0) @binding(4) var dst: texture_storage_2d<rgba16float, write>;
// `p.detail`/`p.masking` (P4 Task 5) are declared for layout match, unused in
// this pass — per-mask-layer sharpen does not yet consume its own
// detail/masking (they ship greyed pending Task 7); `sharpen_node.rs` always
// writes 0.0 for both fields on this dispatch.
struct P { amount: f32, radius: i32, detail: f32, masking: f32 };
@group(0) @binding(5) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(accum));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));

    let a = textureLoad(accum, xy, 0);
    let s = textureLoad(orig_src, xy, 0).rgb;
    let b = textureLoad(blur, xy, 0).rgb;
    let m = clamp(textureLoad(mask, xy, 0).r, 0.0, 1.0);
    let sharp = a.rgb + m * p.amount * (s - b);
    textureStore(dst, xy, vec4<f32>(max(sharp, vec3<f32>(0.0)), a.a));
}
