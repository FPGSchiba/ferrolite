// Dehaze recovery + blend (ST-Task 2): single-input compute pass taking the
// original image I and sampling an EXTERNALLY-supplied SHARED transmission
// texture `trans` (source space) at each output pixel's SOURCE UV, producing
// the recovered/blended image. Mirrors the pure CPU reference `dehaze_recover`
// exactly, but consumes q directly (while the CPU reference takes dark derived
// as (1-q)/DEHAZE_OMEGA).
//
// Source-UV mapping (LOD-INDEPENDENT — fixes the near-black-when-zoomed-out
// bug): `frame_origin`/`full_dims` are this pass's own TileFrame, i.e. this
// tile's haloed origin and the FULL output image size AT THIS LOD (just
// `[0,0]`/the level-0 output dims on the whole-image tier). Normalizing by
// `full_dims` first (`out_norm`) makes the pixel coordinate LOD-independent,
// then re-expanding by the LEVEL-0 output dims (`out_dims`, from the geometry
// uniform) recovers the level-0 output-pixel coordinate the geometry mapping
// (`geo_m`/`geo_off`, baked from level-0 dims) expects. At LOD 0, `full_dims
// == out_dims`, so `out_px_l0 == frame_origin + gid` — identical to the old
// (LOD-dependent) mapping. At a coarser LOD N, `out_dims / full_dims == 2^N`,
// so `out_px_l0` correctly maps back up to the level-0 pixel instead of
// collapsing toward a corner (the old bug: `geo_m` expects level-0 output
// pixels, but `frame_origin + gid` are in the CURRENT LOD's downscaled output
// space at any LOD other than 0). `src = geo_m * out_px_l0 + geo_off` mirrors
// exactly what `GeometryHeadNode`/`geometry.wgsl` used to resample the source
// for this output pixel; `uv = src / src_dims`. Under identity geometry
// (`geo_m = I`, `geo_off = 0`, `out_dims == src_dims`) this reduces to
// `uv = out_norm = (frame_origin + gid) / full_dims` — the whole-image
// alignment the dehaze goldens (and the tiled-vs-whole-image parity golden)
// check. `trans` may be a different resolution than `img` (the transmission is
// computed at a capped working resolution — see `transmission_working_dims`)
// and is MIP-MAPPED — sampled trilinearly via `textureSampleLevel` at an
// explicit LOD (see the `transmission_sample_lod` block below), resolution-
// agnostic since `uv` is normalized. The explicit LOD band-limits the fetch to
// the display resolution so a zoomed-out (coarse-LOD) tile does not undersample
// the map into ringing.
//
// `has_transmission == 0u` (no shared transmission bound — the node's default
// 1×1 neutral fallback) passes `I` through unchanged, same as `amount == 0.0`.
// `full_dims <= 0` (can't-happen once wired — every caller sets a real
// `TileFrame`) is guarded as a passthrough too, to avoid a divide-by-zero.

@group(0) @binding(0) var img: texture_2d<f32>;
@group(0) @binding(1) var trans: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;

struct P {
    amount: f32,
    t0: f32,
    pad0: f32,
    pad1: f32,
    atmos: vec4<f32>,
    geo_m: vec4<f32>,
    geo_off: vec2<f32>,
    src_dims: vec2<f32>,
    frame_origin: vec2<f32>,
    full_dims: vec2<f32>,
    out_dims: vec2<f32>,
    has_transmission: u32,
    pad2: u32,
};

@group(0) @binding(3) var<uniform> p: P;
@group(0) @binding(4) var samp: sampler;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(img));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) {
        return;
    }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(img, xy, 0);
    if (p.amount == 0.0 || p.has_transmission == 0u) {
        textureStore(dst, xy, c);
        return;
    }
    if (p.full_dims.x <= 0.0 || p.full_dims.y <= 0.0) {
        // Can't-happen guard: every caller sets a real `TileFrame` before this
        // node evaluates. Avoids a divide-by-zero rather than assuming it.
        textureStore(dst, xy, c);
        return;
    }
    let a = p.atmos.rgb;

    // LOD-independent output-pixel coordinate (see file doc): normalize by
    // this LOD's full output dims, then re-expand to the level-0 output dims
    // the geometry mapping expects.
    let out_norm = (p.frame_origin + vec2<f32>(f32(gid.x), f32(gid.y))) / p.full_dims;
    let out_px_l0 = out_norm * p.out_dims;
    let src = vec2<f32>(
        p.geo_m.x * out_px_l0.x + p.geo_m.y * out_px_l0.y + p.geo_off.x,
        p.geo_m.z * out_px_l0.x + p.geo_m.w * out_px_l0.y + p.geo_off.y,
    );
    let uv = src / p.src_dims;

    // LOD-aware transmission fetch (mirrors `transmission_sample_lod`): the
    // shared transmission is mip-mapped. When this LOD's output is COARSER than
    // the map (`full_dims` < the map dims — i.e. zoomed out past fit), one
    // output pixel covers many transmission texels; sampling the base level
    // there undersamples the sharp guided-refined edges into ringing. Pick the
    // band-limited level `log2(max(trans_dims/full_dims))` instead (floored at
    // 0, so fit/zoom-in and the whole-image preview tier — where the output is
    // >= the map — still sample the base level exactly as before). The sampler
    // clamps LOD to the available mip count. `full_dims > 0` guaranteed above.
    let trans_dims = vec2<f32>(textureDimensions(trans, 0));
    let ratio = max(trans_dims.x / p.full_dims.x, trans_dims.y / p.full_dims.y);
    let lod = max(0.0, log2(ratio));
    let t = clamp(textureSampleLevel(trans, samp, uv, lod).r, 0.0, 1.0);
    let te = max(t, p.t0);
    let j = (c.rgb - a) / te + a;
    let hazed = a + (c.rgb - a) * t;
    var out = c.rgb;
    if (p.amount >= 0.0) {
        out = c.rgb + p.amount * (j - c.rgb);
    } else {
        out = c.rgb + (-p.amount) * (hazed - c.rgb);
    }
    textureStore(dst, xy, vec4<f32>(out, c.a));
}
