// Dehaze transmission pass 1/8: per-pixel normalized dark channel + luma guide.
// dc0(p) = min(rgb/A); guide(p) = luma709(rgb). Mirrors `dehaze::transmission_map`
// steps 1 (dark channel) and 4 (guide) exactly — no neighbourhood here, that is
// `dehaze_min_h`/`dehaze_min_v`.
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

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(src));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0).rgb;
    let a = p.atmos.rgb;
    let n = c / a;
    let dc0 = min(n.r, min(n.g, n.b));
    let guide = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    textureStore(dc0_out, xy, vec4<f32>(dc0, 0.0, 0.0, 0.0));
    textureStore(guide_out, xy, vec4<f32>(guide, 0.0, 0.0, 0.0));
}
