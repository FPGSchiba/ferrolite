// Generic display-referred histogram: bin the display value (working->display 3x3
// + sRGB OETF, matching display.wgsl) into 256 x {R,G,B,luma} atomic bins.
struct Params {
    m: mat3x3<f32>,
    dims: vec2<u32>,
};
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>, 1024u>;
@group(0) @binding(2) var<uniform> p: Params;

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

fn bin_index(v: f32) -> u32 {
    let x = clamp(v, 0.0, 1.0);
    return u32(clamp(x * 255.0 + 0.5, 0.0, 255.0));
}

@compute @workgroup_size(8, 8, 1)
fn bin(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.dims.x || gid.y >= p.dims.y) { return; }
    let c = textureLoad(src, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    // Clamp before the OETF: out-of-[0,1] (HDR overshoot) folds into bins 0/255.
    // display.wgsl does not pre-clamp — intentional divergence for a fixed-range histogram.
    let disp = linear_to_srgb(clamp(p.m * c.rgb, vec3(0.0), vec3(1.0)));
    let luma = dot(disp, vec3<f32>(0.2126, 0.7152, 0.0722));
    atomicAdd(&bins[0u * 256u + bin_index(disp.r)], 1u);
    atomicAdd(&bins[1u * 256u + bin_index(disp.g)], 1u);
    atomicAdd(&bins[2u * 256u + bin_index(disp.b)], 1u);
    atomicAdd(&bins[3u * 256u + bin_index(luma)], 1u);
}
