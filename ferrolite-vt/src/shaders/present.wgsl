// Fullscreen blit of one already-display-encoded color texture into the swapchain,
// with an optional alpha for the crossfade blend. Generic — no photo concepts.
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;
struct BlitParams { alpha: f32, _pad: vec3<f32> };
@group(0) @binding(2) var<uniform> params: BlitParams;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_blit(@builtin(vertex_index) vid: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var out: VsOut;
    let xy = p[vid];
    out.pos = vec4(xy, 0.0, 1.0);
    out.uv = (xy * 0.5 + vec2(0.5, 0.5)) * vec2(1.0, -1.0) + vec2(0.0, 1.0);
    return out;
}

@fragment
fn fs_blit(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(src_tex, src_samp, in.uv, 0.0).rgb;
    return vec4(c, params.alpha);
}
