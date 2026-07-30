// Geometry: crop + rotate as a bilinear sampling transform, optionally fused
// with the per-channel Lensfun warp grid (distortion + TCA). Output dims differ
// from input dims, so this is NOT a point op — it has its own bind layout
// (0 = src texture, 1 = dst storage, 2 = geometry uniform, 3 = src sampler,
// 4 = warp texture A, 5 = warp texture B, 6 = lens uniform). Uses
// textureSampleLevel for the source (compute has no implicit derivatives).
// `out_origin` offsets the output pixel so a tile pass can render a haloed
// sub-region of the output.
//
// Warp grid scheme (see lens_gpu.rs module docs): the grid is stored as TWO
// full-precision f32 textures — A = rgba32float `[rU,rV,gU,gV]`, B = rg32float
// `[bU,bV]` — because the device has no FLOAT32_FILTERABLE feature, so a
// filtering sampler over them is unavailable. We therefore sample them with
// `textureLoad` + MANUAL bilinear interpolation (4 texel fetches + lerp). The
// grid is baked over undistorted normalized image space and returns per-channel
// DISTORTED source coords in [0,1]; green is the geometric reference, R/B carry
// the transverse chromatic aberration split.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P {
    m: vec4<f32>,         // row-major 2x2: m00,m01,m10,m11
    off: vec2<f32>,
    src_dims: vec2<f32>,
    out_dims: vec2<f32>,
    out_origin: vec2<f32>,
    // Source-normalized clamp rect for base_uv: min_u,min_v,max_u,max_v — the
    // crop sub-rect inset by half a source texel, so an out-of-crop rotated
    // sample clamps to the CROP's edge instead of reading past it into (and
    // duplicating) the frame's own edge texel. Full-frame geometry carries the
    // half-texel-inset FULL rect here, so un-cropped rendering is unchanged.
    crop_bounds: vec4<f32>,
};
@group(0) @binding(2) var<uniform> p: P;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var warp_a: texture_2d<f32>; // [rU,rV,gU,gV]
@group(0) @binding(5) var warp_b: texture_2d<f32>; // [bU,bV]
struct Lens {
    dist_amount: f32,
    tca_amount: f32,
    vig_amount: f32,
    use_warp: u32,
};
@group(0) @binding(6) var<uniform> lens: Lens;

// Manual bilinear fetch of the two warp textures at normalized `base_uv`,
// returning per-channel distorted source coords: `.rg = R uv`, `.ba = G uv`,
// and B uv in the `out_b` out-param. The grid is n×n and node `i` sits at
// normalized coord `i/(n-1)` (NOT at texel centers `(i+0.5)/n`) — this matches
// `ferrolite-lens::bake_geometry`, which divides each result coord by `n-1`. We
// therefore invert with `g = base_uv * (n-1)` to grid space `[0, n-1]`, clamp,
// and lerp the 4 neighboring nodes. The 1×1 identity grid degenerates safely
// (`n-1 = 0` → clamp pins to node 0).
fn warp_sample(base_uv: vec2<f32>, out_b: ptr<function, vec2<f32>>) -> vec4<f32> {
    let dims = textureDimensions(warp_a);
    let n = vec2<f32>(f32(dims.x), f32(dims.y));
    // Node i sits at uv = i/(n-1); invert to grid coords, clamped to [0, n-1].
    let g = clamp(base_uv * (n - vec2<f32>(1.0, 1.0)), vec2<f32>(0.0, 0.0), n - vec2<f32>(1.0, 1.0));
    let g0 = floor(g);
    let g1 = min(g0 + vec2<f32>(1.0, 1.0), n - vec2<f32>(1.0, 1.0));
    let f = g - g0;
    let i0 = vec2<i32>(i32(g0.x), i32(g0.y));
    let i1 = vec2<i32>(i32(g1.x), i32(g1.y));

    let a00 = textureLoad(warp_a, vec2<i32>(i0.x, i0.y), 0);
    let a10 = textureLoad(warp_a, vec2<i32>(i1.x, i0.y), 0);
    let a01 = textureLoad(warp_a, vec2<i32>(i0.x, i1.y), 0);
    let a11 = textureLoad(warp_a, vec2<i32>(i1.x, i1.y), 0);
    let a = mix(mix(a00, a10, f.x), mix(a01, a11, f.x), f.y);

    let b00 = textureLoad(warp_b, vec2<i32>(i0.x, i0.y), 0).xy;
    let b10 = textureLoad(warp_b, vec2<i32>(i1.x, i0.y), 0).xy;
    let b01 = textureLoad(warp_b, vec2<i32>(i0.x, i1.y), 0).xy;
    let b11 = textureLoad(warp_b, vec2<i32>(i1.x, i1.y), 0).xy;
    *out_b = mix(mix(b00, b10, f.x), mix(b01, b11, f.x), f.y);

    return a;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ow = u32(p.out_dims.x);
    let oh = u32(p.out_dims.y);
    if (gid.x >= ow || gid.y >= oh) { return; }
    let po = p.out_origin + vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5);
    let sx = p.m.x * po.x + p.m.y * po.y + p.off.x;
    let sy = p.m.z * po.x + p.m.w * po.y + p.off.y;
    // Clamp to the crop sub-rect (not the whole source texture) so a rotated
    // crop's out-of-bounds corners smear the CROP's own edge rather than the
    // frame's — see the `crop_bounds` field doc on `struct P`.
    let base_uv = clamp(
        vec2<f32>(sx, sy) / p.src_dims,
        p.crop_bounds.xy,
        p.crop_bounds.zw,
    );

    if (lens.use_warp == 0u) {
        // Regression path: byte-identical to the pre-lens single-sample geometry.
        let c = textureSampleLevel(src, samp, base_uv, 0.0);
        textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), c);
        return;
    }

    // Sample the warp grid at the undistorted coord → per-channel distorted uv.
    var b_uv = vec2<f32>(0.0, 0.0);
    let a = warp_sample(base_uv, &b_uv);
    let g_uv = a.zw;            // green = geometric distortion reference
    let r_full = a.xy;         // full R distorted uv (green + TCA)
    let b_full = b_uv;         // full B distorted uv (green + TCA)

    // TCA Amount scales the per-channel (channel − green) split.
    let r_uv = mix(g_uv, r_full, lens.tca_amount);
    let bch_uv = mix(g_uv, b_full, lens.tca_amount);

    // Distortion Amount lerps each channel between identity (base_uv) and its
    // warped coord. At dist=0,tca=0 every channel collapses to base_uv.
    let r_final = mix(base_uv, r_uv, lens.dist_amount);
    let g_final = mix(base_uv, g_uv, lens.dist_amount);
    let b_final = mix(base_uv, bch_uv, lens.dist_amount);

    let r = textureSampleLevel(src, samp, r_final, 0.0).r;
    // Sample green once and reuse its alpha (green is the geometric reference, so
    // alpha follows it) — avoids a second textureSampleLevel at `g_final`.
    let g_sample = textureSampleLevel(src, samp, g_final, 0.0);
    let b = textureSampleLevel(src, samp, b_final, 0.0).b;
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(r, g_sample.g, b, g_sample.a));
}
