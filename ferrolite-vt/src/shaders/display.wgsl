struct Transform {
    zoom: f32,
    _pad0: f32,
    pan: vec2<f32>,
    viewport: vec2<f32>,
    image: vec2<f32>,
};
@group(0) @binding(0) var img_tex: texture_2d<f32>;
@group(0) @binding(1) var img_samp: sampler;
@group(0) @binding(2) var<uniform> xf: Transform;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) screen_uv: vec2<f32>, // 0..1 across the viewport
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Full-screen triangle.
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var out: VsOut;
    let xy = p[vid];
    out.pos = vec4(xy, 0.0, 1.0);
    out.screen_uv = (xy * 0.5 + vec2(0.5, 0.5)) * vec2(1.0, -1.0) + vec2(0.0, 1.0);
    return out;
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Screen pixel -> image pixel: center the image, apply pan, divide by zoom.
    let screen_px = in.screen_uv * xf.viewport;
    let center = xf.image * 0.5 + xf.pan;
    let img_px = center + (screen_px - xf.viewport * 0.5) / xf.zoom;
    let uv = img_px / xf.image;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return vec4(0.05, 0.05, 0.05, 1.0);
    }
    let lin = textureSampleLevel(img_tex, img_samp, uv, 0.0).rgb;
    return vec4(linear_to_srgb(lin), 1.0);
}
