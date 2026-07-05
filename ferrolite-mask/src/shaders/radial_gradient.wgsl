// Radial-gradient (ellipse) mask. The pixel's normalized position is translated
// to the ellipse center, rotated into ellipse-local axes, and normalized by the
// per-axis radii to a scalar distance `d` (d<=1 inside the ellipse). The mask is
// 1 inside and smoothly falls to 0 across the feather band just outside the edge.
// `invert` (0/1) flips inside/outside. Analytic per pixel -> zero halo.
@group(0) @binding(0) var out_tex: texture_storage_2d<r32float, write>;
struct P {
    center: vec2<f32>,
    radius: vec2<f32>,
    rotation: f32,
    feather: f32,
    invert: f32,
    pad: f32,
};
@group(0) @binding(1) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(out_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + vec2<f32>(0.5, 0.5))
        / vec2<f32>(f32(dims.x), f32(dims.y));
    let d0 = uv - p.center;
    let c = cos(p.rotation);
    let s = sin(p.rotation);
    let local = vec2<f32>(c * d0.x + s * d0.y, -s * d0.x + c * d0.y);
    let rx = max(p.radius.x, 1e-6);
    let ry = max(p.radius.y, 1e-6);
    let dist = length(vec2<f32>(local.x / rx, local.y / ry)); // 1 at the edge
    // Feather band expressed as a fraction of the radius: [1, 1 + feather].
    let f = max(p.feather, 1e-6);
    var m = 1.0 - smoothstep(1.0, 1.0 + f, dist);
    if (p.invert > 0.5) { m = 1.0 - m; }
    textureStore(out_tex, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(m, 0.0, 0.0, 1.0));
}
