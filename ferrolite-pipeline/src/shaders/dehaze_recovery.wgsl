// Dehaze recovery + blend (QS-Task 3): two-input compute pass taking the original
// image I and the refined transmission map q, producing the recovered/blended image.
// Mirrors the pure CPU reference `dehaze_recover` exactly, but consumes q directly
// (while the CPU reference takes dark derived as (1-q)/DEHAZE_OMEGA).

@group(0) @binding(0) var img: texture_2d<f32>;
@group(0) @binding(1) var trans: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;

struct P {
    amount: f32,
    t0: f32,
    pad0: f32,
    pad1: f32,
    atmos: vec4<f32>,
};

@group(0) @binding(3) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(img));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) {
        return;
    }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(img, xy, 0);
    if (p.amount == 0.0) {
        textureStore(dst, xy, c);
        return;
    }
    let a = p.atmos.rgb;
    let t = clamp(textureLoad(trans, xy, 0).r, 0.0, 1.0);
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
