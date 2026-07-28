// Dehaze transmission pass 1/8: per-pixel normalized dark channel + luma guide,
// DOWNSAMPLED from the full-res `src` to the (possibly smaller) working
// resolution (`dc0_out`/`guide_out`'s dims — see `transmission_working_dims`).
// The transmission map is a smooth, low-frequency signal, so a single bilinear
// tap per working pixel (rather than a full box-downsample) is acceptable —
// the guided-filter refinement smooths further downstream. At scale==1 this
// samples exactly the source texel (a linear tap at a texel center returns
// that texel), so small inputs are unaffected.
// dc0(p) = min(rgb/A); guide(p) = luma709(rgb). Mirrors `dehaze::transmission_map`
// steps 1 (dark channel) and 4 (guide) exactly (up to the downsample) — no
// neighbourhood here, that is `dehaze_min_h`/`dehaze_min_v`.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dc0_out: texture_storage_2d<r32float, write>;
@group(0) @binding(2) var guide_out: texture_storage_2d<r32float, write>;
struct P {
    radius: i32,
    pad0: i32,
    pad1: vec2<i32>,
    atmos: vec4<f32>,
    omega: f32,
    eps: f32,
    pad2: vec2<f32>,
};
@group(0) @binding(3) var<uniform> p: P;
@group(0) @binding(4) var samp: sampler;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(dc0_out));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let uv = (vec2<f32>(xy) + 0.5) / vec2<f32>(dims);
    let c = textureSampleLevel(src, samp, uv, 0.0).rgb;
    let a = p.atmos.rgb;
    let n = c / a;
    let dc0 = min(n.r, min(n.g, n.b));
    let guide = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    textureStore(dc0_out, xy, vec4<f32>(dc0, 0.0, 0.0, 0.0));
    textureStore(guide_out, xy, vec4<f32>(guide, 0.0, 0.0, 0.0));
}
