// Dehaze recovery + blend (ST-Task 2): single-input compute pass taking the
// original image I and sampling an EXTERNALLY-supplied SHARED transmission
// texture `trans` (source space) at each output pixel's SOURCE UV, producing
// the recovered/blended image. Mirrors the pure CPU reference `dehaze_recover`
// exactly, but consumes q directly (while the CPU reference takes dark derived
// as (1-q)/DEHAZE_OMEGA).
//
// Source-UV mapping: `out_xy = frame_origin + gid` is this pass's own
// output-space pixel coordinate (the haloed tile's local index plus the tile's
// global origin on the tiled tier; just `gid` on the whole-image tier, where
// `frame_origin = [0,0]`). `src = geo_m * out_xy + geo_off` mirrors exactly
// what `GeometryHeadNode`/`geometry.wgsl` used to resample the source for this
// output pixel; `uv = src / src_dims`. Under identity geometry (`geo_m = I`,
// `geo_off = 0`) this reduces to `uv = gid / src_dims` — the whole-image
// alignment the dehaze goldens (and the tiled-vs-whole-image parity golden)
// check. `trans` may be a different resolution than `img` (the transmission is
// computed at a capped working resolution — see `transmission_working_dims`)
// — sampled bilinearly via `textureSampleLevel`, which is resolution-agnostic
// since `uv` is normalized.
//
// `has_transmission == 0u` (no shared transmission bound — the node's default
// 1×1 neutral fallback) passes `I` through unchanged, same as `amount == 0.0`.

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
    let a = p.atmos.rgb;

    let out_xy = p.frame_origin + vec2<f32>(f32(gid.x), f32(gid.y));
    let src = vec2<f32>(
        p.geo_m.x * out_xy.x + p.geo_m.y * out_xy.y + p.geo_off.x,
        p.geo_m.z * out_xy.x + p.geo_m.w * out_xy.y + p.geo_off.y,
    );
    let uv = src / p.src_dims;

    let t = clamp(textureSampleLevel(trans, samp, uv, 0.0).r, 0.0, 1.0);
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
