// Vignetting radial-gain pass (scene-linear, point op). For each pixel it
// computes the normalized radius from the image center (0 = center, 1 = corner)
// and multiplies rgb by a composable gain `corr(r) * manual(r)`:
//   corr(r)   = mix(1.0, lut_gain(r), vig_amount)   — the Lensfun profile path
//   manual(r) = 1.0 + manual * r * r                — parametric lens-free term
// At `vig_amount == 0 && manual == 0` both factors are 1.0, so this is the
// identity (rgb unchanged), which keeps existing goldens byte-identical.
// Negative `manual` darkens corners (adds a vignette); positive brightens them.
// The final gain is clamped non-negative before multiplying rgb.
//
// The LUT is a `len×1` `R32Float` texture (R = gain). The device lacks
// FLOAT32_FILTERABLE, so we sample it with `textureLoad` + manual linear
// interpolation between the two neighboring entries (see lens_gpu.rs docs).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
// Scalar members (not vecs) so the WGSL struct is a tight 32 bytes, byte-matching
// the Rust `VignetteUniform { vig_amount, manual, full_dims:[f32;2],
// origin:[f32;2], pad:[f32;2] }`. When `full_w`/`full_h` are both > 0.5 the pass
// runs in GLOBAL (full-output-image) mode: the radius is measured from the whole
// image's center using this tile's output `origin`, so a tiled render produces a
// single seamless vignette instead of one per tile. At `full_dims == 0` (the
// preview whole-image path + all existing goldens) it falls back to the original
// per-texture `textureDimensions(src)` radius, which is byte-identical.
struct V {
    vig_amount: f32,
    manual: f32,
    full_w: f32,
    full_h: f32,
    origin_x: f32,
    origin_y: f32,
    pad0: f32,
    pad1: f32,
};
@group(0) @binding(2) var<uniform> v: V;
@group(0) @binding(3) var lut: texture_2d<f32>; // len×1, R = gain

// Manual linear lookup into the len×1 gain LUT at normalized radius r in [0,1].
fn lut_gain(r: f32) -> f32 {
    let len = i32(textureDimensions(lut).x);
    if (len <= 1) {
        return textureLoad(lut, vec2<i32>(0, 0), 0).r;
    }
    let rc = clamp(r, 0.0, 1.0);
    let x = rc * f32(len - 1);
    let i0 = i32(floor(x));
    let i1 = min(i0 + 1, len - 1);
    let f = x - floor(x);
    let g0 = textureLoad(lut, vec2<i32>(i0, 0), 0).r;
    let g1 = textureLoad(lut, vec2<i32>(i1, 0), 0).r;
    return mix(g0, g1, f);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    // GLOBAL (tiled) path: measure uv against the full output image using this
    // tile's output origin, so radius is in whole-image coordinates. Otherwise
    // (full_dims == 0) fall back to per-texture dims — the whole-image behavior.
    var uv: vec2<f32>;
    if (v.full_w > 0.5 && v.full_h > 0.5) {
        uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + vec2<f32>(0.5, 0.5)
            + vec2<f32>(v.origin_x, v.origin_y))
            / vec2<f32>(v.full_w, v.full_h);
    } else {
        uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + vec2<f32>(0.5, 0.5))
            / vec2<f32>(f32(dims.x), f32(dims.y));
    }
    let d = uv - vec2<f32>(0.5, 0.5);
    let r = length(d) / length(vec2<f32>(0.5, 0.5)); // 0 center → 1 corner
    let gain = lut_gain(r);
    let corr = mix(1.0, gain, v.vig_amount);
    let man = 1.0 + v.manual * r * r;
    let g = max(corr * man, 0.0);
    let c = textureLoad(src, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(c.rgb * g, c.a));
}
