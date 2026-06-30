// Full-screen blit of a display-linear Rgba16Float texture to an sRGB-encoded
// Rgba8 target. Nearest sampling (1:1 readback for golden tests).

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var out: VsOut;
    let xy = p[vid];
    out.pos = vec4(xy, 0.0, 1.0);
    out.uv = (xy * 0.5 + vec2(0.5, 0.5)) * vec2(1.0, -1.0) + vec2(0.0, 1.0);
    return out;
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let lin = textureSampleLevel(src, samp, in.uv, 0.0).rgb;
    return vec4(linear_to_srgb(lin), 1.0);
}
