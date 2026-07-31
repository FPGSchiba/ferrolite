// Separable sharpen pass 3/3 (P4 Task 5 variant): the GLOBAL unsharp-mask
// apply with Detail (halo suppression) + Masking (edge protection) — design
// §4.3:
//   delta = mix(src - blur_r, src - blur_fine, detail)
//   edge  = masking > 0 ? smoothstep(t0, t1, |grad luma|) : 1
//   out   = src + amount * edge * delta
// At detail == 0 && masking == 0 this is byte-identical to
// `sharpen_apply.wgsl` (mix(...,0) == first arg; edge == 1) — gate 2. The
// node only dispatches THIS shader when at least one of the two is non-zero
// (`SharpenNode::evaluate`); the cheap identity-collapse case keeps using
// `sharpen_apply.wgsl` unchanged, with the identical bind group as before
// this task, so that path is byte-exact rather than merely close.
//
// Only the GLOBAL (unmasked) sharpen dispatch routes through this shader.
// Per-mask-layer sharpen (`sharpen_apply_masked.wgsl`) keeps its existing
// amount/radius-only formula in this task — a layer's own `detail`/`masking`
// fields exist on `Sharpen` (so they round-trip through sidecars and the
// per-mask `AdjustmentSet` without extra plumbing) but are not yet consumed
// by the masked-apply GPU pass.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var blur: texture_2d<f32>;
@group(0) @binding(2) var blur_fine: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;
// EXACTLY `SharpenUniform`'s 16-byte layout — the node writes ONE buffer and
// binds it to the box passes too, so this struct must not grow.
struct P { amount: f32, radius: i32, detail: f32, masking: f32 };
@group(0) @binding(4) var<uniform> p: P;

// Gradient normalization `G` (design §4.3). A `const`, NOT a uniform field,
// precisely so `P` stays 16 bytes. Mirrors
// `uniforms::SHARPEN_MASK_GRADIENT_NORM` — change both together.
const G: f32 = 0.25;

fn luma(c: vec3<f32>) -> f32 {
    return 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(src));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));

    let c = textureLoad(src, xy, 0);
    let b = textureLoad(blur, xy, 0).rgb;
    let bf = textureLoad(blur_fine, xy, 0).rgb;
    let delta = mix(c.rgb - b, c.rgb - bf, p.detail);

    var edge = 1.0;
    if (p.masking > 0.0) {
        let xm = clamp(xy.x - 1, 0, dims.x - 1);
        let xp = clamp(xy.x + 1, 0, dims.x - 1);
        let ym = clamp(xy.y - 1, 0, dims.y - 1);
        let yp = clamp(xy.y + 1, 0, dims.y - 1);
        let gx = luma(textureLoad(src, vec2<i32>(xp, xy.y), 0).rgb)
               - luma(textureLoad(src, vec2<i32>(xm, xy.y), 0).rgb);
        let gy = luma(textureLoad(src, vec2<i32>(xy.x, yp), 0).rgb)
               - luma(textureLoad(src, vec2<i32>(xy.x, ym), 0).rgb);
        let g = length(vec2<f32>(gx, gy));
        let t0 = p.masking * G;
        let t1 = t0 + 0.25 * G;
        edge = smoothstep(t0, t1, g);
    }

    let sharp = c.rgb + p.amount * edge * delta;
    textureStore(dst, xy, vec4<f32>(max(sharp, vec3<f32>(0.0)), c.a));
}
